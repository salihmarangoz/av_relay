// SPDX-License-Identifier: GPL-3.0-or-later
// The media engine end to end, in one process: a second libwebrtc peer
// plays the client (answers, trickles ICE, counts decoded frames), with
// the engine's test sources, so no camera, microphone or permission.

import AVRelayCore
@testable import AVRelayMedia
import Foundation
import Testing
import WebRTC

/// Counts decoded frames and remembers the last size.
final class FrameCounter: NSObject, RTCVideoRenderer, @unchecked Sendable {
    private let lock = NSLock()
    private var _count = 0
    private var _size = CGSize.zero
    var count: Int { lock.withLock { _count } }
    var size: CGSize { lock.withLock { _size } }
    func setSize(_ size: CGSize) {}
    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame else { return }
        lock.withLock {
            _count += 1
            _size = CGSize(width: Int(frame.width), height: Int(frame.height))
        }
    }
}

/// The client's side: a libwebrtc answerer, as the Linux app would be.
final class LoopbackClient: NSObject, SignalingChannel, RTCPeerConnectionDelegate, @unchecked Sendable {
    let engine: MediaEngine
    let factory: RTCPeerConnectionFactory
    let peer: RTCPeerConnection
    let frames = FrameCounter()
    /// Held: libwebrtc's ObjC track wrapper removes its renderers when it's deallocated.
    private var videoTrack: RTCVideoTrack?
    private let lock = NSLock()
    private var _states: [Message] = []
    private var _connected = false
    private var _terminated = false
    var states: [Message] { lock.withLock { _states } }
    var connected: Bool { lock.withLock { _connected } }
    var localAddress: String? { nil }

    init(engine: MediaEngine) {
        self.engine = engine
        // Its own silent audio device: nothing is played or recorded.
        factory = RTCPeerConnectionFactory(encoderFactory: RTCDefaultVideoEncoderFactory(),
                                           decoderFactory: RTCDefaultVideoDecoderFactory(),
                                           audioDevice: MicrophoneDevice(source: .testTone))
        let config = RTCConfiguration()
        config.sdpSemantics = .unifiedPlan
        config.bundlePolicy = .maxBundle
        config.rtcpMuxPolicy = .require
        peer = factory.peerConnection(with: config, constraints: RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil),
                                      delegate: nil)!
        super.init()
        peer.delegate = self
    }

    // The engine's signaling, as the client receives it.
    func send(_ message: Message) async throws {
        switch message {
        case .offer(let sdp):
            // AVRELAY_OFFER_FILE=path: keep a real offer, e.g. to check a client's answer offline.
            if let path = ProcessInfo.processInfo.environment["AVRELAY_OFFER_FILE"] {
                try? sdp.write(toFile: path, atomically: true, encoding: .utf8)
            }
            try await peer.setRemoteDescription(RTCSessionDescription(type: .offer, sdp: sdp))
            if videoTrack == nil, let track = peer.transceivers.first(where: { $0.mediaType == .video })?.receiver.track as? RTCVideoTrack {
                track.add(frames)
                videoTrack = track
            }
            let answer = try await peer.answer(for: RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil))
            try await peer.setLocalDescription(answer)
            engine.receive(.answer(sdp: answer.sdp), from: self)
        case let .ice(candidate, mid, index):
            guard !candidate.isEmpty else { return }
            try await peer.add(RTCIceCandidate(sdp: candidate, sdpMLineIndex: Int32(index ?? 0), sdpMid: mid))
        case .mediaState:
            lock.withLock { _states.append(message) }
        default:
            break
        }
    }

    func terminate() async {
        lock.withLock { _terminated = true }
    }

    func lastState() -> (video: DeviceState.State, audio: DeviceState.State)? {
        guard case let .mediaState(video, audio)? = states.last else { return nil }
        return (video.state, audio.state)
    }

    /// inbound-rtp packetsReceived for a kind, and the codec's MIME type.
    func inbound(_ kind: String) async -> (packets: Int, mimeType: String?) {
        let report = await withCheckedContinuation { continuation in
            peer.statistics { continuation.resume(returning: $0) }
        }
        var packets = 0
        var codecId: String?
        for stats in report.statistics.values where stats.type == "inbound-rtp" && stats.values["kind"] as? String == kind {
            packets += (stats.values["packetsReceived"] as? NSNumber)?.intValue ?? 0
            codecId = stats.values["codecId"] as? String
        }
        let mime = codecId.flatMap { report.statistics[$0]?.values["mimeType"] as? String }
        return (packets, mime)
    }

    func peerConnection(_ peerConnection: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {
        engine.receive(.ice(candidate: candidate.sdp, sdpMid: candidate.sdpMid, sdpMLineIndex: Int(candidate.sdpMLineIndex)), from: self)
    }
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCPeerConnectionState) {
        if newState == .connected { lock.withLock { _connected = true } }
    }
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange stateChanged: RTCSignalingState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    func peerConnectionShouldNegotiate(_ peerConnection: RTCPeerConnection) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceConnectionState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceGatheringState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didOpen dataChannel: RTCDataChannel) {}
}

func waitUntil(_ seconds: Double = 5, _ condition: () async -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(seconds)
    while Date() < deadline {
        if await condition() { return true }
        try? await Task.sleep(for: .milliseconds(20))
    }
    return false
}

@Suite(.serialized) struct LoopbackTests {
    @Test func onDemandVideoAndAudio() async throws {
        var settings = MediaEngine.Settings()
        settings.camera = .testPattern
        settings.microphone = .testTone
        settings.linger = 0.3
        let engine = MediaEngine(settings: settings)
        let client = LoopbackClient(engine: engine)
        defer {
            engine.end(channel: client)
            client.peer.close()
        }
        engine.start(channel: client, hello: Hello(name: "test", app: "test", video: VideoFormat(width: 1920, height: 1080, fps: 30)))
        #expect(await waitUntil { client.connected })
        #expect(client.lastState().map { $0.video == .off && $0.audio == .off } == true)

        // Both on: 1080p at the full frame rate (the level fix), H.264, and audio packets.
        engine.receive(.media(video: true, audio: true), from: client)
        #expect(await waitUntil { client.lastState().map { $0.video == .on && $0.audio == .on } == true })
        #expect(await waitUntil { client.frames.count > 10 })
        let start = client.frames.count
        let audioStart = await client.inbound("audio").packets
        try await Task.sleep(for: .seconds(2))
        let fps = Double(client.frames.count - start) / 2
        #expect(fps >= 25, "frame rate \(fps)")
        // The sender: hardware H.264 at the full size, held back by nothing.
        let report = await engine.statistics()
        let outbound = report?.statistics.values.first { $0.type == "outbound-rtp" && $0.values["kind"] as? String == "video" }
        #expect(outbound?.values["encoderImplementation"] as? String == "VideoToolbox")
        #expect(outbound?.values["qualityLimitationReason"] as? String == "none")
        #expect(client.frames.size == CGSize(width: 1920, height: 1080), "size \(client.frames.size)")
        #expect(await client.inbound("video").mimeType == "video/H264")
        #expect(await client.inbound("audio").packets - audioStart >= 50)  // 20 ms packets: ~100 in 2 s
        #expect(engine.microphoneIsRecording)

        // Both off: after the linger, the mic stops recording and video stops.
        engine.receive(.media(video: false, audio: false), from: client)
        #expect(await waitUntil { client.lastState().map { $0.video == .off && $0.audio == .off } == true })
        #expect(await waitUntil { !engine.microphoneIsRecording })
        try await Task.sleep(for: .milliseconds(300))
        let stopped = client.frames.count
        try await Task.sleep(for: .seconds(1))
        #expect(client.frames.count - stopped <= 2)

        // Video again: it decodes, so the restart began with a keyframe.
        engine.receive(.media(video: true, audio: false), from: client)
        #expect(await waitUntil { client.frames.count > stopped + 20 })
    }

    @Test func lingerKeepsAQuickReopenSeamless() async throws {
        var settings = MediaEngine.Settings()
        settings.camera = .testPattern
        settings.microphone = .testTone
        settings.linger = 1
        let engine = MediaEngine(settings: settings)
        let client = LoopbackClient(engine: engine)
        defer {
            engine.end(channel: client)
            client.peer.close()
        }
        engine.start(channel: client, hello: Hello(name: "test", app: "test", video: VideoFormat(width: 640, height: 480, fps: 30)))
        #expect(await waitUntil { client.connected })
        engine.receive(.media(video: true, audio: false), from: client)
        #expect(await waitUntil { client.lastState()?.video == .on })
        // Off and on again within the linger: the camera never stops.
        engine.receive(.media(video: false, audio: false), from: client)
        engine.receive(.media(video: true, audio: false), from: client)
        try await Task.sleep(for: .milliseconds(1500))
        let videoStates = client.states.compactMap { message -> DeviceState.State? in
            if case let .mediaState(video, _) = message { return video.state }
            return nil
        }
        #expect(!videoStates.dropFirst(videoStates.firstIndex(of: .on) ?? 0).contains(.off))
        #expect(client.frames.size == CGSize(width: 640, height: 480))
    }
}

@Suite struct MediaUnitTests {
    @Test func levelIsRaisedTo5_1() {
        func raised(_ id: String) -> String? {
            H264EncoderFactory.raisingLevel(of: RTCVideoCodecInfo(name: "H264", parameters: ["profile-level-id": id]))
                .parameters["profile-level-id"]
        }
        #expect(raised("42e01f") == "42e033")
        #expect(raised("640c1f") == "640c33")
        #expect(raised("640c34") == "640c34")
    }

    @Test func onlyHostUDPCandidatesOnTheControlAddress() {
        let host = "candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host generation 0"
        #expect(MediaEngine.Call.shouldSignal(host, localAddress: "192.0.2.10"))
        #expect(!MediaEngine.Call.shouldSignal(host, localAddress: "192.0.2.11"))
        #expect(MediaEngine.Call.shouldSignal(host, localAddress: nil))
        #expect(!MediaEngine.Call.shouldSignal("candidate:2 1 tcp 1518280447 192.0.2.10 9 typ host tcptype active", localAddress: nil))
        #expect(!MediaEngine.Call.shouldSignal("candidate:3 1 udp 2122260223 abc.local 50000 typ host", localAddress: nil))
        #expect(!MediaEngine.Call.shouldSignal("candidate:4 1 udp 1686052607 198.51.100.1 50000 typ srflx raddr 0.0.0.0 rport 0", localAddress: nil))
    }

    @Test func bitrateCaps() {
        #expect(MediaEngine.maxVideoBitrate(VideoFormat(width: 1280, height: 720, fps: 30)) == 4_000_000)
        #expect(abs(MediaEngine.maxVideoBitrate(VideoFormat(width: 1920, height: 1080, fps: 30)) - 8_000_000) < 10)
        #expect(MediaEngine.maxVideoBitrate(VideoFormat(width: 160, height: 120, fps: 5)) >= 300_000)
    }
}

/// The real camera and microphone. Opt in: AVRELAY_REAL_DEVICES=1 swift test --filter RealDevices
/// (macOS asks for camera and microphone access the first time; the camera light turns on).
@Suite(.serialized, .enabled(if: ProcessInfo.processInfo.environment["AVRELAY_REAL_DEVICES"] == "1"))
struct RealDevices {
    @Test func cameraAndMicrophone() async throws {
        var settings = MediaEngine.Settings()
        settings.linger = 0.3
        let engine = MediaEngine(settings: settings)
        let client = LoopbackClient(engine: engine)
        defer {
            engine.end(channel: client)
            client.peer.close()
        }
        engine.start(channel: client, hello: Hello(name: "test", app: "test", video: VideoFormat(width: 1280, height: 720, fps: 30)))
        #expect(await waitUntil { client.connected })
        engine.receive(.media(video: true, audio: true), from: client)
        // Generous: the first run waits for the permission prompts.
        #expect(await waitUntil(60) { client.lastState().map { $0.video == .on && $0.audio == .on } == true })
        print("media-state:", client.states.last.map { "\($0)" } ?? "none")
        #expect(await waitUntil(10) { client.frames.count > 30 })
        let start = client.frames.count
        let audioStart = await client.inbound("audio").packets
        try await Task.sleep(for: .seconds(3))
        let fps = Double(client.frames.count - start) / 3
        print("camera: \(client.frames.size) at \(fps) fps; audio packets in 3 s: \(await client.inbound("audio").packets - audioStart)")
        #expect(fps >= 20)
        #expect(client.frames.size == CGSize(width: 1280, height: 720))
        #expect(engine.microphoneIsRecording)

        engine.receive(.media(video: false, audio: false), from: client)
        #expect(await waitUntil { client.lastState().map { $0.video == .off && $0.audio == .off } == true })
        #expect(await waitUntil { !engine.microphoneIsRecording })
    }
}
