// swift-tools-version: 6.0
// SPDX-License-Identifier: GPL-3.0-or-later
import PackageDescription

let package = Package(
    name: "AVRelay",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "AVRelayCore", targets: ["AVRelayCore"]),
        .executable(name: "avrelay-devserver", targets: ["avrelay-devserver"]),
    ],
    dependencies: [
        // libsodium, for CPace's ristretto255 (prebuilt xcframework, pinned).
        .package(url: "https://github.com/jedisct1/swift-sodium.git", exact: "0.11.0"),
    ],
    targets: [
        // Protocol (docs/protocol.md): pairing, Noise, framing, messages, the control server.
        .target(
            name: "AVRelayCore",
            dependencies: [.product(name: "Clibsodium", package: "swift-sodium")]
        ),
        // A headless server for development and interop tests: pairing and sessions, no media.
        .executableTarget(name: "avrelay-devserver", dependencies: ["AVRelayCore"]),
        .testTarget(name: "AVRelayCoreTests", dependencies: ["AVRelayCore"]),
    ]
)
