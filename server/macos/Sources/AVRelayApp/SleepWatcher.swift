// SPDX-License-Identifier: GPL-3.0-or-later
// System sleep and wake, through IOKit's power notifications: unlike
// NSWorkspace's, they let us hold the sleep until the camera and
// microphone are closed (macOS waits for the acknowledgement, up to 30 s).

import Foundation
import IOKit
import IOKit.pwr_mgt
import os

final class SleepWatcher {
    /// Called on the main thread when the Mac is about to sleep; call the
    /// given function once everything is stopped, to let the sleep go on.
    private let willSleep: (@escaping () -> Void) -> Void
    /// Called on the main thread when the Mac is awake again.
    private let didWake: () -> Void
    private var rootPort: io_connect_t = 0
    private var notifyPort: IONotificationPortRef?
    private var notifier: io_object_t = 0

    // iokit_common_msg(...) from IOKit/IOMessage.h (function-like macros don't reach Swift).
    private static let canSystemSleep: natural_t = 0xE000_0270
    private static let systemWillSleep: natural_t = 0xE000_0280
    private static let systemHasPoweredOn: natural_t = 0xE000_0300

    init(willSleep: @escaping (@escaping () -> Void) -> Void, didWake: @escaping () -> Void) {
        self.willSleep = willSleep
        self.didWake = didWake
        rootPort = IORegisterForSystemPower(Unmanaged.passUnretained(self).toOpaque(), &notifyPort, { refcon, _, type, argument in
            guard let refcon else { return }
            Unmanaged<SleepWatcher>.fromOpaque(refcon).takeUnretainedValue().handle(type, argument)
        }, &notifier)
        if rootPort != 0, let notifyPort {
            CFRunLoopAddSource(CFRunLoopGetMain(), IONotificationPortGetRunLoopSource(notifyPort).takeUnretainedValue(), .commonModes)
        } else {
            Logger(subsystem: "io.github.salihmarangoz.avrelay", category: "power")
                .error("couldn't register for sleep notifications")
        }
    }

    deinit {
        if notifier != 0 { IODeregisterForSystemPower(&notifier) }
        if let notifyPort { IONotificationPortDestroy(notifyPort) }
        if rootPort != 0 { IOServiceClose(rootPort) }
    }

    private func handle(_ type: natural_t, _ argument: UnsafeMutableRawPointer?) {
        let id = Int(bitPattern: argument)
        switch type {
        case Self.canSystemSleep:
            IOAllowPowerChange(rootPort, id)  // never veto idle sleep
        case Self.systemWillSleep:
            let port = rootPort
            willSleep { IOAllowPowerChange(port, id) }
        case Self.systemHasPoweredOn:
            didWake()
        default:
            break
        }
    }
}
