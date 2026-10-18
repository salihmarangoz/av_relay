// SPDX-License-Identifier: GPL-3.0-or-later
// The sender's media side of a session (docs/protocol.md, sections 8 and 9):
// a libwebrtc peer connection that offers H.264 video and Opus audio,
// signaled over the control channel, with the camera and microphone
// running only while the client needs them.
//
// Turning a kind on or off doesn't renegotiate: both m-sections are
// negotiated once, and the sender's encoding is paused or resumed.
// Pausing the audio sender makes libwebrtc stop recording (MicrophoneDevice
// closes the mic); resuming the video sender restarts the encoder, so the
// first frame is a keyframe.

import AVFoundation
import AVRelayCore
import CWebRTCSupport
import WebRTC
import os

/// Where a session's signaling goes: the control channel, or a test double.
public protocol SignalingChannel: AnyObject, Sendable {
    func send(_ message: Message) async throws
    /// Our address on the control connection; only candidates on it are signaled.
    var localAddress: String? { get }
    /// Ends the session (the media can't recover).
    func terminate() async
}

extension ServerSession: SignalingChannel {
    public func terminate() async {
        close(sending: nil)
    }
}

public final class MediaEngine: NSObject, SessionHandler, @unchecked Sendable {
    public struct Settings: Sendable {
        public var camera: CameraSource = .device(id: nil)
        public var microphone: MicrophoneSource = .device(id: nil)
        /// How long a device keeps running after the client stops using it.
        public var linger: TimeInterval = 3
        /// After an ICE failure: how long one ICE restart gets before the session ends.
        public var iceRecoveryTimeout: TimeInterval = 30
        public init() {}
    }

    static let defaultFormat = VideoFormat(width: 1280, height: 720, fps: 30)

    /// What the menu bar shows: the camera's and microphone's states.
    public struct Status: Sendable, Equatable {
        public var video: DeviceState
        public var audio: DeviceState
        public static let idle = Status(video: .off, audio: .off)
    }

    /// Called (on a background queue) whenever the status changes.
    public var onStatus: (@Sendable (Status) -> Void)?

    private let queue = DispatchQueue(label: "av-relay.media")
    private let log = Logger(subsystem: "io.github.salihmarangoz.avrelay", category: "media")
    private var settings: Settings
    private let factory: RTCPeerConnectionFactory
    private let microphone: MicrophoneDevice
    private let videoSource: RTCVideoSource
    private let camera: CameraCapture
    private var call: Call?
    /// Between stopAll() and resume(): the computer sleeps, and nothing may open
    /// the camera or microphone, not even events still queued for a session
    /// that has just ended.
    private var suspended = false
    /// Sends libwebrtc's own log to the system log, from now on: warnings and
    /// errors kept, the rest at info level (diagnostics: `log stream --info`).
    /// For the app; tests leave it off, as libwebrtc then also writes to stderr.
    public static func forwardWebRTCLog() { _ = webrtcLog }

    private static let webrtcLog: RTCCallbackLogger = {
        let logger = RTCCallbackLogger()
        let log = Logger(subsystem: "io.github.salihmarangoz.avrelay", category: "webrtc")
        logger.severity = .info
        logger.start { message, severity in
            let line = message.trimmingCharacters(in: .whitespacesAndNewlines)
            if severity == .info {
                log.info("\(line, privacy: .public)")
            } else {
                log.notice("\(line, privacy: .public)")
            }
        }
        return logger
    }()

    public init(settings: Settings = Settings()) {
        self.settings = settings
        RTCInitializeSSL()
        microphone = MicrophoneDevice(source: settings.microphone)
        factory = RTCPeerConnectionFactory(encoderFactory: H264EncoderFactory(),
                                           decoderFactory: RTCDefaultVideoDecoderFactory(), audioDevice: microphone)
        videoSource = factory.videoSource()
        camera = CameraCapture(videoSource: videoSource, source: settings.camera)
        super.init()
        camera.onCapturing = { [weak self] in self?.queue.async { self?.call?.deviceCapturing(.video) } }
        camera.onFailure = { [weak self] error in self?.queue.async { self?.call?.deviceFailed(.video, error) } }
        microphone.onCapturing = { [weak self] in self?.queue.async { self?.call?.deviceCapturing(.audio) } }
        microphone.onFailure = { [weak self] error in self?.queue.async { self?.call?.deviceFailed(.audio, error) } }
    }

    /// Ends the call and returns once the camera and microphone are closed:
    /// the computer is about to sleep, and must not resume capture on wake.
    /// Nothing starts again until resume().
    public func stopAll() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            queue.async { [self] in
                suspended = true
                call?.close()
                call = nil
                camera.stopNow()
                microphone.stopNow()
                onStatus?(.idle)
                continuation.resume()
            }
        }
    }

    /// Frames the camera has delivered, and its mode (diagnostics).
    var cameraFramesCaptured: Int { camera.framesCaptured }
    var cameraMode: String? { camera.activeMode }

    /// Whether libwebrtc has the microphone recording (tests).
    var microphoneIsRecording: Bool { microphone.isRecording }

    /// A few lines on how the current call is doing (diagnostics).
    public func statisticsSummary() async -> String {
        guard let report = await statistics() else { return "no call" }
        return Self.summarize(report)
    }

    static func summarize(_ report: RTCStatisticsReport) -> String {
        var lines: [String] = []
        func values(_ type: String, kind: String? = nil) -> [[String: NSObject]] {
            report.statistics.values.filter { $0.type == type && (kind == nil || $0.values["kind"] as? String == kind) }.map(\.values)
        }
        func show(_ v: [String: NSObject], _ keys: [String]) -> String {
            keys.compactMap { key in v[key].map { "\(key)=\($0)" } }.joined(separator: " ")
        }
        for v in values("outbound-rtp", kind: "video") {
            lines.append("video out: " + show(v, ["frameWidth", "frameHeight", "framesPerSecond", "framesEncoded", "keyFramesEncoded",
                                                 "targetBitrate", "qualityLimitationReason", "qualityLimitationResolutionChanges",
                                                 "hugeFramesSent", "nackCount", "pliCount", "firCount",
                                                 "retransmittedPacketsSent", "encoderImplementation"]))
        }
        for v in values("outbound-rtp", kind: "audio") {
            lines.append("audio out: " + show(v, ["packetsSent", "targetBitrate", "nackCount", "retransmittedPacketsSent"]))
        }
        for v in values("remote-inbound-rtp") {
            lines.append("\(v["kind"] ?? "?" as NSString) as received: " + show(v, ["packetsLost", "fractionLost", "jitter", "roundTripTime"]))
        }
        for v in values("candidate-pair") where (v["nominated"] as? NSNumber)?.boolValue == true {
            lines.append("path: " + show(v, ["availableOutgoingBitrate", "currentRoundTripTime", "bytesSent", "bytesReceived"]))
        }
        return lines.isEmpty ? "no media stats yet" : lines.joined(separator: "\n")
    }

    /// The current call's WebRTC statistics (tests, diagnostics).
    func statistics() async -> RTCStatisticsReport? {
        let peer = queue.sync { call?.peerConnection }
        guard let peer else { return nil }
        return await withCheckedContinuation { continuation in peer.statistics { continuation.resume(returning: $0) } }
    }

    /// The video sender's outbound-rtp stats (tests).
    func videoOutbound() async -> [String: NSObject]? {
        await statistics()?.statistics.values.first { $0.type == "outbound-rtp" && $0.values["kind"] as? String == "video" }?.values
    }

    /// The bandwidth estimate on the call's path, in bit/s (tests). Only the transport's
    /// selected candidate pair has it; more than one pair may be nominated.
    func availableOutgoingBitrate() async -> Int? {
        guard let stats = await statistics()?.statistics else { return nil }
        let selected = stats.values.lazy.compactMap { $0.type == "transport" ? $0.values["selectedCandidatePairId"] as? String : nil }.first
        return (selected.flatMap { stats[$0] }?.values["availableOutgoingBitrate"] as? NSNumber)?.intValue
    }

    /// Switches the camera; a running capture moves over at once.
    public func setCamera(_ source: CameraSource) {
        queue.async { [self] in
            guard settings.camera != source else { return }
            settings.camera = source
            camera.setSource(source, format: call?.format ?? Self.defaultFormat)
            call?.deviceChanged(.video)
        }
    }

    public func setMicrophone(_ source: MicrophoneSource) {
        queue.async { [self] in
            guard settings.microphone != source else { return }
            settings.microphone = source
            microphone.setSource(source)
            call?.deviceChanged(.audio)
        }
    }

    // MARK: SessionHandler

    public func sessionDidStart(_ session: ServerSession, hello: Hello) async {
        start(channel: session, hello: hello)
    }

    public func session(_ session: ServerSession, didReceive message: Message) async {
        receive(message, from: session)
    }

    public func sessionDidEnd(_ session: ServerSession) async {
        end(channel: session)
    }

    // MARK: Entry points (any SignalingChannel)

    /// The computer is awake: sessions may use the camera and microphone again.
    public func resume() {
        queue.async { [self] in suspended = false }
    }

    func start(channel: SignalingChannel, hello: Hello) {
        queue.async { [self] in
            guard !suspended else {
                log.info("not starting a call: going to sleep")
                return
            }
            call?.close()
            call = Call(engine: self, channel: channel, format: Self.validated(hello.video))
            call?.start()
        }
    }

    func receive(_ message: Message, from channel: SignalingChannel) {
        queue.async { [self] in
            guard let call, call.channel === channel else { return }
            call.handle(message)
        }
    }

    func end(channel: SignalingChannel) {
        queue.async { [self] in
            guard let call, call.channel === channel else { return }
            call.close()
            self.call = nil
            onStatus?(.idle)
        }
    }

    /// The client's format, within what makes sense; 720p30 if absent.
    static func validated(_ format: VideoFormat?) -> VideoFormat {
        guard let format, (160...3840).contains(format.width), (120...2160).contains(format.height),
              (1...60).contains(format.fps) else { return defaultFormat }
        return format
    }

    /// Video bitrate cap: 4 Mbit/s at 720p30, 8 at 1080p30, linear in pixel rate between and beyond.
    static func maxVideoBitrate(_ format: VideoFormat) -> Int {
        let pixelRate = Double(format.width * format.height * format.fps)
        let bps = 800_000 + pixelRate * (4_000_000 / 34_560_000)
        return Int(min(max(bps, 300_000), 12_000_000))
    }

    // MARK: One session's call (all on `queue`)

    enum Kind { case video, audio }

    final class Call: NSObject, RTCPeerConnectionDelegate {
        // Strong: the engine drops its call when the session ends.
        let engine: MediaEngine
        let channel: SignalingChannel
        let format: VideoFormat
        private var outbox: AsyncStream<Message>.Continuation?
        private var peer: RTCPeerConnection?
        var peerConnection: RTCPeerConnection? { peer }
        private var videoSender: RTCRtpSender?
        private var audioSender: RTCRtpSender?
        private var remoteDescriptionSet = false
        /// An offer is out and its answer not yet applied: one at a time.
        private var offering = false
        /// An ICE restart asked for while an offer was out; it follows the answer.
        private var restartPending = false
        private var pendingCandidates: [RTCIceCandidate] = []
        private var wanted: [Kind: Bool] = [.video: false, .audio: false]
        private var states: [Kind: DeviceState] = [.video: .off, .audio: .off]
        private var lastReported: Message?
        private var linger: [Kind: DispatchWorkItem] = [:]
        private var iceRecovery: DispatchWorkItem?
        /// The bandwidth estimate's floor is at the seed while video starts (holdBandwidthFloor).
        private var floorHeld = false
        private var floorRelease: DispatchWorkItem?
        private var closed = false
        private var diagnostics: DispatchSourceTimer?
        private var queue: DispatchQueue { engine.queue }
        private var log: Logger { engine.log }

        init(engine: MediaEngine, channel: SignalingChannel, format: VideoFormat) {
            self.engine = engine
            self.channel = channel
            self.format = format
        }

        func start() {
            // Signaling goes out in order (the offer before its candidates).
            let (stream, outbox) = AsyncStream.makeStream(of: Message.self)
            self.outbox = outbox
            Task { [channel] in
                for await message in stream { try? await channel.send(message) }
            }

            let config = RTCConfiguration()
            config.iceServers = []
            config.sdpSemantics = .unifiedPlan
            config.bundlePolicy = .maxBundle
            config.rtcpMuxPolicy = .require
            config.tcpCandidatePolicy = .disabled
            config.continualGatheringPolicy = .gatherOnce
            config.keyType = .ECDSA
            let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
            guard let peer = engine.factory.peerConnection(with: config, constraints: constraints, delegate: self) else {
                log.error("couldn't create the peer connection")
                Task { [channel] in await channel.terminate() }
                return
            }
            self.peer = peer
            seedBandwidthEstimate()

            let videoInit = RTCRtpTransceiverInit()
            videoInit.direction = .sendOnly
            videoInit.streamIds = ["av-relay"]
            let videoEncoding = RTCRtpEncodingParameters()
            videoEncoding.isActive = false
            videoEncoding.maxBitrateBps = NSNumber(value: MediaEngine.maxVideoBitrate(format))
            videoEncoding.minBitrateBps = NSNumber(value: 300_000)
            videoEncoding.maxFramerate = NSNumber(value: format.fps)
            videoInit.sendEncodings = [videoEncoding]
            let videoTrack = engine.factory.videoTrack(with: engine.videoSource, trackId: "camera")
            let video = peer.addTransceiver(with: videoTrack, init: videoInit)
            let capabilities = engine.factory.rtpSenderCapabilities(forKind: kRTCMediaStreamTrackKindVideo)
            let h264 = capabilities.codecs.filter { $0.name == "H264" && $0.parameters["packetization-mode"] == "1" }
            let rtx = capabilities.codecs.filter { $0.name == "rtx" }
            if let video, let error = AVRSetCodecPreferences(video, h264 + rtx) {
                log.error("H.264 codec preference: \(error.localizedDescription, privacy: .public)")
            }
            videoSender = video?.sender
            if let parameters = videoSender?.parameters {
                parameters.degradationPreference = NSNumber(value: RTCDegradationPreference.balanced.rawValue)
                videoSender?.parameters = parameters
            }

            let audioInit = RTCRtpTransceiverInit()
            audioInit.direction = .sendOnly
            audioInit.streamIds = ["av-relay"]
            let audioEncoding = RTCRtpEncodingParameters()
            audioEncoding.isActive = false
            audioInit.sendEncodings = [audioEncoding]
            // The client's call app does its own echo cancellation, gain and noise suppression.
            let processingOff = RTCMediaConstraints(mandatoryConstraints: [
                "googEchoCancellation": "false", "googAutoGainControl": "false",
                "googNoiseSuppression": "false", "googHighpassFilter": "false",
            ], optionalConstraints: nil)
            let audioTrack = engine.factory.audioTrack(with: engine.factory.audioSource(with: processingOff), trackId: "microphone")
            let audio = peer.addTransceiver(with: audioTrack, init: audioInit)
            let opus = engine.factory.rtpSenderCapabilities(forKind: kRTCMediaStreamTrackKindAudio).codecs.filter { $0.name == "opus" }
            if let audio, let error = AVRSetCodecPreferences(audio, opus) {
                log.error("Opus codec preference: \(error.localizedDescription, privacy: .public)")
            }
            audioSender = audio?.sender

            makeOffer(iceRestart: false)
            reportState()
            startDiagnostics()
        }

        /// Where the bandwidth estimate starts: half the format's cap. A LAN carries far more than
        /// libwebrtc's cautious ~300 kbit/s start, which would also make it start 1080p at a reduced
        /// resolution. GCC still backs off if the Wi-Fi can't keep up.
        private var seedBitrate: Int { MediaEngine.maxVideoBitrate(format) / 2 }

        /// At the call's start.
        private func seedBandwidthEstimate() {
            peer?.setBweMinBitrateBps(NSNumber(value: 300_000), currentBitrateBps: NSNumber(value: seedBitrate), maxBitrateBps: nil)
        }

        /// Whenever video starts: the estimate starts at the seed and can't fall below it until the
        /// camera has run for a moment (releaseBandwidthFloor). While only audio flows, the estimate
        /// sinks to 300 kbit/s; a new start alone doesn't hold when no media brings feedback, as
        /// libwebrtc puts the old estimate back within milliseconds. The encoder then drops its first
        /// frames as too large for 300 kbit/s, and video starts at 320x180 and 10 fps and takes ~25 s
        /// to climb back.
        private func holdBandwidthFloor() {
            floorRelease?.cancel()
            floorRelease = nil
            floorHeld = true
            peer?.setBweMinBitrateBps(NSNumber(value: seedBitrate), currentBitrateBps: NSNumber(value: seedBitrate), maxBitrateBps: nil)
        }

        /// Lowers the floor to 300 kbit/s again, after `delay`: the first frames are out and
        /// feedback flows, so the estimate is a real one from here on.
        private func releaseBandwidthFloor(after delay: TimeInterval = 0) {
            guard floorHeld else { return }
            floorRelease?.cancel()
            let item = DispatchWorkItem { [weak self] in
                guard let self, !self.closed, self.floorHeld else { return }
                self.floorHeld = false
                self.floorRelease = nil
                self.peer?.setBweMinBitrateBps(NSNumber(value: 300_000), currentBitrateBps: nil, maxBitrateBps: nil)
            }
            floorRelease = item
            queue.asyncAfter(deadline: .now() + delay, execute: item)
        }

        /// Every 5 s while the camera or microphone runs: one log line with the
        /// call's WebRTC stats and the devices' own cadence, to tell a stutter
        /// at the source from one on the network. Read it with
        /// log show --predicate 'subsystem == "io.github.salihmarangoz.avrelay"' --info --last 10m
        private func startDiagnostics() {
            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now() + 5, repeating: 5)
            timer.setEventHandler { [weak self] in
                guard let self, !closed, let peer else { return }
                let mic = engine.microphone.takeCadence(), camera = engine.camera.takeCadence()
                let active = [states[.video]!.state, states[.audio]!.state].contains { $0 != .off }
                guard active else { return }
                peer.statistics { [log] report in
                    let summary = MediaEngine.summarize(report).replacingOccurrences(of: "\n", with: " | ")
                    log.notice("stats: \(summary, privacy: .public) | mic \(mic.description, privacy: .public) | camera \(camera.description, privacy: .public)")
                }
            }
            diagnostics = timer
            timer.resume()
        }

        func close() {
            guard !closed else { return }
            closed = true
            diagnostics?.cancel()
            diagnostics = nil
            linger.values.forEach { $0.cancel() }
            iceRecovery?.cancel()
            floorRelease?.cancel()
            // At once, no linger: the session is over.
            engine.camera.stop()
            peer?.close()
            peer = nil
            outbox?.finish()
        }

        func handle(_ message: Message) {
            guard !closed else { return }
            switch message {
            case .answer(let sdp):
                // Only the outstanding offer is answered; anything else is stale.
                guard offering else {
                    log.info("ignoring an answer to no outstanding offer")
                    return
                }
                peer?.setRemoteDescription(RTCSessionDescription(type: .answer, sdp: sdp)) { [weak self] error in
                    self?.queue.async { self?.answerApplied(error) }
                }
            case let .ice(candidate, sdpMid, sdpMLineIndex):
                guard !candidate.isEmpty else { return }  // end of candidates
                guard let index = Int32(exactly: sdpMLineIndex ?? 0) else { return }
                let ice = RTCIceCandidate(sdp: candidate, sdpMLineIndex: index, sdpMid: sdpMid)
                if remoteDescriptionSet {
                    add(ice)
                } else {
                    pendingCandidates.append(ice)
                }
            case let .media(video, audio):
                setWanted(.video, video)
                setWanted(.audio, audio)
            case .iceRestart:
                makeOffer(iceRestart: true)
            default:
                break
            }
        }

        private func answerApplied(_ error: Error?) {
            offering = false
            if let error {
                log.error("the client's answer: \(error.localizedDescription, privacy: .public)")
                Task { [channel] in
                    try? await channel.send(.error(.badMessage, "answer rejected"))
                }
            } else {
                remoteDescriptionSet = true
                pendingCandidates.forEach(add)
                pendingCandidates = []
            }
            if restartPending {
                restartPending = false
                makeOffer(iceRestart: true)
            }
        }

        private func add(_ candidate: RTCIceCandidate) {
            peer?.add(candidate) { [weak self] error in
                if let error { self?.log.info("remote candidate: \(error.localizedDescription, privacy: .public)") }
            }
        }

        private func makeOffer(iceRestart: Bool) {
            guard let peer, !closed else { return }
            guard !offering else {
                if iceRestart { restartPending = true }
                return
            }
            offering = true
            let constraints = RTCMediaConstraints(mandatoryConstraints: iceRestart ? ["IceRestart": "true"] : nil,
                                                  optionalConstraints: nil)
            peer.offer(for: constraints) { [weak self] sdp, error in
                guard let self else { return }
                queue.async {
                    guard let sdp, !self.closed else {
                        self.log.error("offer: \(error?.localizedDescription ?? "?", privacy: .public)")
                        self.offering = false
                        return
                    }
                    peer.setLocalDescription(sdp) { error in
                        self.queue.async {
                            if let error {
                                self.log.error("local description: \(error.localizedDescription, privacy: .public)")
                                self.offering = false
                                return
                            }
                            self.outbox?.yield(.offer(sdp: sdp.sdp))
                        }
                    }
                }
            }
        }

        // MARK: On demand (section 9)

        private func setWanted(_ kind: Kind, _ want: Bool) {
            guard wanted[kind] != want else { return }
            wanted[kind] = want
            if want {
                linger[kind]?.cancel()
                linger[kind] = nil
                switch states[kind]!.state {
                case .on, .starting:
                    break
                case .off, .error:
                    begin(kind)
                }
            } else if states[kind]!.state == .error {
                stop(kind)
            } else {
                let item = DispatchWorkItem { [weak self] in
                    guard let self, self.wanted[kind] == false else { return }
                    self.stop(kind)
                }
                linger[kind]?.cancel()
                linger[kind] = item
                queue.asyncAfter(deadline: .now() + engine.settings.linger, execute: item)
            }
        }

        private func begin(_ kind: Kind) {
            guard !engine.suspended else { return }
            setState(kind, DeviceState(state: .starting))
            let needsPermission: Bool
            switch kind {
            case .video: if case .device = engine.settings.camera { needsPermission = true } else { needsPermission = false }
            case .audio: if case .device = engine.settings.microphone { needsPermission = true } else { needsPermission = false }
            }
            let mediaType: AVMediaType = kind == .video ? .video : .audio
            let go = { [weak self] (granted: Bool) in
                guard let self, !self.closed, self.wanted[kind] == true else { return }
                guard granted else { return self.deviceFailed(kind, .permissionDenied) }
                switch kind {
                case .video:
                    self.holdBandwidthFloor()
                    self.engine.camera.start(format: self.format)
                    self.setActive(self.videoSender, true)
                case .audio:
                    // libwebrtc starts recording once the sender is active and negotiated.
                    self.setActive(self.audioSender, true)
                }
            }
            guard needsPermission else { return go(true) }
            switch AVCaptureDevice.authorizationStatus(for: mediaType) {
            case .authorized:
                go(true)
            case .notDetermined:
                AVCaptureDevice.requestAccess(for: mediaType) { [weak self] granted in
                    self?.queue.async { go(granted) }
                }
            default:
                go(false)
            }
        }

        private func stop(_ kind: Kind) {
            switch kind {
            case .video:
                setActive(videoSender, false)
                engine.camera.stop()
                releaseBandwidthFloor()
            case .audio:
                setActive(audioSender, false)
            }
            setState(kind, .off)
        }

        func deviceCapturing(_ kind: Kind) {
            guard !closed, wanted[kind] == true || states[kind]!.state == .starting else { return }
            if kind == .video {
                releaseBandwidthFloor(after: 2)
                setState(.video, DeviceState(state: .on, width: format.width, height: format.height, fps: format.fps))
            } else {
                setState(.audio, DeviceState(state: .on))
            }
        }

        /// The user picked another camera or microphone (protocol section 9):
        /// one in use restarts on it, one that failed gets another try.
        func deviceChanged(_ kind: Kind) {
            guard !closed else { return }
            switch states[kind]!.state {
            case .on:
                setState(kind, DeviceState(state: .starting))  // on again at the new device's first frame/buffer
            case .error where wanted[kind] == true:
                begin(kind)
            default:
                reportState()
            }
        }

        func deviceFailed(_ kind: Kind, _ error: DeviceState.ErrorCode) {
            guard !closed else { return }
            switch kind {
            case .video:
                setActive(videoSender, false)
                engine.camera.stop()
                releaseBandwidthFloor()
            case .audio:
                setActive(audioSender, false)
            }
            setState(kind, DeviceState(state: .error, error: error))
        }

        private func setActive(_ sender: RTCRtpSender?, _ active: Bool) {
            guard let sender else { return }
            let parameters = sender.parameters
            guard let encoding = parameters.encodings.first, encoding.isActive != active else { return }
            encoding.isActive = active
            sender.parameters = parameters
        }

        private func setState(_ kind: Kind, _ state: DeviceState) {
            states[kind] = state
            reportState()
        }

        /// Sends media-state if it changed, with the device names filled in.
        func reportState() {
            guard !closed else { return }
            var video = states[.video]!, audio = states[.audio]!
            video.device = CameraCapture.deviceName(for: engine.settings.camera)
            audio.device = MicrophoneDevice.deviceName(for: engine.settings.microphone)
            let message = Message.mediaState(video: video, audio: audio)
            guard message != lastReported else { return }
            lastReported = message
            outbox?.yield(message)
            engine.onStatus?(Status(video: video, audio: audio))
        }

        // MARK: RTCPeerConnectionDelegate (libwebrtc's signaling thread)

        func peerConnection(_ peerConnection: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {
            queue.async { [self] in
                guard !closed, Self.shouldSignal(candidate.sdp, localAddress: channel.localAddress) else { return }
                outbox?.yield(.ice(candidate: candidate.sdp, sdpMid: candidate.sdpMid, sdpMLineIndex: Int(candidate.sdpMLineIndex)))
            }
        }

        func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceGatheringState) {
            guard newState == .complete else { return }
            queue.async { [self] in
                guard !closed else { return }
                outbox?.yield(.ice(candidate: "", sdpMid: "0", sdpMLineIndex: 0))
            }
        }

        func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCPeerConnectionState) {
            queue.async { [self] in
                guard !closed else { return }
                log.info("peer connection \(String(describing: newState.rawValue), privacy: .public)")
                switch newState {
                case .connected:
                    iceRecovery?.cancel()
                    iceRecovery = nil
                case .failed where iceRecovery == nil:
                    // One ICE restart; if media isn't back in time, end the session so the client reconnects.
                    makeOffer(iceRestart: true)
                    let item = DispatchWorkItem { [weak self] in
                        guard let self, !self.closed else { return }
                        self.log.error("media didn't recover; ending the session")
                        Task { [channel = self.channel] in await channel.terminate() }
                    }
                    iceRecovery = item
                    queue.asyncAfter(deadline: .now() + engine.settings.iceRecoveryTimeout, execute: item)
                default:
                    break
                }
            }
        }

        func peerConnection(_ peerConnection: RTCPeerConnection, didChange stateChanged: RTCSignalingState) {}
        func peerConnection(_ peerConnection: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
        func peerConnection(_ peerConnection: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
        func peerConnectionShouldNegotiate(_ peerConnection: RTCPeerConnection) {}
        func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceConnectionState) {}
        func peerConnection(_ peerConnection: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
        func peerConnection(_ peerConnection: RTCPeerConnection, didOpen dataChannel: RTCDataChannel) {}

        /// Host UDP candidates on the control connection's address only (protocol section 8.4).
        static func shouldSignal(_ candidate: String, localAddress: String?) -> Bool {
            // candidate:<foundation> <component> <transport> <priority> <address> <port> typ <type> ...
            let fields = candidate.split(separator: " ")
            guard fields.count >= 8, fields[2].lowercased() == "udp", fields[7] == "host" else { return false }
            let address = String(fields[4])
            if address.hasSuffix(".local") || address.lowercased().hasPrefix("fe80:") { return false }
            guard let localAddress else { return true }
            return address == localAddress || address == localAddress.replacingOccurrences(of: "::ffff:", with: "")
        }
    }
}
