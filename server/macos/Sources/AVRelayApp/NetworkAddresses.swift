// SPDX-License-Identifier: GPL-3.0-or-later
// This Mac's IPv4 addresses, to show in the pairing window.

import Darwin
import Foundation

enum NetworkAddresses {
    struct Entry {
        var address: String
        var label: String
    }

    /// Up, non-loopback, non-link-local IPv4 addresses; Wi-Fi/Ethernet (en*) first.
    static func current() -> [Entry] {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return [] }
        defer { freeifaddrs(list) }
        var entries: [(Entry, Int)] = []
        for pointer in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let ifa = pointer.pointee
            guard let addr = ifa.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET),
                  ifa.ifa_flags & UInt32(IFF_UP) != 0, ifa.ifa_flags & UInt32(IFF_LOOPBACK) == 0 else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            guard getnameinfo(addr, socklen_t(addr.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 else { continue }
            let address = String(decoding: host.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
            guard !address.hasPrefix("169.254.") else { continue }
            let name = String(cString: ifa.ifa_name)
            let label = name.hasPrefix("en") ? "Wi-Fi or Ethernet (\(name))" : name.hasPrefix("utun") ? "VPN (\(name))" : name
            entries.append((Entry(address: address, label: label), name.hasPrefix("en") ? 0 : 1))
        }
        return entries.sorted { $0.1 < $1.1 }.map(\.0)
    }
}
