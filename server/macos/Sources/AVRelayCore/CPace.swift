// SPDX-License-Identifier: GPL-3.0-or-later
// CPace (draft-irtf-cfrg-cpace-21), suite CPACE-RISTR255-SHA512, in the
// initiator-responder setting, plus the AV Relay pairing parameters
// (docs/protocol.md, section 5.3). Checked against tests/vectors.

import Clibsodium
import CryptoKit
import Foundation

public enum CPaceError: Error, Equatable {
    /// The peer's point doesn't decode, or the shared point is the identity: abort.
    case invalidPeerPoint
    case badInput
}

public enum CPace {
    static let dsi = Data("CPaceRistretto255".utf8)
    static let sha512BlockSize = 128
    public static let pointLength = 32
    public static let scalarLength = 32

    private static let sodiumReady: Bool = sodium_init() >= 0

    // MARK: String helpers (draft appendix A)

    static func prependLen(_ data: Data) -> Data {
        var length = data.count
        var out = Data()
        repeat {
            out.append(length < 128 ? UInt8(length) : UInt8(length & 0x7F) | 0x80)
            length >>= 7
        } while length != 0
        return out + data
    }

    static func lvCat(_ parts: Data...) -> Data { lvCat(parts) }

    static func lvCat(_ parts: [Data]) -> Data {
        parts.reduce(into: Data()) { $0 += prependLen($1) }
    }

    static func generatorString(prs: Data, ci: Data, sid: Data) -> Data {
        let zpad = max(0, sha512BlockSize - 1 - prependLen(prs).count - prependLen(dsi).count)
        return lvCat(dsi, prs, Data(count: zpad), ci, sid)
    }

    // MARK: Group operations

    /// The generator g (encoded), from the password, channel identifier and session id.
    static func generator(prs: Data, ci: Data, sid: Data) -> Data {
        precondition(sodiumReady)
        let hash = Data(SHA512.hash(data: generatorString(prs: prs, ci: ci, sid: sid)))
        var point = Data(count: pointLength)
        point.withUnsafeMutableBytes { p in
            hash.withUnsafeBytes { h in
                _ = crypto_core_ristretto255_from_hash(p.bindMemory(to: UInt8.self).baseAddress!,
                                                       h.bindMemory(to: UInt8.self).baseAddress!)
            }
        }
        return point
    }

    /// 32 random bytes with the bits above 252 cleared (draft section 8.3).
    static func sampleScalar() -> Data {
        precondition(sodiumReady)
        var scalar = Data(count: scalarLength)
        scalar.withUnsafeMutableBytes { randombytes_buf($0.baseAddress!, scalarLength) }
        scalar[scalarLength - 1] &= 0x0F
        return scalar
    }

    /// encode(y · decode(point)), or nil when the point doesn't decode or the result is the identity.
    static func scalarMult(_ scalar: Data, _ point: Data) -> Data? {
        precondition(sodiumReady)
        guard scalar.count == scalarLength, point.count == pointLength else { return nil }
        var out = Data(count: pointLength)
        let status = out.withUnsafeMutableBytes { q in
            scalar.withUnsafeBytes { n in
                point.withUnsafeBytes { p in
                    crypto_scalarmult_ristretto255(q.bindMemory(to: UInt8.self).baseAddress!,
                                                   n.bindMemory(to: UInt8.self).baseAddress!,
                                                   p.bindMemory(to: UInt8.self).baseAddress!)
                }
            }
        }
        return status == 0 ? out : nil
    }

    /// ISK = SHA-512(lv_cat(DSI || "_ISK", sid, K) || transcript_ir(Ya, ADa, Yb, ADb)).
    static func isk(sid: Data, k: Data, ya: Data, ada: Data, yb: Data, adb: Data) -> Data {
        let input = lvCat(dsi + Data("_ISK".utf8), sid, k) + lvCat(ya, ada) + lvCat(yb, adb)
        return Data(SHA512.hash(data: input))
    }

    // MARK: One party

    /// One side of a CPace run. The initiator (A) is the client; the
    /// responder (B) is the Mac. A scalar is used for one run only.
    public struct Party: Sendable {
        public let isInitiator: Bool
        private let scalar: Data
        private let sid: Data
        private let ad: Data
        /// This party's public share (Ya or Yb), to send to the peer.
        public let share: Data

        public init(initiator: Bool, prs: Data, ci: Data, sid: Data, ad: Data = Data()) {
            self.init(initiator: initiator, prs: prs, ci: ci, sid: sid, ad: ad, scalar: CPace.sampleScalar())
        }

        /// Tests only (fixed vectors): a given scalar.
        public init(initiator: Bool, prs: Data, ci: Data, sid: Data, ad: Data = Data(), scalar: Data) {
            isInitiator = initiator
            self.scalar = scalar
            self.sid = sid
            self.ad = ad
            let g = CPace.generator(prs: prs, ci: ci, sid: sid)
            // A fresh scalar times a valid generator is never the identity in practice.
            share = CPace.scalarMult(scalar, g) ?? Data(count: CPace.pointLength)
        }

        /// The intermediate session key, from the peer's share and associated data.
        public func isk(peerShare: Data, peerAD: Data = Data()) throws -> Data {
            guard let k = CPace.scalarMult(scalar, peerShare) else { throw CPaceError.invalidPeerPoint }
            return isInitiator
                ? CPace.isk(sid: sid, k: k, ya: share, ada: ad, yb: peerShare, adb: peerAD)
                : CPace.isk(sid: sid, k: k, ya: peerShare, ada: peerAD, yb: share, adb: ad)
        }
    }
}

// MARK: - AV Relay pairing parameters

public enum PairingCrypto {
    /// CI = lv_cat("av-relay/1 pairing", "client", "server").
    public static let channelIdentifier = CPace.lvCat(Data("av-relay/1 pairing".utf8), Data("client".utf8),
                                                      Data("server".utf8))
    public static let sidHalfLength = 16
    static let pskInfo = Data("av-relay/1 pairing psk".utf8)
    static let pairPrologueLabel = Data("av-relay/1 pair".utf8)
    public static let sessionPrologue = Data("av-relay/1 session".utf8)

    /// The code as the CPace password: 6 ASCII digits.
    public static func prs(code: PairingCode) -> Data { Data(code.digits.utf8) }

    /// psk = HKDF-SHA256(salt: empty, IKM: ISK, info: "av-relay/1 pairing psk"), 32 bytes.
    public static func psk(isk: Data) -> Data {
        HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: isk), salt: Data(), info: pskInfo,
                               outputByteCount: 32).withUnsafeBytes { Data($0) }
    }

    /// The Noise prologue for pairing: "av-relay/1 pair" || sid || Ya || Yb.
    public static func pairPrologue(sid: Data, ya: Data, yb: Data) -> Data {
        pairPrologueLabel + sid + ya + yb
    }

    public static func randomSidHalf() -> Data {
        var bytes = Data(count: sidHalfLength)
        bytes.withUnsafeMutableBytes { randombytes_buf($0.baseAddress!, sidHalfLength) }
        return bytes
    }
}

/// A 6-digit pairing code: uniform over 000000–999999, from the system CSPRNG.
public struct PairingCode: Sendable, Equatable, CustomStringConvertible {
    public let digits: String

    public init?(_ digits: String) {
        guard digits.count == 6, digits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        self.digits = digits
    }

    public static func random() -> PairingCode {
        var rng = SystemRandomNumberGenerator()
        return PairingCode(String(format: "%06u", UInt32.random(in: 0..<1_000_000, using: &rng)))!
    }

    /// As shown to the user: "042 137".
    public var display: String { "\(digits.prefix(3)) \(digits.suffix(3))" }
    public var description: String { display }
}

/// The device ID: the first 8 bytes of SHA-256 over the static public key, as 16 hex digits.
public func deviceID(publicKey: Data) -> String {
    sha256(publicKey).prefix(8).map { String(format: "%02x", $0) }.joined()
}
