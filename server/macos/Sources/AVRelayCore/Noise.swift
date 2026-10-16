// SPDX-License-Identifier: GPL-3.0-or-later
// The Noise Protocol Framework (revision 34), the two patterns AV Relay uses:
//   Noise_IK_25519_ChaChaPoly_SHA256      sessions (docs/protocol.md, section 6)
//   Noise_XXpsk3_25519_ChaChaPoly_SHA256  pairing  (docs/protocol.md, section 5.4)
// Checked against the cacophony vectors and tests/vectors.

import CryptoKit
import Foundation

public enum NoiseError: Error, Equatable {
    case decryptionFailed
    case malformedMessage
    case wrongTurn
    case handshakeNotComplete
    case missingKey
    case invalidKey
    case nonceExhausted
    case messageTooLong
}

public enum NoisePattern: Sendable {
    case ik
    case xxpsk3

    var protocolName: String {
        switch self {
        case .ik: "Noise_IK_25519_ChaChaPoly_SHA256"
        case .xxpsk3: "Noise_XXpsk3_25519_ChaChaPoly_SHA256"
        }
    }

    enum Token { case e, s, ee, es, se, ss, psk }

    /// Tokens of the responder's pre-message (the initiator knows the responder's static key).
    var responderPreMessage: [Token] { self == .ik ? [.s] : [] }

    var messages: [[Token]] {
        switch self {
        case .ik: [[.e, .es, .s, .ss], [.e, .ee, .se]]
        case .xxpsk3: [[.e], [.e, .ee, .s, .es], [.s, .se, .psk]]
        }
    }

    var usesPSK: Bool { self == .xxpsk3 }
}

/// Noise's maximum message length.
public let noiseMaxMessageLength = 65535
let noiseTagLength = 16
let dhLength = 32

// MARK: - CipherState

struct CipherState: Sendable {
    private var key: SymmetricKey?
    private var nonce: UInt64 = 0

    init(key: SymmetricKey? = nil) { self.key = key }

    var hasKey: Bool { key != nil }

    mutating func encrypt(ad: Data, plaintext: Data) throws -> Data {
        guard let key else { return plaintext }
        guard nonce < UInt64.max else { throw NoiseError.nonceExhausted }
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: Self.nonce(nonce), authenticating: ad)
        nonce += 1
        return box.ciphertext + box.tag
    }

    mutating func decrypt(ad: Data, ciphertext: Data) throws -> Data {
        guard let key else { return ciphertext }
        guard nonce < UInt64.max else { throw NoiseError.nonceExhausted }
        guard ciphertext.count >= noiseTagLength else { throw NoiseError.decryptionFailed }
        let body = Data(ciphertext.prefix(ciphertext.count - noiseTagLength))
        let tag = Data(ciphertext.suffix(noiseTagLength))
        let plaintext: Data
        do {
            let box = try ChaChaPoly.SealedBox(nonce: Self.nonce(nonce), ciphertext: body, tag: tag)
            plaintext = try ChaChaPoly.open(box, using: key, authenticating: ad)
        } catch {
            throw NoiseError.decryptionFailed
        }
        nonce += 1
        return plaintext
    }

    /// 32 bits of zeros followed by the little-endian 64-bit counter.
    private static func nonce(_ n: UInt64) -> ChaChaPoly.Nonce {
        var bytes = Data(count: 4)
        withUnsafeBytes(of: n.littleEndian) { bytes.append(contentsOf: $0) }
        return try! ChaChaPoly.Nonce(data: bytes)
    }
}

// MARK: - SymmetricState

struct SymmetricState: Sendable {
    private(set) var ck: Data
    private(set) var h: Data
    private(set) var cipher = CipherState()

    init(protocolName: String) {
        let name = Data(protocolName.utf8)
        h = name.count <= 32 ? name + Data(count: 32 - name.count) : sha256(name)
        ck = h
    }

    mutating func mixKey(_ ikm: Data) {
        let (newCK, key) = hkdf2(ck, ikm)
        ck = newCK
        cipher = CipherState(key: SymmetricKey(data: key))
    }

    mutating func mixHash(_ data: Data) {
        h = sha256(h + data)
    }

    mutating func mixKeyAndHash(_ ikm: Data) {
        let (newCK, tempH, key) = hkdf3(ck, ikm)
        ck = newCK
        mixHash(tempH)
        cipher = CipherState(key: SymmetricKey(data: key))
    }

    mutating func encryptAndHash(_ plaintext: Data) throws -> Data {
        let ciphertext = try cipher.encrypt(ad: h, plaintext: plaintext)
        mixHash(ciphertext)
        return ciphertext
    }

    mutating func decryptAndHash(_ ciphertext: Data) throws -> Data {
        let plaintext = try cipher.decrypt(ad: h, ciphertext: ciphertext)
        mixHash(ciphertext)
        return plaintext
    }

    func split() -> (CipherState, CipherState) {
        let (k1, k2) = hkdf2(ck, Data())
        return (CipherState(key: SymmetricKey(data: k1)), CipherState(key: SymmetricKey(data: k2)))
    }
}

func sha256(_ data: Data) -> Data { Data(SHA256.hash(data: data)) }

private func hmacSHA256(_ key: Data, _ data: Data) -> Data {
    Data(HMAC<SHA256>.authenticationCode(for: data, using: SymmetricKey(data: key)))
}

private func hkdf2(_ ck: Data, _ ikm: Data) -> (Data, Data) {
    let temp = hmacSHA256(ck, ikm)
    let out1 = hmacSHA256(temp, Data([1]))
    let out2 = hmacSHA256(temp, out1 + Data([2]))
    return (out1, out2)
}

private func hkdf3(_ ck: Data, _ ikm: Data) -> (Data, Data, Data) {
    let temp = hmacSHA256(ck, ikm)
    let out1 = hmacSHA256(temp, Data([1]))
    let out2 = hmacSHA256(temp, out1 + Data([2]))
    let out3 = hmacSHA256(temp, out2 + Data([3]))
    return (out1, out2, out3)
}

// MARK: - Keys

/// An X25519 key pair.
public struct NoiseKeyPair: Sendable {
    public let privateKey: Curve25519.KeyAgreement.PrivateKey
    public var publicKey: Data { privateKey.publicKey.rawRepresentation }

    public init() { privateKey = Curve25519.KeyAgreement.PrivateKey() }

    public init(privateKey raw: Data) throws {
        guard raw.count == 32 else { throw NoiseError.invalidKey }
        privateKey = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: raw)
    }

    public var privateKeyData: Data { privateKey.rawRepresentation }

    func dh(_ remotePublic: Data) throws -> Data {
        guard remotePublic.count == dhLength else { throw NoiseError.invalidKey }
        let remote = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: remotePublic)
        let secret = try privateKey.sharedSecretFromKeyAgreement(with: remote)
        return secret.withUnsafeBytes { Data($0) }
    }
}

// MARK: - HandshakeState

/// One side of a Noise handshake. Write and read the pattern's messages in
/// turn; once `isComplete`, take `transport()`.
public struct NoiseHandshake: Sendable {
    public let pattern: NoisePattern
    public let isInitiator: Bool
    private var symmetric: SymmetricState
    private let s: NoiseKeyPair
    private var e: NoiseKeyPair?
    private let fixedEphemeral: NoiseKeyPair?
    public private(set) var remoteStaticKey: Data?
    private var re: Data?
    private let psk: Data?
    private var messageIndex = 0

    /// - Parameters:
    ///   - remoteStatic: the responder's static key, which an IK initiator must know in advance.
    ///   - psk: 32 bytes, for psk patterns.
    ///   - ephemeral: tests only (fixed test vectors); leave nil everywhere else.
    public init(pattern: NoisePattern, initiator: Bool, prologue: Data, staticKey: NoiseKeyPair,
                remoteStatic: Data? = nil, psk: Data? = nil, ephemeral: NoiseKeyPair? = nil) throws {
        self.pattern = pattern
        isInitiator = initiator
        s = staticKey
        fixedEphemeral = ephemeral
        remoteStaticKey = remoteStatic
        if pattern.usesPSK {
            guard let psk, psk.count == 32 else { throw NoiseError.missingKey }
        }
        self.psk = psk
        symmetric = SymmetricState(protocolName: pattern.protocolName)
        symmetric.mixHash(prologue)
        for token in pattern.responderPreMessage where token == .s {
            if initiator {
                guard let remoteStatic, remoteStatic.count == dhLength else { throw NoiseError.missingKey }
                symmetric.mixHash(remoteStatic)
            } else {
                symmetric.mixHash(staticKey.publicKey)
            }
        }
    }

    public var isComplete: Bool { messageIndex >= pattern.messages.count }
    public var handshakeHash: Data { symmetric.h }
    private var isMyTurn: Bool { (messageIndex % 2 == 0) == isInitiator }

    public mutating func writeMessage(payload: Data = Data()) throws -> Data {
        guard !isComplete, isMyTurn else { throw NoiseError.wrongTurn }
        var out = Data()
        for token in pattern.messages[messageIndex] {
            switch token {
            case .e:
                let ephemeral = fixedEphemeral ?? NoiseKeyPair()
                e = ephemeral
                out += ephemeral.publicKey
                symmetric.mixHash(ephemeral.publicKey)
                if pattern.usesPSK { symmetric.mixKey(ephemeral.publicKey) }
            case .s:
                out += try symmetric.encryptAndHash(s.publicKey)
            case .psk:
                symmetric.mixKeyAndHash(psk!)
            default:
                try mixDH(token)
            }
        }
        out += try symmetric.encryptAndHash(payload)
        guard out.count <= noiseMaxMessageLength else { throw NoiseError.messageTooLong }
        messageIndex += 1
        return out
    }

    public mutating func readMessage(_ message: Data) throws -> Data {
        guard !isComplete, !isMyTurn else { throw NoiseError.wrongTurn }
        guard message.count <= noiseMaxMessageLength else { throw NoiseError.messageTooLong }
        var rest = message[...]
        for token in pattern.messages[messageIndex] {
            switch token {
            case .e:
                guard rest.count >= dhLength else { throw NoiseError.malformedMessage }
                let key = Data(rest.prefix(dhLength))
                rest = rest.dropFirst(dhLength)
                re = key
                symmetric.mixHash(key)
                if pattern.usesPSK { symmetric.mixKey(key) }
            case .s:
                let length = dhLength + (symmetric.cipher.hasKey ? noiseTagLength : 0)
                guard rest.count >= length else { throw NoiseError.malformedMessage }
                let key = try symmetric.decryptAndHash(Data(rest.prefix(length)))
                rest = rest.dropFirst(length)
                remoteStaticKey = key
            case .psk:
                symmetric.mixKeyAndHash(psk!)
            default:
                try mixDH(token)
            }
        }
        let payload = try symmetric.decryptAndHash(Data(rest))
        messageIndex += 1
        return payload
    }

    /// ee, es, se, ss: the first letter is the initiator's key, the second the responder's.
    private mutating func mixDH(_ token: NoisePattern.Token) throws {
        let mine: NoiseKeyPair?
        let theirs: Data?
        switch (token, isInitiator) {
        case (.ee, _): (mine, theirs) = (e, re)
        case (.es, true): (mine, theirs) = (e, remoteStaticKey)
        case (.es, false): (mine, theirs) = (s, re)
        case (.se, true): (mine, theirs) = (s, re)
        case (.se, false): (mine, theirs) = (e, remoteStaticKey)
        case (.ss, _): (mine, theirs) = (s, remoteStaticKey)
        default: throw NoiseError.malformedMessage
        }
        guard let mine, let theirs else { throw NoiseError.missingKey }
        do {
            symmetric.mixKey(try mine.dh(theirs))
        } catch {
            throw NoiseError.invalidKey
        }
    }

    /// The transport keys, once the handshake is complete.
    public func transport() throws -> NoiseTransport {
        guard isComplete else { throw NoiseError.handshakeNotComplete }
        let (c1, c2) = symmetric.split()
        return isInitiator ? NoiseTransport(send: c1, receive: c2) : NoiseTransport(send: c2, receive: c1)
    }
}

// MARK: - Transport

/// Encrypts and decrypts transport messages after the handshake, in order.
public struct NoiseTransport: Sendable {
    private var sendCipher: CipherState
    private var receiveCipher: CipherState

    init(send: CipherState, receive: CipherState) {
        sendCipher = send
        receiveCipher = receive
    }

    /// The largest plaintext that fits one Noise message.
    public static let maxPlaintextLength = noiseMaxMessageLength - noiseTagLength

    public mutating func encrypt(_ plaintext: Data) throws -> Data {
        guard plaintext.count <= Self.maxPlaintextLength else { throw NoiseError.messageTooLong }
        return try sendCipher.encrypt(ad: Data(), plaintext: plaintext)
    }

    public mutating func decrypt(_ ciphertext: Data) throws -> Data {
        guard ciphertext.count <= noiseMaxMessageLength else { throw NoiseError.messageTooLong }
        return try receiveCipher.decrypt(ad: Data(), ciphertext: ciphertext)
    }
}
