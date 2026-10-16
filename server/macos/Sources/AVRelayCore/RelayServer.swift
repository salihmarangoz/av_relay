// SPDX-License-Identifier: GPL-3.0-or-later
// The Mac's control server (docs/protocol.md, sections 4 to 7, 10 and 11):
// listens on TCP, runs pairing while the user has the pairing window open,
// and serves one authenticated session at a time. Media is not handled
// here; a SessionHandler (the media engine) gets each session's messages.

import Foundation
import Network
import os

/// What the media engine implements to take part in sessions.
public protocol SessionHandler: AnyObject, Sendable {
    /// A session is authenticated and `welcome` has been sent; send the first `offer` now.
    func sessionDidStart(_ session: ServerSession, hello: Hello) async
    /// `answer`, `ice`, `media`, `ice-restart`, and any type the server itself doesn't handle.
    func session(_ session: ServerSession, didReceive message: Message) async
    /// The session is over (closed, replaced, timed out); stop its capture now.
    /// Called once for every session that started, after its last message.
    func sessionDidEnd(_ session: ServerSession) async
}

public struct Hello: Sendable, Equatable {
    public var name: String
    public var app: String
    public var video: VideoFormat?
}

public actor RelayServer {
    public struct Configuration: Sendable {
        /// 0 picks a free port (tests).
        public var port: UInt16 = 48100
        /// The Mac's name as shown to clients.
        public var name: String
        /// "AV Relay 0.1.0 macOS".
        public var app: String
        public var pairingWindow: TimeInterval = 120
        public var pairingTimeout: TimeInterval = 60
        public var handshakeTimeout: TimeInterval = 10
        public var idleTimeout: TimeInterval = 15
        public var maxUnauthenticated = 8
        public var plaintextFrameLimit = 1024
        /// Advertise `_avrelay._tcp` over Bonjour.
        public var advertise = true

        public init(name: String, app: String) {
            self.name = name
            self.app = app
        }
    }

    public enum Event: Sendable, Equatable {
        case listening(port: UInt16)
        case listenerFailed(String)
        case pairingWindowOpened(code: PairingCode, expires: Date)
        case pairingAttemptStarted
        case pairingWindowClosed
        case paired(PairedClient)
        case pairingFailed(String)
        case sessionStarted(PairedClient)
        case sessionEnded(PairedClient, reason: String)
        case clientRemoved(PairedClient)
    }

    public nonisolated let configuration: Configuration
    public nonisolated let store: IdentityStore
    public nonisolated let events: AsyncStream<Event>
    private let emit: AsyncStream<Event>.Continuation
    private var handler: (any SessionHandler)?
    private var listener: NWListener?
    private var unauthenticated = 0
    private var window: PairingWindow?
    private var windowTimer: Task<Void, Never>?
    private var active: ServerSession?
    private let log = Logger(subsystem: "io.github.salihmarangoz.avrelay", category: "server")

    struct PairingWindow {
        let id = UUID()
        let code: PairingCode
        let expires: Date
        var attemptInProgress = false
    }

    public init(configuration: Configuration, store: IdentityStore) {
        self.configuration = configuration
        self.store = store
        (events, emit) = AsyncStream.makeStream(of: Event.self, bufferingPolicy: .bufferingNewest(64))
    }

    public func setHandler(_ handler: (any SessionHandler)?) {
        self.handler = handler
    }

    // MARK: Listening

    /// Starts listening; returns the port once the listener is ready.
    @discardableResult
    public func start() async throws -> UInt16 {
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        let parameters = NWParameters(tls: nil, tcp: tcp)
        parameters.allowLocalEndpointReuse = true
        let port = configuration.port == 0 ? NWEndpoint.Port.any : NWEndpoint.Port(rawValue: configuration.port)!
        let listener = try NWListener(using: parameters, on: port)
        if configuration.advertise {
            listener.service = NWListener.Service(name: configuration.name, type: "_avrelay._tcp", domain: nil,
                                                  txtRecord: Self.txtRecord(deviceID: store.deviceID))
        }
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { connection.cancel(); return }
            Task { await self.accept(connection) }
        }
        self.listener = listener
        let ready: UInt16 = try await withCheckedThrowingContinuation { continuation in
            let once = Once()
            listener.stateUpdateHandler = { [weak self] state in
                switch state {
                case .ready:
                    if once.claim() { continuation.resume(returning: listener.port?.rawValue ?? 0) }
                case .failed(let error):
                    if once.claim() {
                        continuation.resume(throwing: ConnectionError.failed(error.localizedDescription))
                    } else if let self {
                        Task { await self.listenerFailed(error) }
                    }
                default:
                    break
                }
            }
            listener.start(queue: DispatchQueue(label: "av-relay.listener"))
        }
        emit.yield(.listening(port: ready))
        log.info("listening on TCP \(ready)")
        return ready
    }

    public func stop() async {
        listener?.cancel()
        listener = nil
        closePairingWindow()
        if let session = active {
            active = nil
            await session.close(sending: .bye(.quit))
            endSession(session, reason: "quit")
        }
    }

    private func listenerFailed(_ error: NWError) {
        log.error("listener failed: \(error.localizedDescription, privacy: .public)")
        emit.yield(.listenerFailed(error.localizedDescription))
    }

    static func txtRecord(deviceID: String) -> NWTXTRecord {
        var txt = NWTXTRecord()
        txt["v"] = "1"
        txt["id"] = deviceID
        return txt
    }

    // MARK: Pairing window

    /// Opens the pairing window with a fresh code (replacing any open one).
    @discardableResult
    public func openPairingWindow() -> (code: PairingCode, expires: Date) {
        windowTimer?.cancel()
        let window = PairingWindow(code: .random(), expires: Date().addingTimeInterval(configuration.pairingWindow))
        self.window = window
        windowTimer = Task { [weak self, configuration] in
            try? await Task.sleep(for: .seconds(configuration.pairingWindow))
            await self?.windowExpired(window.id)
        }
        emit.yield(.pairingWindowOpened(code: window.code, expires: window.expires))
        log.info("pairing window open")
        return (window.code, window.expires)
    }

    public func closePairingWindow() {
        windowTimer?.cancel()
        windowTimer = nil
        if window != nil {
            window = nil
            emit.yield(.pairingWindowClosed)
        }
    }

    private func windowExpired(_ id: UUID) {
        // An attempt in progress may finish (it has its own timeout).
        guard let window, window.id == id, !window.attemptInProgress else { return }
        closePairingWindow()
    }

    enum WindowCheck { case open(UUID), error(ErrorCode) }

    private func checkWindow() -> WindowCheck {
        guard let window, window.expires > Date() else { return .error(.notPairing) }
        return window.attemptInProgress ? .error(.busy) : .open(window.id)
    }

    /// Uses up the code: one attempt per code.
    private func consumeCode(_ id: UUID) -> Result<PairingCode, ErrorCode> {
        guard var window, window.id == id, window.expires > Date() else { return .failure(.notPairing) }
        guard !window.attemptInProgress else { return .failure(.busy) }
        window.attemptInProgress = true
        self.window = window
        emit.yield(.pairingAttemptStarted)
        return .success(window.code)
    }

    /// Stores a newly paired client, but only while its pairing window is
    /// still the open one: the user may have closed it, or opened a new one,
    /// during the attempt.
    private func commitPairing(_ id: UUID, client: PairedClient) throws -> Bool {
        guard let window, window.id == id, window.attemptInProgress else { return false }
        try store.add(client)
        return true
    }

    private func pairingFinished(_ id: UUID, client: PairedClient?, failure: String?) {
        let wasOpen = window?.id == id
        if wasOpen {
            windowTimer?.cancel()
            windowTimer = nil
            window = nil
        }
        if let client {
            emit.yield(.paired(client))
            log.info("paired with \(client.name, privacy: .public)")
        } else {
            emit.yield(.pairingFailed(failure ?? "failed"))
            log.info("pairing failed: \(failure ?? "", privacy: .public)")
        }
        if wasOpen { emit.yield(.pairingWindowClosed) }
    }

    // MARK: Paired clients

    public func removeClient(publicKey: Data) async throws {
        guard let removed = try store.remove(publicKey: publicKey) else { return }
        emit.yield(.clientRemoved(removed))
        if let session = active, session.client.publicKey == publicKey {
            active = nil
            await session.close(sending: .bye(.unpaired))
            endSession(session, reason: "unpaired")
        }
    }

    public var activeSession: ServerSession? { active }

    // MARK: Connections

    private func accept(_ nw: NWConnection) async {
        guard unauthenticated < configuration.maxUnauthenticated else {
            nw.cancel()
            return
        }
        unauthenticated += 1
        let connection = FramedConnection(nw)
        do {
            try await connection.start()
        } catch {
            unauthenticated -= 1
            return
        }
        connection.setDeadline(configuration.handshakeTimeout)
        var counted = true
        defer { if counted { unauthenticated -= 1 } }
        do {
            let preambleData = try await connection.receiveFrame(maxLength: configuration.plaintextFrameLimit)
            // Loosely first: a future version may bring a mode we don't know.
            struct AnyPreamble: Decodable { var proto: String?; var version: Int?; var mode: String? }
            guard let loose = try? JSON.decode(AnyPreamble.self, from: preambleData),
                  loose.proto == Preamble.protocolName, let version = loose.version else {
                connection.close()
                return
            }
            guard version == Preamble.currentVersion else {
                try await sendPlain(.error(.unsupportedVersion), on: connection)
                connection.closeGracefully()
                return
            }
            guard let mode = loose.mode.flatMap(Preamble.Mode.init(rawValue:)) else {
                try await sendPlain(.error(.badMessage, "unknown mode"), on: connection)
                connection.closeGracefully()
                return
            }
            // The handshake's own timeout counts from the preamble.
            connection.setDeadline(configuration.handshakeTimeout)
            switch mode {
            case .pair:
                await runPairing(on: connection)
            case .session:
                if let session = try await authenticateSession(on: connection) {
                    unauthenticated -= 1
                    counted = false
                    await runSession(session)
                }
            }
        } catch {
            connection.close()
        }
    }

    private func sendPlain(_ message: Message, on connection: FramedConnection) async throws {
        try await connection.send(try JSON.encode(message))
    }

    // MARK: Pairing (section 5)

    private func runPairing(on connection: FramedConnection) async {
        connection.setDeadline(configuration.pairingTimeout)
        guard case let .open(windowID) = checkWindow() else {
            if case let .error(code) = checkWindow() { try? await sendPlain(.error(code), on: connection) }
            connection.closeGracefully()
            return
        }
        var consumed = false
        do {
            let sidS = PairingCrypto.randomSidHalf()
            try await connection.send(try JSON.encode(PairStart(sid: sidS)))
            let request = try JSON.decode(CPaceMessage.self,
                                          from: try await connection.receiveFrame(maxLength: configuration.plaintextFrameLimit))
            guard request.type == "cpace", let sidC = request.sid, sidC.count == PairingCrypto.sidHalfLength,
                  request.share.count == CPace.pointLength else {
                try? await sendPlain(.error(.badMessage), on: connection)
                connection.closeGracefully()
                return
            }
            let code: PairingCode
            switch consumeCode(windowID) {
            case .success(let c): code = c
            case .failure(let error):
                try? await sendPlain(.error(error), on: connection)
                connection.closeGracefully()
                return
            }
            consumed = true
            let sid = sidC + sidS
            let cpace = CPace.Party(initiator: false, prs: PairingCrypto.prs(code: code),
                                    ci: PairingCrypto.channelIdentifier, sid: sid)
            let isk = try cpace.isk(peerShare: request.share)
            try await connection.send(try JSON.encode(CPaceMessage(sid: nil, share: cpace.share)))

            var noise = try NoiseHandshake(pattern: .xxpsk3, initiator: false,
                                           prologue: PairingCrypto.pairPrologue(sid: sid, ya: request.share, yb: cpace.share),
                                           staticKey: store.identity, psk: PairingCrypto.psk(isk: isk))
            _ = try noise.readMessage(try await connection.receiveFrame())
            try await connection.send(try noise.writeMessage())
            let info = try JSON.decode(ClientInfo.self, from: try noise.readMessage(try await connection.receiveFrame()))
            guard let clientKey = noise.remoteStaticKey else { throw NoiseError.missingKey }
            var transport = try noise.transport()

            let client = PairedClient(publicKey: clientKey, name: Self.cleanName(info.name))
            guard try commitPairing(windowID, client: client) else {
                connection.close()
                pairingFinished(windowID, client: nil, failure: "the pairing window was closed")
                return
            }
            try await connection.send(try transport.encrypt(JSON.encode(
                Message.paired(name: configuration.name, deviceID: store.deviceID, app: configuration.app))))
            pairingFinished(windowID, client: client, failure: nil)
            connection.closeGracefully()
        } catch {
            connection.close()
            if consumed {
                pairingFinished(windowID, client: nil, failure: Self.describe(error))
            }
        }
    }

    static func describe(_ error: Error) -> String {
        switch error {
        case NoiseError.decryptionFailed: "wrong code"
        case CPaceError.invalidPeerPoint: "invalid message"
        case ConnectionError.closed: "the connection closed"
        default: "\(error)"
        }
    }

    /// A display name from the peer: printable, at most 64 characters.
    static func cleanName(_ name: String) -> String {
        let printable = name.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }
        let trimmed = String(String.UnicodeScalarView(printable)).trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? "Unnamed device" : String(trimmed.prefix(64))
    }

    // MARK: Session (section 6)

    /// Runs Noise IK and waits for `hello`. Returns the session only if
    /// `hello` decrypts: message 1 alone could be a replay.
    private func authenticateSession(on connection: FramedConnection) async throws -> ServerSession? {
        var noise = try NoiseHandshake(pattern: .ik, initiator: false, prologue: PairingCrypto.sessionPrologue,
                                       staticKey: store.identity)
        _ = try noise.readMessage(try await connection.receiveFrame())
        let message2 = try noise.writeMessage()
        guard let clientKey = noise.remoteStaticKey else { throw NoiseError.missingKey }
        var transport = try noise.transport()
        try await connection.send(message2)
        guard store.client(publicKey: clientKey) != nil else {
            try? await connection.send(try transport.encrypt(JSON.encode(Message.error(.notPaired))))
            connection.closeGracefully()
            return nil
        }
        let first = try transport.decrypt(try await connection.receiveFrame())
        guard case let .hello(name, app, video)? = try? JSON.decode(Message.self, from: first) else {
            try? await connection.send(try transport.encrypt(JSON.encode(Message.error(.badMessage, "expected hello"))))
            connection.closeGracefully()
            return nil
        }
        // The client holds its keys. It may have been removed while we
        // waited for hello, so look it up again.
        guard let current = store.client(publicKey: clientKey) else {
            try? await connection.send(try transport.encrypt(JSON.encode(Message.error(.notPaired))))
            connection.closeGracefully()
            return nil
        }
        // Decide and claim the slot with no suspension in between: it may
        // replace this client's own old session, or find the server busy.
        let previous = active
        if let previous, previous.client.publicKey != clientKey {
            try? await connection.send(try transport.encrypt(JSON.encode(Message.error(.busy))))
            connection.closeGracefully()
            return nil
        }
        let session = ServerSession(client: current, connection: connection, transport: transport,
                                    hello: Hello(name: name, app: app, video: video))
        active = session
        if let previous {
            await previous.close(sending: .bye(.replaced))
            endSession(previous, reason: "replaced")
        }
        return session
    }

    private func runSession(_ session: ServerSession) async {
        emit.yield(.sessionStarted(session.client))
        log.info("session with \(session.client.name, privacy: .public)")
        // The handler gets its calls in order on its own task, so a slow one
        // (a permission prompt) never stalls reading, pings or the idle timeout.
        // A session replaced before its welcome went out never starts, and
        // then doesn't end either, as far as the handler can tell.
        let handler = self.handler
        Task {
            var started = false
            for await event in session.events {
                switch event {
                case .started:
                    started = true
                    await handler?.sessionDidStart(session, hello: session.hello)
                case .message(let message):
                    await handler?.session(session, didReceive: message)
                case .ended:
                    if started { await handler?.sessionDidEnd(session) }
                    return
                }
            }
        }
        session.connection.setDeadline(configuration.idleTimeout)
        var reason = "closed"
        do {
            try await session.send(.welcome(name: configuration.name, app: configuration.app, deviceID: store.deviceID))
            session.deliver.yield(.started)
            while true {
                let plaintext = try await session.receive()
                // Replaced, removed or stopped while this frame was on its way: drop it.
                guard active === session else { throw ConnectionError.closed }
                session.connection.setDeadline(configuration.idleTimeout)
                guard let message = try? JSON.decode(Message.self, from: plaintext) else {
                    reason = "bad message"
                    await session.close(sending: .error(.badMessage))
                    throw ConnectionError.closed
                }
                switch message {
                case .ping(let id):
                    try await session.send(.pong(id: id))
                case .bye:
                    reason = "client left"
                    throw ConnectionError.closed
                case .unpair:
                    _ = try? store.remove(publicKey: session.client.publicKey)
                    emit.yield(.clientRemoved(session.client))
                    reason = "client unpaired"
                    await session.close(sending: .bye(.unpaired))
                    throw ConnectionError.closed
                case .hello:
                    reason = "second hello"
                    await session.close(sending: .error(.badMessage, "hello was already sent"))
                    throw ConnectionError.closed
                default:
                    session.deliver.yield(.message(message))
                }
            }
        } catch {
            if case ConnectionError.closed = error {} else if reason == "closed" { reason = Self.describe(error) }
        }
        if active === session {
            active = nil
            await session.close(sending: nil)
            endSession(session, reason: reason)
        }
    }

    /// Called exactly once per session that was active, by whoever took it
    /// out of `active`. The handler hears about it after any message it got.
    private func endSession(_ session: ServerSession, reason: String) {
        session.deliver.yield(.ended)
        session.deliver.finish()
        emit.yield(.sessionEnded(session.client, reason: reason))
        log.info("session with \(session.client.name, privacy: .public) ended: \(reason, privacy: .public)")
    }
}

/// An authenticated session's channel: sends and receives control messages in order.
public actor ServerSession {
    public nonisolated let client: PairedClient
    public nonisolated let hello: Hello
    nonisolated let connection: FramedConnection
    private var transport: NoiseTransport
    private var closed = false
    /// What the handler gets, in order: start, messages, end. See RelayServer.runSession.
    nonisolated let events: AsyncStream<SessionEvent>
    nonisolated let deliver: AsyncStream<SessionEvent>.Continuation

    enum SessionEvent: Sendable {
        case started
        case message(Message)
        case ended
    }

    init(client: PairedClient, connection: FramedConnection, transport: NoiseTransport, hello: Hello) {
        self.client = client
        self.connection = connection
        self.transport = transport
        self.hello = hello
        (events, deliver) = AsyncStream.makeStream(of: SessionEvent.self)
    }

    /// The Mac's address on this connection: the one to offer ICE candidates on.
    public nonisolated var localAddress: String? { connection.localAddress }
    public nonisolated var remoteAddress: String? { connection.remoteAddress }

    public var isClosed: Bool { closed }

    public func send(_ message: Message) async throws {
        guard !closed else { throw ConnectionError.closed }
        // Encrypt and queue with no suspension in between: the frames must
        // leave in nonce order.
        let sent = connection.enqueue(try transport.encrypt(JSON.encode(message)))
        try await sent.wait()
    }

    /// The next message's plaintext. Throws once the session is closed.
    func receive() async throws -> Data {
        guard !closed else { throw ConnectionError.closed }
        let frame = try await connection.receiveFrame()
        guard !closed else { throw ConnectionError.closed }
        return try transport.decrypt(frame)
    }

    /// Ends the session, optionally saying why first.
    public func close(sending message: Message?) {
        guard !closed else { return }
        closed = true
        if let message, let frame = try? transport.encrypt(JSON.encode(message)) {
            _ = connection.enqueue(frame)
            connection.closeGracefully()
        } else {
            connection.close()
        }
    }
}

/// Resumes a continuation once, from callbacks on a serial queue.
final class Once: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    func claim() -> Bool {
        lock.withLock {
            defer { done = true }
            return !done
        }
    }
}
