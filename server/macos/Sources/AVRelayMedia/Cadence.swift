// SPDX-License-Identifier: GPL-3.0-or-later
// How regularly a capture device delivers: events and samples per window,
// and the longest gap between two events. Diagnostics, for telling a
// stutter that starts at the camera or microphone from one on the network.

import Foundation

struct Cadence: Sendable, CustomStringConvertible {
    var events = 0
    var samples = 0
    var seconds = 0.0
    var maxGap = 0.0

    var description: String {
        guard seconds > 0 else { return "-" }
        return String(format: "%.1f/s (%.0f samples/s), longest gap %.0f ms",
                      Double(events) / seconds, Double(samples) / seconds, maxGap * 1000)
    }
}

/// Confined to its device's queue.
struct CadenceMeter {
    private var window = Cadence()
    private var windowStart = Date()
    private var last: Date?

    mutating func tick(samples: Int = 1) {
        let now = Date()
        if let last { window.maxGap = max(window.maxGap, now.timeIntervalSince(last)) }
        last = now
        window.events += 1
        window.samples += samples
    }

    /// The window so far; starts a new one.
    mutating func take() -> Cadence {
        var result = window
        result.seconds = Date().timeIntervalSince(windowStart)
        window = Cadence()
        windowStart = Date()
        return result
    }

    mutating func reset() {
        self = CadenceMeter()
    }
}
