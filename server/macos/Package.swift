// swift-tools-version: 6.0
// SPDX-License-Identifier: GPL-3.0-or-later
import PackageDescription

let package = Package(
    name: "AVRelay",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "AVRelayCore", targets: ["AVRelayCore"]),
        .executable(name: "AVRelay", targets: ["AVRelay"]),
        .executable(name: "avrelay-devserver", targets: ["avrelay-devserver"]),
    ],
    dependencies: [
        // libsodium, for CPace's ristretto255 (prebuilt xcframework, pinned).
        .package(url: "https://github.com/jedisct1/swift-sodium.git", exact: "0.11.0"),
        // libwebrtc M155, prebuilt (BSD); the package pins the binary by checksum.
        .package(url: "https://github.com/stasel/WebRTC.git", exact: "155.0.0"),
    ],
    targets: [
        // Protocol (docs/protocol.md): pairing, Noise, framing, messages, the control server.
        .target(
            name: "AVRelayCore",
            dependencies: [.product(name: "Clibsodium", package: "swift-sodium")]
        ),
        // libwebrtc glue: the RTCAudioDevice protocol (missing from its macOS headers), helpers.
        .target(
            name: "CWebRTCSupport",
            dependencies: [.product(name: "WebRTC", package: "WebRTC")]
        ),
        // Media (docs/protocol.md, sections 8 and 9): camera and microphone on
        // demand, sent with libwebrtc. ObjC types without Sendable annotations,
        // so Swift 5 mode; the engine keeps its state on one serial queue.
        .target(
            name: "AVRelayMedia",
            dependencies: ["AVRelayCore", "CWebRTCSupport", .product(name: "WebRTC", package: "WebRTC")],
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        // The menu bar app. scripts/build-app.sh wraps it into "AV Relay.app".
        .executableTarget(
            name: "AVRelay",
            dependencies: ["AVRelayCore", "AVRelayMedia"],
            path: "Sources/AVRelayApp"
        ),
        // A headless server for development and interop tests: pairing, sessions, and optionally
        // the media engine with test sources (no camera or microphone).
        .executableTarget(name: "avrelay-devserver", dependencies: ["AVRelayCore", "AVRelayMedia"]),
        .testTarget(name: "AVRelayCoreTests", dependencies: ["AVRelayCore"]),
        .testTarget(
            name: "AVRelayMediaTests",
            dependencies: ["AVRelayMedia", .product(name: "WebRTC", package: "WebRTC")],
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
