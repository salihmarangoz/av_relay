// SPDX-License-Identifier: GPL-3.0-or-later
// Audio redundancy: RED (RFC 2198) around Opus, carrying the two frames
// before each one, when the receiver accepts it; plain Opus when it doesn't.

import AVRelayCore
@testable import AVRelayMedia
import Foundation
import Testing
import WebRTC

@Suite(.serialized) struct AudioRedundancyTests {
    /// Runs audio for 2 s; returns the answer and the sent payload bytes per packet.
    private func run(acceptingAudio codecs: [String]?) async throws -> (answer: String, bytesPerPacket: Double, received: Int) {
        var settings = MediaEngine.Settings()
        settings.camera = .testPattern
        settings.microphone = .testTone
        let engine = MediaEngine(settings: settings)
        let client = LoopbackClient(engine: engine)
        client.audioCodecs = codecs
        defer {
            engine.end(channel: client)
            client.peer.close()
        }
        engine.start(channel: client, hello: Hello(name: "test", app: "test", video: VideoFormat(width: 640, height: 360, fps: 30)))
        #expect(await waitUntil { client.connected })
        engine.receive(.media(video: false, audio: true), from: client)
        #expect(await waitUntil { await client.inbound("audio").packets > 20 })
        try await Task.sleep(for: .seconds(2))
        let report = await engine.statistics()
        let outbound = report?.statistics.values.first { $0.type == "outbound-rtp" && $0.values["kind"] as? String == "audio" }
        let bytes = (outbound?.values["bytesSent"] as? NSNumber)?.doubleValue ?? 0
        let packets = (outbound?.values["packetsSent"] as? NSNumber)?.doubleValue ?? 1
        return (client.answer ?? "", bytes / packets, await client.inbound("audio").packets)
    }

    /// The payload types of the answer's audio m-line, in order, by codec name.
    private func audioCodecs(_ sdp: String) -> [String] {
        let lines = sdp.components(separatedBy: "\r\n")
        guard let mline = lines.first(where: { $0.hasPrefix("m=audio") }) else { return [] }
        return mline.split(separator: " ").dropFirst(3).compactMap { pt in
            lines.first { $0.hasPrefix("a=rtpmap:\(pt) ") }?.split(separator: " ")[1].split(separator: "/").first.map(String.init)
        }
    }

    @Test func redWithTwoEarlierFramesWhenTheReceiverTakesIt() async throws {
        let (answer, bytesPerPacket, received) = try await run(acceptingAudio: nil)
        #expect(audioCodecs(answer).first == "red", "\(audioCodecs(answer))")
        // A 20 ms Opus frame at 32 kbit/s is ~80 bytes: three of them, plus RED's headers.
        #expect(bytesPerPacket > 200 && bytesPerPacket < 320, "\(bytesPerPacket) bytes per packet")
        #expect(received > 100)
    }

    @Test func plainOpusForAReceiverWithoutRed() async throws {
        let (answer, bytesPerPacket, received) = try await run(acceptingAudio: ["opus"])
        #expect(audioCodecs(answer) == ["opus"], "\(audioCodecs(answer))")
        #expect(bytesPerPacket < 130, "\(bytesPerPacket) bytes per packet")
        #expect(received > 100)
    }
}
