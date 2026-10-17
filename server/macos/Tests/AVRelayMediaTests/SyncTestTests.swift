// SPDX-License-Identifier: GPL-3.0-or-later
// The A/V sync test signal: the flash frame and the beep's first sample are
// captured at whole multiples of 2 s on the host clock, and the signal gets
// through the media engine.

import AVRelayCore
@testable import AVRelayMedia
import Foundation
import Testing
import WebRTC

@Suite struct SyncTestSignalTests {
    @Test(arguments: [24, 25, 30, 60])
    func flashFramesSitOnThePeriod(fps: Int) {
        let perPeriod = SyncTestSignal.framesPerPeriod(fps: fps)
        #expect(perPeriod == UInt64(2 * fps))
        for k in [0, 1, 7, 12_345].map(UInt64.init) {
            let flash = k * perPeriod
            #expect(SyncTestSignal.isFlash(frame: flash, fps: fps))
            #expect(SyncTestSignal.frameTime(flash, fps: fps) == k * SyncTestSignal.periodNs)
            #expect(!SyncTestSignal.isFlash(frame: flash + 1, fps: fps))
        }
    }

    @Test(arguments: [24, 25, 30, 60])
    func frameIndexIsTheLastFrameDue(fps: Int) {
        var generator = SystemRandomNumberGenerator()
        for _ in 0..<2000 {
            let now = UInt64.random(in: 1_000_000_000...200_000_000_000_000, using: &generator)
            let index = SyncTestSignal.frameIndex(atOrBefore: now, fps: fps)
            #expect(SyncTestSignal.frameTime(index, fps: fps) <= now)
            #expect(SyncTestSignal.frameTime(index + 1, fps: fps) > now)
        }
        // Exactly on a frame's time: that frame.
        let t = SyncTestSignal.frameTime(1_000_003, fps: fps)
        #expect(SyncTestSignal.frameIndex(atOrBefore: t, fps: fps) == 1_000_003)
    }

    @Test func theBeepStartsOnThePeriodAndLasts50ms() {
        let chunksPerPeriod = SyncTestSignal.periodNs / 10_000_000  // 200
        let first = 37 * chunksPerPeriod
        #expect(SyncTestSignal.chunkTime(first) % SyncTestSignal.periodNs == 0)
        var samples = [Int16](repeating: 1, count: Int(SyncTestSignal.chunkFrames))
        func chunk(_ index: UInt64) -> [Int16] {
            samples.withUnsafeMutableBufferPointer { SyncTestSignal.fillChunk(index, into: $0) }
            return samples
        }
        #expect(chunk(first - 1).allSatisfy { $0 == 0 }, "silence before")
        let onset = chunk(first)
        #expect(onset[0] == 0 && onset[1] > 0, "a sine from phase 0, at the chunk's first sample")
        // 1 kHz at 48 kHz: 48 samples per cycle; peak 0.5 of full scale.
        #expect(abs(Int(onset[12]) - Int(Int16.max) / 2) < 10)
        for c in first..<(first + 5) {
            #expect(chunk(c).contains { $0 != 0 }, "beep in chunk \(c - first)")
        }
        #expect(chunk(first + 5).allSatisfy { $0 == 0 }, "silence after 50 ms")
        #expect(SyncTestSignal.chunksDone(by: SyncTestSignal.chunkTime(first) + 9_999_999) == first)
        #expect(SyncTestSignal.chunksDone(by: SyncTestSignal.chunkTime(first + 1)) == first + 1)
    }

    @Test func hostClockConversionsRoundTrip() {
        let ns = SyncTestSignal.nowNs
        let back = SyncTestSignal.hostTicksToNs(SyncTestSignal.nsToHostTicks(ns))
        #expect(ns - back < 100, "\(ns) vs \(back)")
        #expect(DispatchTime.now().uptimeNanoseconds - ns < 50_000_000, "dispatch's uptime clock is the host clock")
    }
}

@Suite(.serialized) struct SyncTestLoopbackTests {
    @Test func flashesAndBeepsGetThrough() async throws {
        var settings = MediaEngine.Settings()
        settings.camera = .syncTest
        settings.microphone = .syncTest
        let engine = MediaEngine(settings: settings)
        let client = LoopbackClient(engine: engine)
        defer {
            engine.end(channel: client)
            client.peer.close()
        }
        engine.start(channel: client, hello: Hello(name: "test", app: "test", video: VideoFormat(width: 1280, height: 720, fps: 30)))
        #expect(await waitUntil { client.connected })
        engine.receive(.media(video: true, audio: true), from: client)
        #expect(await waitUntil { client.lastState().map { $0.video == .on && $0.audio == .on } == true })
        if case let .mediaState(video, audio)? = client.states.last {
            #expect(video.device == "Sync test (a white frame every 2 s)")
            #expect(audio.device == "Sync test (a beep every 2 s)")
        }
        #expect(await waitUntil { client.frames.count > 10 })
        let frames = client.frames.count, bright = client.frames.brightCount
        try await Task.sleep(for: .seconds(4.2))
        // 4.2 s at 30 fps, with two or three flashes of one frame each.
        let flashes = client.frames.brightCount - bright
        #expect((2...3).contains(flashes), "\(flashes) flashes")
        #expect(client.frames.count - frames >= (Hardware.isVirtualMachine ? 20 : 110), "\(client.frames.count - frames) frames")
        // The beep reached libwebrtc: the sent audio has energy (silence has none).
        let report = await engine.statistics()
        let source = report?.statistics.values.first { $0.type == "media-source" && $0.values["kind"] as? String == "audio" }
        #expect(((source?.values["totalAudioEnergy"] as? NSNumber)?.doubleValue ?? 0) > 0)
    }
}
