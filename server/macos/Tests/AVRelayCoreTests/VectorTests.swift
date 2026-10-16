// SPDX-License-Identifier: GPL-3.0-or-later
// The shared vectors in tests/vectors (docs/protocol.md, section 13).

import Foundation
import Testing
@testable import AVRelayCore

enum Vectors {
    static let directory = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
        .appendingPathComponent("tests/vectors")

    static func load(_ name: String) throws -> [String: Any] {
        let data = try Data(contentsOf: directory.appendingPathComponent(name))
        return try JSONSerialization.jsonObject(with: data) as! [String: Any]
    }
}

extension Data {
    init(hex: String) {
        var bytes = [UInt8]()
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            bytes.append(UInt8(hex[index..<next], radix: 16)!)
            index = next
        }
        self.init(bytes)
    }

    var hex: String { map { String(format: "%02x", $0) }.joined() }
}

extension Dictionary where Key == String, Value == Any {
    func hex(_ key: String) -> Data { Data(hex: self[key] as! String) }
    func dict(_ key: String) -> [String: Any] { self[key] as! [String: Any] }
}

@Suite struct CPaceVectors {
    @Test func draftAppendixB3() throws {
        let v = try Vectors.load("cpace-draft.json").dict("vectors")
        let (prs, ci, sid) = (v.hex("PRS"), v.hex("CI"), v.hex("sid"))
        #expect(CPace.generator(prs: prs, ci: ci, sid: sid) == v.hex("g"))
        let a = CPace.Party(initiator: true, prs: prs, ci: ci, sid: sid, ad: v.hex("ADa"), scalar: v.hex("ya"))
        let b = CPace.Party(initiator: false, prs: prs, ci: ci, sid: sid, ad: v.hex("ADb"), scalar: v.hex("yb"))
        #expect(a.share == v.hex("Ya"))
        #expect(b.share == v.hex("Yb"))
        #expect(CPace.scalarMult(v.hex("ya"), v.hex("Yb")) == v.hex("K"))
        #expect(try a.isk(peerShare: b.share, peerAD: v.hex("ADb")) == v.hex("ISK_IR"))
        #expect(try b.isk(peerShare: a.share, peerAD: v.hex("ADa")) == v.hex("ISK_IR"))
        #expect(CPace.scalarMult(v.hex("valid_s"), v.hex("valid_X")) == v.hex("valid_sX"))
    }

    @Test func invalidPointsAbort() throws {
        let v = try Vectors.load("cpace-draft.json").dict("vectors")
        #expect(CPace.scalarMult(v.hex("valid_s"), v.hex("invalid_Y1")) == nil)
        #expect(CPace.scalarMult(v.hex("valid_s"), v.hex("invalid_Y2")) == nil)
        let a = CPace.Party(initiator: true, prs: v.hex("PRS"), ci: v.hex("CI"), sid: v.hex("sid"))
        #expect(throws: CPaceError.invalidPeerPoint) { try a.isk(peerShare: v.hex("invalid_Y1")) }
        #expect(throws: CPaceError.invalidPeerPoint) { try a.isk(peerShare: v.hex("invalid_Y2")) }
    }

    @Test func stringHelpers() {
        #expect(CPace.prependLen(Data()) == Data([0]))
        #expect(CPace.prependLen(Data(0..<128)).prefix(2) == Data([0x80, 0x01]))
        #expect(CPace.lvCat(Data("1234".utf8), Data("5".utf8), Data(), Data("678".utf8)) == Data(hex: "043132333401350003363738"))
    }
}

@Suite struct NoiseVectors {
    @Test func cacophony() throws {
        let vectors = try Vectors.load("noise-cacophony.json")["vectors"] as! [[String: Any]]
        #expect(vectors.count == 2)
        for v in vectors {
            let pattern: NoisePattern = v["protocol_name"] as! String == "Noise_IK_25519_ChaChaPoly_SHA256" ? .ik : .xxpsk3
            let psk = (v["init_psks"] as? [String])?.first.map { Data(hex: $0) }
            var initiator = try NoiseHandshake(
                pattern: pattern, initiator: true, prologue: v.hex("init_prologue"),
                staticKey: try NoiseKeyPair(privateKey: v.hex("init_static")),
                remoteStatic: v["init_remote_static"].map { Data(hex: $0 as! String) }, psk: psk,
                ephemeral: try NoiseKeyPair(privateKey: v.hex("init_ephemeral")))
            var responder = try NoiseHandshake(
                pattern: pattern, initiator: false, prologue: v.hex("resp_prologue"),
                staticKey: try NoiseKeyPair(privateKey: v.hex("resp_static")), psk: psk,
                ephemeral: try NoiseKeyPair(privateKey: v.hex("resp_ephemeral")))
            var transports: (NoiseTransport, NoiseTransport)?
            for (index, m) in (v["messages"] as! [[String: Any]]).enumerated() {
                let payload = m.hex("payload")
                let initiatorWrites = index % 2 == 0
                let out: Data
                if var t = transports {
                    if initiatorWrites {
                        out = try t.0.encrypt(payload)
                        #expect(try t.1.decrypt(out) == payload)
                    } else {
                        out = try t.1.encrypt(payload)
                        #expect(try t.0.decrypt(out) == payload)
                    }
                    transports = t
                } else if initiatorWrites {
                    out = try initiator.writeMessage(payload: payload)
                    #expect(try responder.readMessage(out) == payload)
                } else {
                    out = try responder.writeMessage(payload: payload)
                    #expect(try initiator.readMessage(out) == payload)
                }
                #expect(out == m.hex("ciphertext"), "\(pattern) message \(index)")
                if transports == nil, initiator.isComplete, responder.isComplete {
                    transports = (try initiator.transport(), try responder.transport())
                    if let hash = v["handshake_hash"] as? String {
                        #expect(initiator.handshakeHash == Data(hex: hash))
                    }
                }
            }
        }
    }
}

@Suite struct ProtocolVectors {
    @Test func pairing() throws {
        let v = try Vectors.load("pairing.json")
        let c = v.dict("cpace")
        let code = PairingCode(v["code"] as! String)!
        #expect(PairingCrypto.prs(code: code) == c.hex("PRS"))
        #expect(PairingCrypto.channelIdentifier == c.hex("CI"))
        #expect(c.hex("sid") == c.hex("sid_c") + c.hex("sid_s"))
        #expect(CPace.generatorString(prs: c.hex("PRS"), ci: c.hex("CI"), sid: c.hex("sid")) == c.hex("generator_string"))
        let client = CPace.Party(initiator: true, prs: c.hex("PRS"), ci: c.hex("CI"), sid: c.hex("sid"), scalar: c.hex("ya"))
        let server = CPace.Party(initiator: false, prs: c.hex("PRS"), ci: c.hex("CI"), sid: c.hex("sid"), scalar: c.hex("yb"))
        #expect(client.share == c.hex("Ya"))
        #expect(server.share == c.hex("Yb"))
        let isk = try server.isk(peerShare: client.share)
        #expect(isk == c.hex("ISK"))
        #expect(try client.isk(peerShare: server.share) == isk)
        #expect(PairingCrypto.psk(isk: isk) == v.hex("psk"))

        let wrongServer = CPace.Party(initiator: false, prs: Data("042138".utf8), ci: c.hex("CI"), sid: c.hex("sid"), scalar: c.hex("yb"))
        #expect(try client.isk(peerShare: wrongServer.share) == c.hex("ISK_if_server_code_were_042138"))

        let n = v.dict("noise")
        let prologue = PairingCrypto.pairPrologue(sid: c.hex("sid"), ya: client.share, yb: server.share)
        #expect(prologue == n.hex("prologue"))
        var initiator = try NoiseHandshake(pattern: .xxpsk3, initiator: true, prologue: prologue,
                                           staticKey: try NoiseKeyPair(privateKey: n.hex("client_static_private")),
                                           psk: v.hex("psk"), ephemeral: try NoiseKeyPair(privateKey: n.hex("client_ephemeral_private")))
        var responder = try NoiseHandshake(pattern: .xxpsk3, initiator: false, prologue: prologue,
                                           staticKey: try NoiseKeyPair(privateKey: n.hex("server_static_private")),
                                           psk: v.hex("psk"), ephemeral: try NoiseKeyPair(privateKey: n.hex("server_ephemeral_private")))
        let m1 = try initiator.writeMessage()
        #expect(m1 == n.hex("msg1"))
        _ = try responder.readMessage(m1)
        let m2 = try responder.writeMessage()
        #expect(m2 == n.hex("msg2"))
        _ = try initiator.readMessage(m2)
        let info = Data((n["msg3_payload"] as! String).utf8)
        let m3 = try initiator.writeMessage(payload: info)
        #expect(m3 == n.hex("msg3"))
        #expect(try JSON.decode(ClientInfo.self, from: try responder.readMessage(m3)) == ClientInfo(name: "ubuntu-desktop", app: "av-relay 0.1.0 linux"))
        #expect(responder.remoteStaticKey == n.hex("client_static_public"))
        #expect(initiator.remoteStaticKey == n.hex("server_static_public"))
        #expect(initiator.handshakeHash == n.hex("handshake_hash"))
        var serverTransport = try responder.transport()
        var clientTransport = try initiator.transport()
        let paired = Data((n["transport_s2c_0_plaintext"] as! String).utf8)
        let t1 = try serverTransport.encrypt(paired)
        #expect(t1 == n.hex("transport_s2c_0"))
        let decoded = try JSON.decode(Message.self, from: try clientTransport.decrypt(t1))
        #expect(decoded == .paired(name: "MacBook Air", deviceID: (v.dict("device_id")["server"] as! String), app: "AV Relay 0.1.0 macOS"))

        let ids = v.dict("device_id")
        #expect(deviceID(publicKey: n.hex("client_static_public")) == ids["client"] as! String)
        #expect(deviceID(publicKey: n.hex("server_static_public")) == ids["server"] as! String)
    }

    @Test func pairingWrongCodeFailsAtMessage3() throws {
        let v = try Vectors.load("pairing.json")
        let c = v.dict("cpace")
        let n = v.dict("noise")
        let wrongPSK = PairingCrypto.psk(isk: c.hex("ISK_if_server_code_were_042138"))
        var initiator = try NoiseHandshake(pattern: .xxpsk3, initiator: true, prologue: n.hex("prologue"),
                                           staticKey: try NoiseKeyPair(privateKey: n.hex("client_static_private")), psk: v.hex("psk"))
        var responder = try NoiseHandshake(pattern: .xxpsk3, initiator: false, prologue: n.hex("prologue"),
                                           staticKey: try NoiseKeyPair(privateKey: n.hex("server_static_private")), psk: wrongPSK)
        _ = try responder.readMessage(try initiator.writeMessage())
        _ = try initiator.readMessage(try responder.writeMessage())
        let m3 = try initiator.writeMessage(payload: Data("{}".utf8))
        #expect(throws: NoiseError.decryptionFailed) { _ = try responder.readMessage(m3) }
    }

    @Test func session() throws {
        let v = try Vectors.load("session.json")
        let n = v.dict("noise")
        #expect(PairingCrypto.sessionPrologue == n.hex("prologue"))
        var initiator = try NoiseHandshake(pattern: .ik, initiator: true, prologue: n.hex("prologue"),
                                           staticKey: try NoiseKeyPair(privateKey: n.hex("client_static_private")),
                                           remoteStatic: n.hex("server_static_public"),
                                           ephemeral: try NoiseKeyPair(privateKey: n.hex("client_ephemeral_private")))
        var responder = try NoiseHandshake(pattern: .ik, initiator: false, prologue: n.hex("prologue"),
                                           staticKey: try NoiseKeyPair(privateKey: n.hex("server_static_private")),
                                           ephemeral: try NoiseKeyPair(privateKey: n.hex("server_ephemeral_private")))
        let m1 = try initiator.writeMessage()
        #expect(m1 == n.hex("msg1"))
        _ = try responder.readMessage(m1)
        #expect(responder.remoteStaticKey == n.hex("client_static_public"))
        let m2 = try responder.writeMessage()
        #expect(m2 == n.hex("msg2"))
        _ = try initiator.readMessage(m2)
        #expect(initiator.handshakeHash == n.hex("handshake_hash"))
        var client = try initiator.transport()
        var server = try responder.transport()
        let hello = Data((n["transport_c2s_0_plaintext"] as! String).utf8)
        let t0 = try client.encrypt(hello)
        #expect(t0 == n.hex("transport_c2s_0"))
        #expect(try JSON.decode(Message.self, from: try server.decrypt(t0))
                == .hello(name: "ubuntu-desktop", app: "av-relay 0.1.0 linux", video: VideoFormat(width: 1280, height: 720, fps: 30)))
        let welcome = Data((n["transport_s2c_0_plaintext"] as! String).utf8)
        let t1 = try server.encrypt(welcome)
        #expect(t1 == n.hex("transport_s2c_0"))
        guard case .welcome(_, _, let id) = try JSON.decode(Message.self, from: try client.decrypt(t1)) else {
            Issue.record("not a welcome")
            return
        }
        #expect(id == deviceID(publicKey: n.hex("server_static_public")))
    }

    @Test func wireFramesHaveLengthPrefixes() throws {
        for name in ["pairing.json", "session.json"] {
            for frame in try Vectors.load(name)["wire"] as! [[String: Any]] {
                let bytes = frame.hex("frame")
                #expect(Int(bytes[0]) << 8 | Int(bytes[1]) == bytes.count - 2)
            }
        }
    }
}
