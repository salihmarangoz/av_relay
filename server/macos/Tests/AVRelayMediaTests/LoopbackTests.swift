// SPDX-License-Identifier: GPL-3.0-or-later
// The media engine end to end, in one process: a second libwebrtc peer
// plays the client (answers, trickles ICE, counts decoded frames), with
// the engine's test sources, so no camera, microphone or permission.

import AVRelayCore
@testable import AVRelayMedia
import Foundation
import Testing
import WebRTC

enum Hardware {
    /// True on a virtual Mac, such as a CI runner.
    static let isVirtualMachine: Bool = {
        var present: Int32 = 0
        var size = MemoryLayout<Int32>.size
        return sysctlbyname("kern.hv_vmm_present", &present, &size, nil, 0) == 0 && present == 1
    }()
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
        #expect(await waitUntil { await client.inbound("audio").packets > 0 })
        let start = client.frames.count
        let audioStart = await client.inbound("audio").packets
        try await Task.sleep(for: .seconds(2))
        let fps = Double(client.frames.count - start) / 2
        let audioReceived = await client.inbound("audio").packets - audioStart
        #expect(await client.inbound("video").mimeType == "video/H264")
        let report = await engine.statistics()
        let outbound = report?.statistics.values.first { $0.type == "outbound-rtp" && $0.values["kind"] as? String == "video" }
        #expect(outbound?.values["encoderImplementation"] as? String == "VideoToolbox")
        if Hardware.isVirtualMachine {
            // A CI runner: no media hardware and little CPU. Media must flow; speed isn't measured.
            #expect(fps >= 5, "frame rate \(fps)")
            #expect(audioReceived >= 20, "audio packets in 2 s: \(audioReceived)")
        } else {
            // 1080p at the full frame rate (the level fix), from hardware H.264, held back by nothing.
            #expect(fps >= 25, "frame rate \(fps)")
            #expect(outbound?.values["qualityLimitationReason"] as? String == "none")
            #expect(client.frames.size == CGSize(width: 1920, height: 1080), "size \(client.frames.size)")
            #expect(audioReceived >= 80, "audio packets in 2 s: \(audioReceived)")  // 20 ms packets: ~100
        }
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
        if !Hardware.isVirtualMachine {
            #expect(client.frames.size == CGSize(width: 640, height: 480))
        }
    }
}

@Suite(.serialized) struct NegotiationTests {
    /// Review T14 #7: restarts asked for while an offer is out wait for its
    /// answer; a stale answer is ignored; a bad ICE index is dropped.
    @Test func restartsQueueAndStaleAnswersAreIgnored() async throws {
        var settings = MediaEngine.Settings()
        settings.camera = .testPattern
        settings.microphone = .testTone
        let engine = MediaEngine(settings: settings)
        let client = LoopbackClient(engine: engine)
        defer {
            engine.end(channel: client)
            client.peer.close()
        }
        engine.start(channel: client, hello: Hello(name: "test", app: "test", video: VideoFormat(width: 640, height: 480, fps: 30)))
        #expect(await waitUntil { client.connected })
        #expect(client.offers == 1)

        engine.receive(.iceRestart, from: client)
        engine.receive(.iceRestart, from: client)
        #expect(await waitUntil { client.offers == 3 })
        try await Task.sleep(for: .milliseconds(500))
        #expect(client.offers == 3)
        #expect(client.errors.isEmpty, "\(client.errors)")

        engine.receive(.answer(sdp: "v=0\r\n"), from: client)  // answers nothing outstanding
        engine.receive(.ice(candidate: "candidate:1 1 udp 1 192.0.2.10 9 typ host", sdpMid: "0", sdpMLineIndex: Int.max), from: client)
        try await Task.sleep(for: .milliseconds(300))
        #expect(client.errors.isEmpty, "\(client.errors)")

        // Media still flows after the restarts.
        engine.receive(.media(video: true, audio: false), from: client)
        #expect(await waitUntil { client.frames.count > 10 })
    }
}

@Suite(.serialized) struct SleepTests {
    /// Review T14: after stopAll() (before sleep), events still queued for the
    /// ended session must not start a call or a device; after resume() they may.
    @Test func nothingStartsBetweenStopAllAndResume() async throws {
        var settings = MediaEngine.Settings()
        settings.camera = .testPattern
        settings.microphone = .testTone
        let engine = MediaEngine(settings: settings)
        let client = LoopbackClient(engine: engine)
        defer {
            engine.end(channel: client)
            client.peer.close()
        }
        await engine.stopAll()
        engine.start(channel: client, hello: Hello(name: "test", app: "test", video: VideoFormat(width: 640, height: 480, fps: 30)))
        engine.receive(.media(video: true, audio: true), from: client)
        try await Task.sleep(for: .milliseconds(500))
        #expect(client.offers == 0)
        #expect(engine.cameraFramesCaptured == 0)
        #expect(!engine.microphoneIsRecording)

        engine.resume()
        engine.start(channel: client, hello: Hello(name: "test", app: "test", video: VideoFormat(width: 640, height: 480, fps: 30)))
        #expect(await waitUntil { client.connected })
        engine.receive(.media(video: true, audio: false), from: client)
        #expect(await waitUntil { client.frames.count > 5 })
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
