// SPDX-License-Identifier: GPL-3.0-or-later
// The Mac's media side of a session (docs/protocol.md, sections 8 and 9):
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
    /// The Mac's address on the control connection; only candidates on it are signaled.
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

    private let queue = DispatchQueue(label: "av-relay.media")
    private let log = Logger(subsystem: "io.github.salihmarangoz.avrelay", category: "media")
    private var settings: Settings
    private let factory: RTCPeerConnectionFactory
    private let microphone: MicrophoneDevice
    private let videoSource: RTCVideoSource
    private let camera: CameraCapture
    private var call: Call?

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

    /// Whether libwebrtc has the microphone recording (tests).
    var microphoneIsRecording: Bool { microphone.isRecording }

    /// The current call's WebRTC statistics (tests, diagnostics).
    func statistics() async -> RTCStatisticsReport? {
        let peer = queue.sync { call?.peerConnection }
        guard let peer else { return nil }
        return await withCheckedContinuation { continuation in peer.statistics { continuation.resume(returning: $0) } }
    }

    /// Switches the camera; a running capture moves over at once.
    public func setCamera(_ source: CameraSource) {
        queue.async { [self] in
            settings.camera = source
            camera.setSource(source, format: call?.format ?? Self.defaultFormat)
            call?.reportState()
        }
    }

    public func setMicrophone(_ source: MicrophoneSource) {
        queue.async { [self] in
            settings.microphone = source
            microphone.setSource(source)
            call?.reportState()
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

    func start(channel: SignalingChannel, hello: Hello) {
        queue.async { [self] in
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
        unowned let engine: MediaEngine
        let channel: SignalingChannel
        let format: VideoFormat
        private var outbox: AsyncStream<Message>.Continuation?
        private var peer: RTCPeerConnection?
        var peerConnection: RTCPeerConnection? { peer }
        private var videoSender: RTCRtpSender?
        private var audioSender: RTCRtpSender?
        private var remoteDescriptionSet = false
        private var pendingCandidates: [RTCIceCandidate] = []
        private var wanted: [Kind: Bool] = [.video: false, .audio: false]
        private var states: [Kind: DeviceState] = [.video: .off, .audio: .off]
        private var lastReported: Message?
        private var linger: [Kind: DispatchWorkItem] = [:]
        private var iceRecovery: DispatchWorkItem?
        private var closed = false
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
                return
            }
            self.peer = peer
            // A LAN carries far more than libwebrtc's cautious ~300 kbit/s start, which would also make
            // it start 1080p at a reduced resolution. GCC still backs off if the Wi-Fi can't keep up.
            let cap = MediaEngine.maxVideoBitrate(format)
            peer.setBweMinBitrateBps(NSNumber(value: 300_000), currentBitrateBps: NSNumber(value: cap / 2), maxBitrateBps: nil)

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
        }

        func close() {
            guard !closed else { return }
            closed = true
            linger.values.forEach { $0.cancel() }
            iceRecovery?.cancel()
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
                peer?.setRemoteDescription(RTCSessionDescription(type: .answer, sdp: sdp)) { [weak self] error in
                    self?.queue.async { self?.answerApplied(error) }
                }
            case let .ice(candidate, sdpMid, sdpMLineIndex):
                guard !candidate.isEmpty else { return }  // end of candidates
                let ice = RTCIceCandidate(sdp: candidate, sdpMLineIndex: Int32(sdpMLineIndex ?? 0), sdpMid: sdpMid)
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
            if let error {
                log.error("the client's answer: \(error.localizedDescription, privacy: .public)")
                Task { [channel] in
                    try? await channel.send(.error(.badMessage, "answer rejected"))
                }
                return
            }
            remoteDescriptionSet = true
            pendingCandidates.forEach(add)
            pendingCandidates = []
        }

        private func add(_ candidate: RTCIceCandidate) {
            peer?.add(candidate) { [weak self] error in
                if let error { self?.log.info("remote candidate: \(error.localizedDescription, privacy: .public)") }
            }
        }

        private func makeOffer(iceRestart: Bool) {
            guard let peer else { return }
            let constraints = RTCMediaConstraints(mandatoryConstraints: iceRestart ? ["IceRestart": "true"] : nil,
                                                  optionalConstraints: nil)
            peer.offer(for: constraints) { [weak self] sdp, error in
                guard let self else { return }
                queue.async {
                    guard let sdp, !self.closed else {
                        self.log.error("offer: \(error?.localizedDescription ?? "?", privacy: .public)")
                        return
                    }
                    peer.setLocalDescription(sdp) { error in
                        self.queue.async {
                            if let error {
                                self.log.error("local description: \(error.localizedDescription, privacy: .public)")
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
            case .audio:
                setActive(audioSender, false)
            }
            setState(kind, .off)
        }

        func deviceCapturing(_ kind: Kind) {
            guard !closed, wanted[kind] == true || states[kind]!.state == .starting else { return }
            if kind == .video {
                setState(.video, DeviceState(state: .on, width: format.width, height: format.height, fps: format.fps))
            } else {
                setState(.audio, DeviceState(state: .on))
            }
        }

        func deviceFailed(_ kind: Kind, _ error: DeviceState.ErrorCode) {
            guard !closed else { return }
            switch kind {
            case .video:
                setActive(videoSender, false)
                engine.camera.stop()
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
            video.device = engine.camera.deviceName
            audio.device = engine.microphone.deviceName
            let message = Message.mediaState(video: video, audio: audio)
            guard message != lastReported else { return }
            lastReported = message
            outbox?.yield(message)
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
