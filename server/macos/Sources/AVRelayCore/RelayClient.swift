// SPDX-License-Identifier: GPL-3.0-or-later
// The client side of pairing and sessions (docs/protocol.md, sections 5 and 6).
// The Mac app doesn't need it; the tests drive the server with it.

import Foundation

public enum RelayClientError: Error, Equatable {
    /// The server answered with an `error` message.
    case server(code: String, message: String?)
    /// Pairing failed after the code was used: most likely a wrong code.
    case pairingRejected
    /// The server closed during the handshake: it doesn't hold the key we know for it.
    case serverKeyMismatch
    case unexpected(String)
}

public struct PairedServer: Sendable, Equatable {
    public var publicKey: Data
    public var name: String
    public var deviceID: String
    public var app: String
}

public enum RelayClient {
    /// Pairs with the server at host:port using the code shown on the Mac.
    public static func pair(host: String, port: UInt16, code: PairingCode, identity: NoiseKeyPair,
                            info: ClientInfo) async throws -> PairedServer {
        let connection = try await FramedConnection.connect(host: host, port: port)
        defer { connection.close() }
        try await connection.send(try JSON.encode(Preamble(mode: .pair)))

        let startData = try await connection.receiveFrame(maxLength: 1024)
        try throwIfError(startData)
        let start = try JSON.decode(PairStart.self, from: startData)
        guard start.type == "pair-start", start.sid.count == PairingCrypto.sidHalfLength else {
            throw RelayClientError.unexpected("pair-start")
        }

        let sidC = PairingCrypto.randomSidHalf()
        let sid = sidC + start.sid
        let cpace = CPace.Party(initiator: true, prs: PairingCrypto.prs(code: code), ci: PairingCrypto.channelIdentifier, sid: sid)
        try await connection.send(try JSON.encode(CPaceMessage(sid: sidC, share: cpace.share)))
        let replyData = try await connection.receiveFrame(maxLength: 1024)
        try throwIfError(replyData)
        let reply = try JSON.decode(CPaceMessage.self, from: replyData)
        guard reply.type == "cpace", reply.share.count == CPace.pointLength else { throw RelayClientError.unexpected("cpace") }
        let isk = try cpace.isk(peerShare: reply.share)

        var noise = try NoiseHandshake(pattern: .xxpsk3, initiator: true,
                                       prologue: PairingCrypto.pairPrologue(sid: sid, ya: cpace.share, yb: reply.share),
                                       staticKey: identity, psk: PairingCrypto.psk(isk: isk))
        try await connection.send(try noise.writeMessage())
        _ = try noise.readMessage(try await connection.receiveFrame())
        try await connection.send(try noise.writeMessage(payload: JSON.encode(info)))
        var transport = try noise.transport()
        let paired: Message
        do {
            paired = try JSON.decode(Message.self, from: try transport.decrypt(try await connection.receiveFrame()))
        } catch is ConnectionError {
            throw RelayClientError.pairingRejected
        } catch NoiseError.decryptionFailed {
            throw RelayClientError.pairingRejected
        }
        guard case let .paired(name, deviceID, app) = paired, let serverKey = noise.remoteStaticKey else {
            throw RelayClientError.unexpected("paired")
        }
        return PairedServer(publicKey: serverKey, name: name, deviceID: deviceID, app: app)
    }

    /// Opens a session and sends `hello`; read `welcome` (or an error) next.
    public static func openSession(host: String, port: UInt16, server: Data, identity: NoiseKeyPair,
                                   hello: Message) async throws -> ClientSession {
        let connection = try await FramedConnection.connect(host: host, port: port)
        do {
            try await connection.send(try JSON.encode(Preamble(mode: .session)))
            var noise = try NoiseHandshake(pattern: .ik, initiator: true, prologue: PairingCrypto.sessionPrologue,
                                           staticKey: identity, remoteStatic: server)
            try await connection.send(try noise.writeMessage())
            let message2: Data
            do {
                message2 = try await connection.receiveFrame()
            } catch is ConnectionError {
                throw RelayClientError.serverKeyMismatch
            }
            _ = try noise.readMessage(message2)
            let session = ClientSession(connection: connection, transport: try noise.transport())
            try await session.send(hello)
            return session
        } catch {
            connection.close()
            throw error
        }
    }

    static func throwIfError(_ data: Data) throws {
        guard JSON.messageType(of: data) == "error" else { return }
        if case let .error(code, message, _) = try JSON.decode(Message.self, from: data) {
            throw RelayClientError.server(code: code, message: message)
        }
    }
}

public actor ClientSession {
    let connection: FramedConnection
    private var transport: NoiseTransport

    init(connection: FramedConnection, transport: NoiseTransport) {
        self.connection = connection
        self.transport = transport
    }

    public func send(_ message: Message) async throws {
        // Encrypt and queue without suspending in between (nonce order).
        let sent = connection.enqueue(try transport.encrypt(JSON.encode(message)))
        try await sent.wait()
    }

    public func receive() async throws -> Message {
        try JSON.decode(Message.self, from: try transport.decrypt(try await connection.receiveFrame()))
    }

    public func close() {
        connection.close()
    }
}
