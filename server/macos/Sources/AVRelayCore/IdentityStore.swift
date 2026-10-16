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
    /// Only a missing file means "first use": a file that exists but can't
    /// be read is an error, never a reason to make a new key.
    public init(directory: URL = IdentityStore.defaultDirectory) throws {
        self.directory = directory
        let fm = FileManager.default
        try fm.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try fm.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)

        let keyPath = directory.appendingPathComponent("identity.key")
        if let raw = try Self.readIfPresent(keyPath) {
            identity = try NoiseKeyPair(privateKey: raw)
            try fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyPath.path)
        } else {
            let fresh = NoiseKeyPair()
            // Another instance may have created it meanwhile: then use that one.
            if try Self.createExclusively(fresh.privateKeyData, at: keyPath) {
                identity = fresh
            } else {
                identity = try NoiseKeyPair(privateKey: try Data(contentsOf: keyPath))
            }
        }
        if let data = try Self.readIfPresent(directory.appendingPathComponent("paired-clients.json")) {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            clients = try decoder.decode([PairedClient].self, from: data)
        } else {
            clients = []
        }
    }

    private static func readIfPresent(_ url: URL) throws -> Data? {
        do {
            return try Data(contentsOf: url)
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return nil
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

    /// Writes a file that only the user can read, atomically: a 0600 temp
    /// file from the start, synced, then renamed over the old one.
    private static func writePrivately(_ data: Data, to url: URL) throws {
        let temp = try writeTemp(data, next: url)
        guard rename(temp.path, url.path) == 0 else {
            let error = POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
            unlink(temp.path)
            throw error
        }
    }

    /// Creates the file with this content unless it exists; false if it did.
    private static func createExclusively(_ data: Data, at url: URL) throws -> Bool {
        let temp = try writeTemp(data, next: url)
        defer { unlink(temp.path) }
        // link() fails if the name exists, so the file appears whole or not at all.
        if link(temp.path, url.path) == 0 { return true }
        if errno == EEXIST { return false }
        throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
    }

    private static func writeTemp(_ data: Data, next url: URL) throws -> URL {
        let temp = url.deletingLastPathComponent().appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString)")
        let fd = open(temp.path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
        defer { close(fd) }
        let written = data.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        guard written == data.count, fsync(fd) == 0 else {
            let error = POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
            unlink(temp.path)
            throw error
        }
        return temp
    }
}
