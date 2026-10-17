// SPDX-License-Identifier: GPL-3.0-or-later
// A headless AV Relay server for development and interop tests: pairing and
// sessions as in docs/protocol.md. By default without media: it answers
// `media` with made-up states, so a client's on-demand logic can be
// exercised. With --media test, the real media engine (libwebrtc, H.264 and
// Opus) sends a moving test pattern and a 440 Hz tone instead of the camera
// and microphone, so a client's WebRTC side can be tested too.
//
//   swift run avrelay-devserver [--port 48100] [--dir DIR] [--name NAME] [--media test]
//
// Then type `pair` for a code. Commands: pair, close, list, remove ID, stats, quit.

import AVRelayCore
import AVRelayMedia
import Foundation

final class FakeMedia: SessionHandler {
    func sessionDidStart(_ session: ServerSession, hello: Hello) async {
        print("session: hello from \(hello.name) (\(hello.app)), video \(hello.video.map { "\($0.width)x\($0.height)@\($0.fps)" } ?? "-")")
        try? await session.send(.mediaState(video: .off, audio: .off))
    }

    func session(_ session: ServerSession, didReceive message: Message) async {
        print("session: \(message)")
        guard case let .media(video, audio) = message else { return }
        let fmt = session.hello.video ?? VideoFormat(width: 1280, height: 720, fps: 30)
        func state(_ wanted: Bool, _ device: String, video: Bool) -> DeviceState {
            wanted ? DeviceState(state: .on, device: device, width: video ? fmt.width : nil,
                                 height: video ? fmt.height : nil, fps: video ? fmt.fps : nil)
                : DeviceState(state: .off, device: device)
        }
        try? await session.send(.mediaState(video: state(video, "Dev Camera", video: true),
                                            audio: state(audio, "Dev Microphone", video: false)))
    }

    func sessionDidEnd(_ session: ServerSession) async {
        print("session: ended")
    }
}

setvbuf(stdout, nil, _IOLBF, 0)  // line by line, also into a pipe

var port: UInt16 = 48100
var directory = FileManager.default.temporaryDirectory.appendingPathComponent("av-relay-devserver")
var name = Host.current().localizedName ?? "Mac"
var testMedia = false
var arguments = CommandLine.arguments.dropFirst()
while let flag = arguments.popFirst() {
    switch flag {
    case "--port": port = UInt16(arguments.popFirst() ?? "") ?? port
    case "--dir": directory = URL(fileURLWithPath: arguments.popFirst() ?? directory.path)
    case "--name": name = arguments.popFirst() ?? name
    case "--media" where arguments.first == "test":
        arguments.removeFirst()
        testMedia = true
    default:
        print("usage: avrelay-devserver [--port N] [--dir DIR] [--name NAME] [--media test]")
        exit(2)
    }
}

let store = try IdentityStore(directory: directory)
var configuration = RelayServer.Configuration(name: name, app: "AV Relay devserver")
configuration.port = port
let server = RelayServer(configuration: configuration, store: store)
let handler: any SessionHandler
var mediaEngine: MediaEngine?
if testMedia {
    var settings = MediaEngine.Settings()
    settings.camera = .testPattern
    settings.microphone = .testTone
    let engine = MediaEngine(settings: settings)
    engine.onStatus = { status in print("media: camera \(status.video.state), microphone \(status.audio.state)") }
    mediaEngine = engine
    handler = engine
} else {
    handler = FakeMedia()
}
await server.setHandler(handler)
let listening = try await server.start()
print("AV Relay dev server: TCP \(listening), device id \(store.deviceID), state in \(directory.path), media: \(testMedia ? "test pattern + tone" : "none")")
print("commands: pair, close, list, remove ID, stats, quit")

Task {
    for await event in server.events {
        switch event {
        case let .pairingWindowOpened(code, expires):
            print("pairing code: \(code.display) (until \(expires.formatted(date: .omitted, time: .standard)))")
        default:
            print("event: \(event)")
        }
    }
}

for try await line in FileHandle.standardInput.bytes.lines {
    let words = line.split(separator: " ").map(String.init)
    switch words.first {
    case "pair": await server.openPairingWindow()
    case "close": await server.closePairingWindow()
    case "list":
        for client in store.pairedClients {
            print("\(client.deviceID)  \(client.name)  paired \(client.pairedAt.formatted())")
        }
    case "remove":
        if let id = words.dropFirst().first, let client = store.pairedClients.first(where: { $0.deviceID == id }) {
            try await server.removeClient(publicKey: client.publicKey)
        } else {
            print("no such device id")
        }
    case "stats":
        print(await mediaEngine?.statisticsSummary() ?? "no media engine (start with --media test)")
    case "quit":
        await server.stop()
        exit(0)
    case nil: break
    default: print("commands: pair, close, list, remove ID, stats, quit")
    }
}
await server.stop()
