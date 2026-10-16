// SPDX-License-Identifier: GPL-3.0-or-later
// The control server end to end, over TCP on loopback (docs/protocol.md, sections 4 to 6).

import Foundation
import Testing
@testable import AVRelayCore

/// A server on a free port with its own state folder, plus a client identity.
struct Fixture {
    let server: RelayServer
    let port: UInt16
    let directory: URL
    let handler = RecordingHandler()

    init(configure: (inout RelayServer.Configuration) -> Void = { _ in }) async throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("av-relay-test-\(UUID().uuidString)")
        var configuration = RelayServer.Configuration(name: "Test Mac", app: "AV Relay test")
        configuration.port = 0
        configuration.advertise = false
        configure(&configuration)
        server = RelayServer(configuration: configuration, store: try IdentityStore(directory: directory))
        await server.setHandler(handler)
        port = try await server.start()
    }

    func pair(_ identity: NoiseKeyPair, name: String = "test-client") async throws -> PairedServer {
        let (code, _) = await server.openPairingWindow()
        return try await RelayClient.pair(host: "127.0.0.1", port: port, code: code, identity: identity,
                                          info: ClientInfo(name: name, app: "test"))
    }

    func session(_ identity: NoiseKeyPair, server key: Data) async throws -> ClientSession {
        try await RelayClient.openSession(host: "127.0.0.1", port: port, server: key, identity: identity,
                                          hello: .hello(name: "test-client", app: "test", video: VideoFormat(width: 1280, height: 720, fps: 30)))
    }

    func cleanUp() async {
        await server.stop()
        try? FileManager.default.removeItem(at: directory)
    }
}

final class RecordingHandler: SessionHandler, @unchecked Sendable {
    private let lock = NSLock()
    private var _messages: [Message] = []
    private var _started = 0
    private var _ended = 0
    var messages: [Message] { lock.withLock { _messages } }
    var started: Int { lock.withLock { _started } }
    var ended: Int { lock.withLock { _ended } }

    func sessionDidStart(_ session: ServerSession, hello: Hello) async {
        lock.withLock { _started += 1 }
        try? await session.send(.offer(sdp: "v=0 (test)"))
    }

    func session(_ session: ServerSession, didReceive message: Message) async {
        lock.withLock { _messages.append(message) }
    }

    func sessionDidEnd(_ session: ServerSession) async {
        lock.withLock { _ended += 1 }
    }
}

/// Polls until the condition holds (at most ~2 s).
func eventually(_ condition: () async -> Bool) async -> Bool {
    for _ in 0..<200 {
        if await condition() { return true }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return false
}

@Suite(.serialized) struct ServerTests {
    @Test func pairThenSession() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me, name: "laptop\u{7}\n")
        #expect(paired.publicKey == f.server.store.identity.publicKey)
        #expect(paired.name == "Test Mac")
        #expect(paired.deviceID == f.server.store.deviceID)
        let stored = try #require(f.server.store.client(publicKey: me.publicKey))
        #expect(stored.name == "laptop")

        let session = try await f.session(me, server: paired.publicKey)
        #expect(try await session.receive() == .welcome(name: "Test Mac", app: "AV Relay test", deviceID: f.server.store.deviceID))
        #expect(try await session.receive() == .offer(sdp: "v=0 (test)"))
        try await session.send(.ping(id: 7))
        #expect(try await session.receive() == .pong(id: 7))
        try await session.send(.media(video: true, audio: false))
        #expect(await eventually { f.handler.messages == [.media(video: true, audio: false)] })
        #expect(f.handler.started == 1)
        await session.close()
        #expect(await eventually { f.handler.ended == 1 })
    }

    @Test func wrongCodeUsesUpTheCode() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let (code, _) = await f.server.openPairingWindow()
        let wrong = PairingCode(String(format: "%06d", (Int(code.digits)! + 1) % 1_000_000))!
        await #expect(throws: RelayClientError.pairingRejected) {
            _ = try await RelayClient.pair(host: "127.0.0.1", port: f.port, code: wrong, identity: NoiseKeyPair(),
                                           info: ClientInfo(name: "x", app: "test"))
        }
        #expect(f.server.store.pairedClients.isEmpty)
        // The code is gone: even the right one fails now.
        await #expect(throws: RelayClientError.server(code: "not-pairing", message: nil)) {
            _ = try await RelayClient.pair(host: "127.0.0.1", port: f.port, code: code, identity: NoiseKeyPair(),
                                           info: ClientInfo(name: "x", app: "test"))
        }
    }

    @Test func noWindowNoPairing() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        await #expect(throws: RelayClientError.server(code: "not-pairing", message: nil)) {
            _ = try await RelayClient.pair(host: "127.0.0.1", port: f.port, code: .random(), identity: NoiseKeyPair(),
                                           info: ClientInfo(name: "x", app: "test"))
        }
    }

    @Test func windowExpires() async throws {
        let f = try await Fixture { $0.pairingWindow = 0.2 }
        defer { Task { await f.cleanUp() } }
        let (code, _) = await f.server.openPairingWindow()
        try await Task.sleep(for: .milliseconds(400))
        await #expect(throws: RelayClientError.server(code: "not-pairing", message: nil)) {
            _ = try await RelayClient.pair(host: "127.0.0.1", port: f.port, code: code, identity: NoiseKeyPair(),
                                           info: ClientInfo(name: "x", app: "test"))
        }
    }

    @Test func unknownClientIsNotPaired() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let session = try await f.session(NoiseKeyPair(), server: f.server.store.identity.publicKey)
        #expect(try await session.receive() == .error(code: "not-paired", message: nil, versions: nil))
    }

    @Test func staleServerKey() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        await #expect(throws: RelayClientError.serverKeyMismatch) {
            _ = try await f.session(NoiseKeyPair(), server: NoiseKeyPair().publicKey)
        }
    }

    @Test func sameClientReplacesItsSession() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        let first = try await f.session(me, server: paired.publicKey)
        _ = try await first.receive()  // welcome
        _ = try await first.receive()  // offer
        let second = try await f.session(me, server: paired.publicKey)
        #expect(try await first.receive() == .bye(.replaced))
        #expect(try await second.receive() == .welcome(name: "Test Mac", app: "AV Relay test", deviceID: f.server.store.deviceID))
    }

    @Test func otherClientIsBusy() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let (a, b) = (NoiseKeyPair(), NoiseKeyPair())
        let paired = try await f.pair(a, name: "a")
        _ = try await f.pair(b, name: "b")
        let first = try await f.session(a, server: paired.publicKey)
        _ = try await first.receive()
        let second = try await f.session(b, server: paired.publicKey)
        #expect(try await second.receive() == .error(code: "busy", message: nil, versions: nil))
        try await first.send(.ping(id: 1))
        _ = try await first.receive()  // offer
        #expect(try await first.receive() == .pong(id: 1))
    }

    /// A replayed message 1 of a live client must not disturb its session (protocol section 6).
    @Test func replayedMessage1ChangesNothing() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)

        // The client's session, recording its message 1 as an eavesdropper would.
        let conn = try await FramedConnection.connect(host: "127.0.0.1", port: f.port)
        let preamble = try JSON.encode(Preamble(mode: .session))
        try await conn.send(preamble)
        var noise = try NoiseHandshake(pattern: .ik, initiator: true, prologue: PairingCrypto.sessionPrologue,
                                       staticKey: me, remoteStatic: paired.publicKey)
        let message1 = try noise.writeMessage()
        try await conn.send(message1)
        _ = try noise.readMessage(try await conn.receiveFrame())
        var transport = try noise.transport()
        try await conn.send(try transport.encrypt(JSON.encode(Message.hello(name: "real", app: "test", video: nil))))
        _ = try transport.decrypt(try await conn.receiveFrame())  // welcome

        // The replay: the same preamble and message 1, then garbage instead of hello.
        let replay = try await FramedConnection.connect(host: "127.0.0.1", port: f.port)
        try await replay.send(preamble)
        try await replay.send(message1)
        _ = try await replay.receiveFrame()  // the server's message 2, useless without the ephemeral key
        try await replay.send(Data(repeating: 0, count: 40))
        await #expect(throws: ConnectionError.self) { _ = try await replay.receiveFrame() }

        // The live session is untouched.
        #expect(f.handler.ended == 0)
        _ = try transport.decrypt(try await conn.receiveFrame())  // offer
        try await conn.send(try transport.encrypt(JSON.encode(Message.ping(id: 9))))
        #expect(try JSON.decode(Message.self, from: try transport.decrypt(try await conn.receiveFrame())) == .pong(id: 9))
        conn.close()
    }

    @Test func clientUnpairs() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        let session = try await f.session(me, server: paired.publicKey)
        _ = try await session.receive()
        _ = try await session.receive()
        try await session.send(.unpair)
        #expect(try await session.receive() == .bye(.unpaired))
        #expect(f.server.store.pairedClients.isEmpty)
    }

    @Test func macRemovesClient() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        let session = try await f.session(me, server: paired.publicKey)
        _ = try await session.receive()
        _ = try await session.receive()
        try await f.server.removeClient(publicKey: me.publicKey)
        #expect(try await session.receive() == .bye(.unpaired))
        #expect(await eventually { f.handler.ended == 1 })
    }

    @Test func idleSessionTimesOut() async throws {
        let f = try await Fixture { $0.idleTimeout = 0.3 }
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        let session = try await f.session(me, server: paired.publicKey)
        _ = try await session.receive()
        _ = try await session.receive()
        await #expect(throws: (any Error).self) { _ = try await session.receive() }
        #expect(await eventually { f.handler.ended == 1 })
    }

    @Test func unsupportedVersion() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let conn = try await FramedConnection.connect(host: "127.0.0.1", port: f.port)
        try await conn.send(Data(#"{"proto":"av-relay","version":2,"mode":"session"}"#.utf8))
        #expect(try JSON.decode(Message.self, from: try await conn.receiveFrame()) == .error(code: "unsupported-version", message: nil, versions: [1]))
        conn.close()
    }

    @Test func identityPersists() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("av-relay-test-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let first = try IdentityStore(directory: directory)
        try first.add(PairedClient(publicKey: NoiseKeyPair().publicKey, name: "a"))
        let second = try IdentityStore(directory: directory)
        #expect(second.identity.publicKey == first.identity.publicKey)
        #expect(second.pairedClients == first.pairedClients)
        let attributes = try FileManager.default.attributesOfItem(atPath: directory.appendingPathComponent("identity.key").path)
        #expect((attributes[.posixPermissions] as? NSNumber)?.intValue == 0o600)
    }
}

@Suite struct MessageTests {
    @Test func roundTrips() throws {
        let messages: [Message] = [
            .hello(name: "n", app: "a", video: VideoFormat(width: 1920, height: 1080, fps: 30)),
            .welcome(name: "n", app: "a", deviceID: "0011223344556677"),
            .offer(sdp: "v=0\r\n"), .answer(sdp: "v=0\r\n"),
            .ice(candidate: "candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host", sdpMid: "0", sdpMLineIndex: 0),
            .media(video: true, audio: false),
            .mediaState(video: DeviceState(state: .on, device: "Cam", width: 1280, height: 720, fps: 30),
                        audio: DeviceState(state: .error, device: "Mic", error: .permissionDenied)),
            .ping(id: 1), .pong(id: 1), .iceRestart, .unpair, .bye(.quit), .error(.busy),
        ]
        for message in messages {
            #expect(try JSON.decode(Message.self, from: JSON.encode(message)) == message)
        }
    }

    @Test func unknownTypesAndFieldsAreIgnored() throws {
        #expect(try JSON.decode(Message.self, from: Data(#"{"type":"future-thing","x":1}"#.utf8)) == .unknown(type: "future-thing"))
        #expect(try JSON.decode(Message.self, from: Data(#"{"type":"ping","id":3,"extra":true}"#.utf8)) == .ping(id: 3))
    }

    @Test func codes() {
        for _ in 0..<1000 {
            let code = PairingCode.random()
            #expect(code.digits.count == 6)
            #expect(code.display.count == 7)
        }
        #expect(PairingCode("12345") == nil)
        #expect(PairingCode("12a456") == nil)
    }
}
