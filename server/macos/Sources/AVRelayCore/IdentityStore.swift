// SPDX-License-Identifier: GPL-3.0-or-later
// The Mac's static key and its list of paired clients (docs/protocol.md, section 2).
//
// Both live in files in the app's support folder (mode 0700), the key
// readable only by the user (mode 0600), like an SSH key. Not the Keychain:
// releases are ad-hoc signed, every update changes the signature, and the
// Keychain would ask the user to allow access again after each update.

import Foundation

public struct PairedClient: Codable, Sendable, Equatable, Identifiable {
    public var publicKey: Data
    public var name: String
    public var deviceID: String
    public var pairedAt: Date
    public var id: String { deviceID }

    public init(publicKey: Data, name: String, pairedAt: Date = Date()) {
        self.publicKey = publicKey
        self.name = name
        deviceID = AVRelayCore.deviceID(publicKey: publicKey)
        // Whole seconds: that's what the file keeps (ISO 8601).
        self.pairedAt = Date(timeIntervalSince1970: pairedAt.timeIntervalSince1970.rounded(.down))
    }
}

public final class IdentityStore: @unchecked Sendable {
    public let directory: URL
    public let identity: NoiseKeyPair
    public var deviceID: String { AVRelayCore.deviceID(publicKey: identity.publicKey) }

    private let lock = NSLock()
    private var clients: [PairedClient]

    private var keyFile: URL { directory.appendingPathComponent("identity.key") }
    private var clientsFile: URL { directory.appendingPathComponent("paired-clients.json") }

    /// The default folder: ~/Library/Application Support/AV Relay.
    public static var defaultDirectory: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("AV Relay", isDirectory: true)
    }

    /// Loads the key and the paired clients, creating the key on first use.
    public init(directory: URL = IdentityStore.defaultDirectory) throws {
        self.directory = directory
        let fm = FileManager.default
        try fm.createDirectory(at: directory, withIntermediateDirectories: true,
                               attributes: [.posixPermissions: 0o700])
        let keyPath = directory.appendingPathComponent("identity.key")
        if let raw = fm.contents(atPath: keyPath.path) {
            identity = try NoiseKeyPair(privateKey: raw)
        } else {
            let fresh = NoiseKeyPair()
            try Self.writePrivately(fresh.privateKeyData, to: keyPath)
            identity = fresh
        }
        let clientsPath = directory.appendingPathComponent("paired-clients.json")
        if let data = fm.contents(atPath: clientsPath.path) {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            clients = try decoder.decode([PairedClient].self, from: data)
        } else {
            clients = []
        }
    }

    public var pairedClients: [PairedClient] {
        lock.withLock { clients }
    }

    public func client(publicKey: Data) -> PairedClient? {
        lock.withLock { clients.first { $0.publicKey == publicKey } }
    }

    /// Adds a client, or replaces the entry with the same key.
    public func add(_ client: PairedClient) throws {
        try lock.withLock {
            clients.removeAll { $0.publicKey == client.publicKey }
            clients.append(client)
            try saveClients()
        }
    }

    @discardableResult
    public func remove(publicKey: Data) throws -> PairedClient? {
        try lock.withLock {
            guard let index = clients.firstIndex(where: { $0.publicKey == publicKey }) else { return nil }
            let removed = clients.remove(at: index)
            try saveClients()
            return removed
        }
    }

    private func saveClients() throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        encoder.dateEncodingStrategy = .iso8601
        try Self.writePrivately(try encoder.encode(clients), to: clientsFile)
    }

    /// Writes a file that only the user can read, atomically.
    private static func writePrivately(_ data: Data, to url: URL) throws {
        let temp = url.deletingLastPathComponent().appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString)")
        guard FileManager.default.createFile(atPath: temp.path, contents: data, attributes: [.posixPermissions: 0o600]) else {
            throw CocoaError(.fileWriteUnknown, userInfo: [NSFilePathErrorKey: temp.path])
        }
        guard rename(temp.path, url.path) == 0 else {
            let error = POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
            try? FileManager.default.removeItem(at: temp)
            throw error
        }
    }
}
