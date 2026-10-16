// SPDX-License-Identifier: GPL-3.0-or-later
// A TCP connection that carries frames: a 2-byte big-endian length, then
// the payload (docs/protocol.md, section 4).

import Foundation
import Network

public enum ConnectionError: Error, Equatable {
    case closed
    case frameTooLarge(Int)
    case failed(String)
}

public final class FramedConnection: @unchecked Sendable {
    public static let maxFrameLength = 65535

    let connection: NWConnection
    private let queue: DispatchQueue
    private var deadline: DispatchWorkItem?  // touched on `queue` only

    public init(_ connection: NWConnection) {
        self.connection = connection
        queue = DispatchQueue(label: "av-relay.connection")
    }

    /// Connects to host:port (client side).
    public static func connect(host: String, port: UInt16, timeout: TimeInterval = 5) async throws -> FramedConnection {
        let options = NWProtocolTCP.Options()
        options.connectionTimeout = Int(timeout.rounded(.up))
        options.noDelay = true
        let connection = NWConnection(host: NWEndpoint.Host(host), port: NWEndpoint.Port(rawValue: port)!,
                                      using: NWParameters(tls: nil, tcp: options))
        let framed = FramedConnection(connection)
        try await framed.start(waitUntilReady: true)
        return framed
    }

    /// Starts the connection; for an accepted one, there's nothing to wait for.
    public func start(waitUntilReady: Bool = false) async throws {
        guard waitUntilReady else {
            connection.start(queue: queue)
            return
        }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let once = Once()
            connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    if once.claim() { continuation.resume() }
                case .failed(let error), .waiting(let error):
                    if once.claim() { continuation.resume(throwing: ConnectionError.failed(error.localizedDescription)) }
                case .cancelled:
                    if once.claim() { continuation.resume(throwing: ConnectionError.closed) }
                default:
                    break
                }
            }
            connection.start(queue: queue)
        }
        connection.stateUpdateHandler = nil
    }

    /// Reads one frame. Fails if the peer announces more than `maxLength` bytes.
    public func receiveFrame(maxLength: Int = FramedConnection.maxFrameLength) async throws -> Data {
        let header = try await receive(exactly: 2)
        let length = Int(header[header.startIndex]) << 8 | Int(header[header.startIndex + 1])
        guard length <= maxLength else { throw ConnectionError.frameTooLarge(length) }
        return length == 0 ? Data() : try await receive(exactly: length)
    }

    public func send(_ payload: Data) async throws {
        guard payload.count <= Self.maxFrameLength else { throw ConnectionError.frameTooLarge(payload.count) }
        var frame = Data([UInt8(payload.count >> 8), UInt8(payload.count & 0xFF)])
        frame.append(payload)
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.send(content: frame, completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: ConnectionError.failed(error.localizedDescription))
                } else {
                    continuation.resume()
                }
            })
        }
    }

    /// Closes the connection after `seconds` unless the deadline is moved or cleared first.
    public func setDeadline(_ seconds: TimeInterval?) {
        queue.async { [self] in
            deadline?.cancel()
            deadline = nil
            guard let seconds else { return }
            let item = DispatchWorkItem { [connection] in connection.cancel() }
            deadline = item
            queue.asyncAfter(deadline: .now() + seconds, execute: item)
        }
    }

    public func close() {
        queue.async { [self] in
            deadline?.cancel()
            deadline = nil
        }
        connection.cancel()
    }

    /// Closes after our last frame is surely readable: sends FIN, then reads
    /// and drops whatever the peer still sends until it closes too (or
    /// `timeout`). Closing outright with unread data in our receive buffer
    /// would make the kernel send a reset, and the peer could lose our last
    /// frame, such as an error.
    public func closeGracefully(timeout: TimeInterval = 2) {
        connection.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .idempotent)
        setDeadline(timeout)
        drain()
    }

    private func drain() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [self] _, _, isComplete, error in
            if isComplete || error != nil {
                close()
            } else {
                drain()
            }
        }
    }

    /// Our end's IP address, once connected.
    public var localAddress: String? { Self.host(of: connection.currentPath?.localEndpoint) }
    public var remoteAddress: String? { Self.host(of: connection.currentPath?.remoteEndpoint ?? connection.endpoint) }

    static func host(of endpoint: NWEndpoint?) -> String? {
        guard case let .hostPort(host, _)? = endpoint else { return nil }
        switch host {
        case .ipv4(let address): return "\(address)"
        case .ipv6(let address): return "\(address)".components(separatedBy: "%").first
        case .name(let name, _): return name
        @unknown default: return nil
        }
    }

    private func receive(exactly count: Int) async throws -> Data {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Data, Error>) in
            connection.receive(minimumIncompleteLength: count, maximumLength: count) { data, _, _, error in
                if let error {
                    continuation.resume(throwing: ConnectionError.failed(error.localizedDescription))
                } else if let data, data.count == count {
                    continuation.resume(returning: data)
                } else {
                    continuation.resume(throwing: ConnectionError.closed)
                }
            }
        }
    }
}
