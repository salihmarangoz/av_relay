// SPDX-License-Identifier: GPL-3.0-or-later
// A headless AV Relay server for development and interop tests: pairing and
// sessions as in docs/protocol.md, but no camera, microphone or WebRTC.
// It answers `media` with made-up states, so a client's on-demand logic can
// be exercised without the Mac app.
//
//   swift run avrelay-devserver [--port 48100] [--dir DIR] [--name NAME]
//
// Then type `pair` for a code. Commands: pair, close, list, remove ID, quit.

import AVRelayCore
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
var arguments = CommandLine.arguments.dropFirst()
while let flag = arguments.popFirst() {
    switch flag {
    case "--port": port = UInt16(arguments.popFirst() ?? "") ?? port
    case "--dir": directory = URL(fileURLWithPath: arguments.popFirst() ?? directory.path)
    case "--name": name = arguments.popFirst() ?? name
    default:
        print("usage: avrelay-devserver [--port N] [--dir DIR] [--name NAME]")
        exit(2)
    }
}

let store = try IdentityStore(directory: directory)
var configuration = RelayServer.Configuration(name: name, app: "AV Relay devserver")
configuration.port = port
let server = RelayServer(configuration: configuration, store: store)
let media = FakeMedia()
await server.setHandler(media)
let listening = try await server.start()
print("AV Relay dev server: TCP \(listening), device id \(store.deviceID), state in \(directory.path)")
print("commands: pair, close, list, remove ID, quit")

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
    case "quit":
        await server.stop()
        exit(0)
    case nil: break
    default: print("commands: pair, close, list, remove ID, quit")
    }
}
await server.stop()
