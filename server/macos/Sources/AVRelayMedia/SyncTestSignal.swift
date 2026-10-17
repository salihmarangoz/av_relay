// SPDX-License-Identifier: GPL-3.0-or-later
// A test signal for audio/video sync: every 2 seconds one white video frame
// (black otherwise) and the start of a 50 ms 1 kHz beep (silence otherwise),
// both at the same capture instant. Measuring flash against beep at the
// receiver's camera and microphone gives the A/V offset end to end.
//
// Both are laid on the host clock, the clock camera and microphone
// timestamps come from: the flash frame and the beep's first sample are
// captured at whole multiples of 2 s since boot, exactly.

import Foundation

enum SyncTestSignal {
    static let periodNs: UInt64 = 2_000_000_000
    static let sampleRate: UInt64 = 48_000
    /// 10 ms of audio, what libwebrtc takes at a time.
    static let chunkFrames: UInt64 = sampleRate / 100
    static let beepFrames: UInt64 = sampleRate / 20
    static let beepHz = 1000.0
    static let beepLevel = 0.5

    // MARK: Video

    /// Frames per period at `fps` (60 at 30 fps).
    static func framesPerPeriod(fps: Int) -> UInt64 { UInt64(max(fps, 1)) * periodNs / 1_000_000_000 }

    /// When frame `index` (counted from boot) is captured, in ns.
    static func frameTime(_ index: UInt64, fps: Int) -> UInt64 {
        let perPeriod = framesPerPeriod(fps: fps)
        return index / perPeriod * periodNs + index % perPeriod * periodNs / perPeriod
    }

    /// The last frame whose capture time has come by `nowNs`.
    static func frameIndex(atOrBefore nowNs: UInt64, fps: Int) -> UInt64 {
        let perPeriod = framesPerPeriod(fps: fps)
        var index = nowNs / periodNs * perPeriod + nowNs % periodNs * perPeriod / periodNs
        // frameTime rounds down too, so the next frame may already be due.
        while frameTime(index + 1, fps: fps) <= nowNs { index += 1 }
        return index
    }

    static func isFlash(frame index: UInt64, fps: Int) -> Bool { index % framesPerPeriod(fps: fps) == 0 }

    // MARK: Audio

    /// When the first sample of chunk `index` (10 ms chunks counted from boot) is captured, in ns.
    static func chunkTime(_ index: UInt64) -> UInt64 { index * 10_000_000 }

    /// Chunks wholly captured by `nowNs` end before this index.
    static func chunksDone(by nowNs: UInt64) -> UInt64 { nowNs / 10_000_000 }

    /// Chunk `index`'s samples: the beep where it falls, silence elsewhere.
    static func fillChunk(_ index: UInt64, into samples: UnsafeMutableBufferPointer<Int16>) {
        let periodFrames = periodNs / 1_000_000_000 * sampleRate
        for i in 0..<Int(chunkFrames) {
            let intoPeriod = (index * chunkFrames + UInt64(i)) % periodFrames
            guard intoPeriod < beepFrames else {
                samples[i] = 0
                continue
            }
            let value = sin(2 * .pi * beepHz * Double(intoPeriod) / Double(sampleRate)) * beepLevel
            samples[i] = Int16(value * Double(Int16.max))
        }
    }

    // MARK: Host clock

    private static let timebase: mach_timebase_info_data_t = {
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        return info
    }()

    static var nowNs: UInt64 { hostTicksToNs(mach_absolute_time()) }
    static func hostTicksToNs(_ ticks: UInt64) -> UInt64 { ticks * UInt64(timebase.numer) / UInt64(timebase.denom) }
    static func nsToHostTicks(_ ns: UInt64) -> UInt64 { ns * UInt64(timebase.denom) / UInt64(timebase.numer) }
}

extension UInt64 {
    func saturatingSubtract(_ other: UInt64) -> UInt64 { self > other ? self - other : 0 }
}
