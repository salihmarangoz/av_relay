// SPDX-License-Identifier: GPL-3.0-or-later
// Regression tests for the T2 review: ordering, races around the active
// session, the pairing window, and the identity store.

import Foundation
import Testing
@testable import AVRelayCore

/// Sends a burst from many tasks at once when a session starts.
final class BurstHandler: SessionHandler, @unchecked Sendable {
    let count: Int
    init(count: Int) { self.count = count }

    func sessionDidStart(_ session: ServerSession, hello: Hello) async {
        await withTaskGroup(of: Void.self) { group in
            for i in 0..<count {
                group.addTask { try? await session.send(.ice(candidate: "c\(i)", sdpMid: "0", sdpMLineIndex: 0)) }
            }
        }
    }
    func session(_ session: ServerSession, didReceive message: Message) async {}
    func sessionDidEnd(_ session: ServerSession) async {}
}

/// Takes its time to start, like a camera permission prompt.
final class SlowHandler: SessionHandler, @unchecked Sendable {
    func sessionDidStart(_ session: ServerSession, hello: Hello) async {
        try? await Task.sleep(for: .milliseconds(1200))
    }
    func session(_ session: ServerSession, didReceive message: Message) async {}
    func sessionDidEnd(_ session: ServerSession) async {}
}

/// A session handshake by hand, stopping before hello.
struct RawSession {
    let connection: FramedConnection
    var transport: NoiseTransport

    static func handshake(port: UInt16, me: NoiseKeyPair, server: Data) async throws -> RawSession {
        let connection = try await FramedConnection.connect(host: "127.0.0.1", port: port)
        try await connection.send(try JSON.encode(Preamble(mode: .session)))
        var noise = try NoiseHandshake(pattern: .ik, initiator: true, prologue: PairingCrypto.sessionPrologue,
                                       staticKey: me, remoteStatic: server)
        try await connection.send(try noise.writeMessage())
        _ = try noise.readMessage(try await connection.receiveFrame())
        return RawSession(connection: connection, transport: try noise.transport())
    }

    mutating func send(_ message: Message) async throws {
        try await connection.send(try transport.encrypt(JSON.encode(message)))
    }

    mutating func sendRaw(_ plaintext: Data) async throws {
        try await connection.send(try transport.encrypt(plaintext))
    }

    mutating func receive() async throws -> Message {
        try JSON.decode(Message.self, from: try transport.decrypt(try await connection.receiveFrame()))
    }
}

@Suite(.serialized) struct ReviewTests {
    /// Review 1: concurrent sends must reach the wire in nonce order.
    @Test func concurrentSendsStayInNonceOrder() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        await f.server.setHandler(BurstHandler(count: 200))
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        let session = try await f.session(me, server: paired.publicKey)
        for i in 0..<50 { try await session.send(.ping(id: i)) }
        var ice = 0, pongs = 0
        while ice < 200 || pongs < 50 {
            switch try await session.receive() {
            case .ice: ice += 1
            case .pong: pongs += 1
            default: break
            }
        }
        #expect(ice == 200 && pongs == 50)
    }

    /// Review 2: replacing a session never leaves two sessions served.
    @Test func rapidReconnectsLeaveOneSession() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        var sessions: [ClientSession] = []
        try await withThrowingTaskGroup(of: ClientSession.self) { group in
            for _ in 0..<8 { group.addTask { try await f.session(me, server: paired.publicKey) } }
            for try await s in group { sessions.append(s) }
        }
        // Exactly one client still gets answers.
        var served = 0
        for (i, session) in sessions.enumerated() {
            try? await session.send(.ping(id: i))
            let answered = await withTaskGroup(of: Bool.self) { group in
                group.addTask {
                    while let message = try? await session.receive() {
                        if message == .pong(id: i) { return true }
                    }
                    return false
                }
                group.addTask { try? await Task.sleep(for: .milliseconds(500)); return false }
                let first = await group.next()!
                group.cancelAll()
                await session.close()
                return first
            }
            if answered { served += 1 }
        }
        #expect(served == 1)
        // All clients are closed now: every session that started has ended, once.
        #expect(await eventually { f.handler.started >= 1 && f.handler.ended == f.handler.started })
    }

    /// Review 3: a client removed while the server waits for hello isn't served.
    @Test func removedBeforeHelloIsNotPaired() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        var raw = try await RawSession.handshake(port: f.port, me: me, server: paired.publicKey)
        try await f.server.removeClient(publicKey: me.publicKey)
        try await raw.send(.hello(name: "x", app: "test", video: nil))
        #expect(try await raw.receive() == .error(code: "not-paired", message: nil, versions: nil))
        #expect(await f.server.activeSession == nil)
        #expect(f.handler.started == 0)
    }

    /// Review 4: frames from a replaced session never reach the handler.
    @Test func replacedSessionMessagesAreDropped() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        let old = try await f.session(me, server: paired.publicKey)
        _ = try await old.receive()  // welcome
        _ = try await old.receive()  // offer
        let new = try await f.session(me, server: paired.publicKey)
        _ = try await new.receive()  // welcome
        try? await old.send(.media(video: true, audio: true))
        try await new.send(.media(video: false, audio: true))
        #expect(await eventually { f.handler.messages == [.media(video: false, audio: true)] })
        try await Task.sleep(for: .milliseconds(100))
        #expect(f.handler.messages == [.media(video: false, audio: true)])
    }

    /// Review 7: a slow handler doesn't trip the idle timeout.
    @Test func slowHandlerDoesNotStallTheSession() async throws {
        let f = try await Fixture { $0.idleTimeout = 0.5 }
        defer { Task { await f.cleanUp() } }
        await f.server.setHandler(SlowHandler())
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        let session = try await f.session(me, server: paired.publicKey)
        _ = try await session.receive()  // welcome
        for i in 0..<8 {
            try await session.send(.ping(id: i))
            #expect(try await session.receive() == .pong(id: i))
            try await Task.sleep(for: .milliseconds(200))
        }
    }

    /// Review 8: closing the window during an attempt stores nothing.
    @Test func windowClosedDuringAttemptStoresNothing() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let (code, _) = await f.server.openPairingWindow()
        let conn = try await FramedConnection.connect(host: "127.0.0.1", port: f.port)
        try await conn.send(try JSON.encode(Preamble(mode: .pair)))
        let start = try JSON.decode(PairStart.self, from: try await conn.receiveFrame())
        let sidC = PairingCrypto.randomSidHalf()
        let sid = sidC + start.sid
        let me = NoiseKeyPair()
        let cpace = CPace.Party(initiator: true, prs: PairingCrypto.prs(code: code), ci: PairingCrypto.channelIdentifier, sid: sid)
        try await conn.send(try JSON.encode(CPaceMessage(sid: sidC, share: cpace.share)))
        let reply = try JSON.decode(CPaceMessage.self, from: try await conn.receiveFrame())
        var noise = try NoiseHandshake(pattern: .xxpsk3, initiator: true,
                                       prologue: PairingCrypto.pairPrologue(sid: sid, ya: cpace.share, yb: reply.share),
                                       staticKey: me, psk: PairingCrypto.psk(isk: try cpace.isk(peerShare: reply.share)))
        try await conn.send(try noise.writeMessage())
        _ = try noise.readMessage(try await conn.receiveFrame())
        await f.server.closePairingWindow()
        try await conn.send(try noise.writeMessage(payload: JSON.encode(ClientInfo(name: "late", app: "test"))))
        await #expect(throws: ConnectionError.self) { _ = try await conn.receiveFrame() }
        #expect(f.server.store.pairedClients.isEmpty)
    }

    /// Review T14 #1: before sleep the session ends with bye(sleep), and
    /// until wake nobody gets a new session or a pairing.
    @Test func suspendForSleep() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        let session = try await f.session(me, server: paired.publicKey)
        _ = try await session.receive()  // welcome
        _ = try await session.receive()  // offer
        await f.server.suspend()
        #expect(try await session.receive() == .bye(.sleep))
        #expect(await eventually { f.handler.ended == 1 })
        #expect(await f.server.activeSession == nil)

        // Reconnecting at once gets nowhere while the Mac goes to sleep.
        await #expect(throws: (any Error).self) {
            let again = try await f.session(me, server: paired.publicKey)
            _ = try await again.receive()
        }
        await #expect(throws: (any Error).self) { _ = try await f.pair(NoiseKeyPair()) }
        #expect(f.handler.started == 1)

        // Awake: sessions are welcome again.
        await f.server.resume()
        let after = try await f.session(me, server: paired.publicKey)
        #expect(try await after.receive() == .welcome(name: "Test Mac", app: "AV Relay test", deviceID: f.server.store.deviceID))
    }

    @Test func unknownModeOfAFutureVersionGetsTheVersionError() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let conn = try await FramedConnection.connect(host: "127.0.0.1", port: f.port)
        try await conn.send(Data(#"{"proto":"av-relay","version":2,"mode":"something-new"}"#.utf8))
        #expect(try JSON.decode(Message.self, from: try await conn.receiveFrame())
                == .error(code: "unsupported-version", message: nil, versions: [1]))
    }

    @Test func secondHelloAndBadJSONCloseWithBadMessage() async throws {
        let f = try await Fixture()
        defer { Task { await f.cleanUp() } }
        let me = NoiseKeyPair()
        let paired = try await f.pair(me)
        for bad in [try JSON.encode(Message.hello(name: "again", app: "test", video: nil)), Data("{not json".utf8)] {
            var raw = try await RawSession.handshake(port: f.port, me: me, server: paired.publicKey)
            try await raw.send(.hello(name: "x", app: "test", video: nil))
            _ = try await raw.receive()  // welcome
            _ = try await raw.receive()  // offer
            try await raw.sendRaw(bad)
            guard case .error(code: "bad-message", _, _) = try await raw.receive() else {
                Issue.record("expected bad-message")
                continue
            }
            await #expect(throws: ConnectionError.self) { _ = try await raw.connection.receiveFrame() }
        }
    }

    /// Review 5: an unreadable key is an error, not a reason to make a new one.
    @Test func unreadableKeyIsNotReplaced() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("av-relay-test-\(UUID().uuidString)")
        defer {
            chmod(directory.appendingPathComponent("identity.key").path, 0o600)
            try? FileManager.default.removeItem(at: directory)
        }
        let first = try IdentityStore(directory: directory)
        let keyPath = directory.appendingPathComponent("identity.key").path
        chmod(keyPath, 0o000)
        #expect(throws: (any Error).self) { _ = try IdentityStore(directory: directory) }
        chmod(keyPath, 0o600)
        #expect(try IdentityStore(directory: directory).identity.publicKey == first.identity.publicKey)
    }

    /// Review 6: the folder is forced to 0700 and the key file is never wider than 0600.
    @Test func permissionsAreTightened() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("av-relay-test-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o755])
        let store = try IdentityStore(directory: directory)
        try store.add(PairedClient(publicKey: NoiseKeyPair().publicKey, name: "a"))
        func mode(_ path: String) throws -> Int {
            (try FileManager.default.attributesOfItem(atPath: path)[.posixPermissions] as! NSNumber).intValue
        }
        #expect(try mode(directory.path) == 0o700)
        #expect(try mode(directory.appendingPathComponent("identity.key").path) == 0o600)
        #expect(try mode(directory.appendingPathComponent("paired-clients.json").path) == 0o600)
    }
}
