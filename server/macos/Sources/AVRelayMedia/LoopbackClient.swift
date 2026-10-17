// SPDX-License-Identifier: GPL-3.0-or-later
// A client in the same process: a second libwebrtc peer that answers the
// engine's offer, trickles ICE and counts decoded frames. The tests use it
// with test sources; the app's self-test (--self-test) with the real camera
// and microphone, which only an app bundle may open.

import AVRelayCore
import Foundation
import WebRTC

/// Counts decoded frames, remembers the last size, and counts bright ones
/// (the sync test's flashes) by the luma at the centre.
final class FrameCounter: NSObject, RTCVideoRenderer, @unchecked Sendable {
    private let lock = NSLock()
    private var _count = 0
    private var _bright = 0
    private var _size = CGSize.zero
    var count: Int { lock.withLock { _count } }
    var brightCount: Int { lock.withLock { _bright } }
    var size: CGSize { lock.withLock { _size } }
    func setSize(_ size: CGSize) {}
    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame else { return }
        let bright = Self.centreLuma(frame) > 128
        lock.withLock {
            _count += 1
            if bright { _bright += 1 }
            _size = CGSize(width: Int(frame.width), height: Int(frame.height))
        }
    }

    private static func centreLuma(_ frame: RTCVideoFrame) -> UInt8 {
        if let cv = frame.buffer as? RTCCVPixelBuffer, CVPixelBufferGetPlaneCount(cv.pixelBuffer) >= 1 {
            let buffer = cv.pixelBuffer
            CVPixelBufferLockBaseAddress(buffer, .readOnly)
            defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
            guard let base = CVPixelBufferGetBaseAddressOfPlane(buffer, 0) else { return 0 }
            let row = CVPixelBufferGetHeightOfPlane(buffer, 0) / 2, col = CVPixelBufferGetWidthOfPlane(buffer, 0) / 2
            return base.load(fromByteOffset: row * CVPixelBufferGetBytesPerRowOfPlane(buffer, 0) + col, as: UInt8.self)
        }
        let i420 = frame.buffer.toI420()
        return i420.dataY[Int(i420.height / 2) * Int(i420.strideY) + Int(i420.width / 2)]
    }
}

/// The receiver's side: a libwebrtc answerer, as a receiver app would be.
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
    private var _offers = 0
    private var _errors: [Message] = []
    var states: [Message] { lock.withLock { _states } }
    var offers: Int { lock.withLock { _offers } }
    var errors: [Message] { lock.withLock { _errors } }
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
        case .error:
            lock.withLock { _errors.append(message) }
        case .offer(let sdp):
            lock.withLock { _offers += 1 }
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

