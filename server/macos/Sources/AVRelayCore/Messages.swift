// SPDX-License-Identifier: GPL-3.0-or-later
// The JSON messages of docs/protocol.md: the plaintext ones before Noise
// (sections 4 and 5) and the control messages of a session (section 7).
// Unknown message types and unknown fields are ignored.

import Foundation

// MARK: - Before Noise

public struct Preamble: Codable, Sendable, Equatable {
    public enum Mode: String, Codable, Sendable { case pair, session }
    public var proto: String
    public var version: Int
    public var mode: Mode

    public static let protocolName = "av-relay"
    public static let currentVersion = 1

    public init(mode: Mode) {
        proto = Self.protocolName
        version = Self.currentVersion
        self.mode = mode
    }
}

public struct PairStart: Codable, Sendable, Equatable {
    public var type = "pair-start"
    public var sid: Data
    public init(sid: Data) { self.sid = sid }
}

public struct CPaceMessage: Codable, Sendable, Equatable {
    public var type = "cpace"
    /// The client's half of the session id; absent in the server's message.
    public var sid: Data?
    public var share: Data

    enum CodingKeys: String, CodingKey { case type, sid, share = "Y" }

    public init(sid: Data?, share: Data) {
        self.sid = sid
        self.share = share
    }
}

/// The client's info in pairing message 3.
public struct ClientInfo: Codable, Sendable, Equatable {
    public var name: String
    public var app: String
    public init(name: String, app: String) {
        self.name = name
        self.app = app
    }
}

// MARK: - Session messages

public struct VideoFormat: Codable, Sendable, Equatable {
    public var width: Int
    public var height: Int
    public var fps: Int
    public init(width: Int, height: Int, fps: Int) {
        self.width = width
        self.height = height
        self.fps = fps
    }
}

public struct DeviceState: Codable, Sendable, Equatable {
    public enum State: String, Codable, Sendable { case off, starting, on, error }
    public enum ErrorCode: String, Codable, Sendable {
        case permissionDenied = "permission-denied", noDevice = "no-device", deviceBusy = "device-busy", failed
    }
    public var state: State
    public var device: String?
    public var width: Int?
    public var height: Int?
    public var fps: Int?
    public var error: ErrorCode?

    public init(state: State, device: String? = nil, width: Int? = nil, height: Int? = nil, fps: Int? = nil,
                error: ErrorCode? = nil) {
        self.state = state
        self.device = device
        self.width = width
        self.height = height
        self.fps = fps
        self.error = error
    }

    public static let off = DeviceState(state: .off)
}

public enum ErrorCode: String, Codable, Sendable, Error {
    case unsupportedVersion = "unsupported-version"
    case notPairing = "not-pairing"
    case busy
    case notPaired = "not-paired"
    case badMessage = "bad-message"
    case `internal`
}

public enum ByeReason: String, Codable, Sendable { case quit, unpaired, replaced }

/// A message of the protocol, by its `type`.
public enum Message: Sendable, Equatable {
    // Pairing
    case paired(name: String, deviceID: String, app: String)
    // Client to server
    case hello(name: String, app: String, video: VideoFormat?)
    case answer(sdp: String)
    case media(video: Bool, audio: Bool)
    case iceRestart
    case ping(id: Int)
    case unpair
    // Server to client
    case welcome(name: String, app: String, deviceID: String)
    case offer(sdp: String)
    case mediaState(video: DeviceState, audio: DeviceState)
    case pong(id: Int)
    // Both
    case ice(candidate: String, sdpMid: String?, sdpMLineIndex: Int?)
    case bye(reason: String)
    case error(code: String, message: String?, versions: [Int]?)
    /// A type this version doesn't know; ignored.
    case unknown(type: String)

    public var type: String {
        switch self {
        case .paired: "paired"
        case .hello: "hello"
        case .answer: "answer"
        case .media: "media"
        case .iceRestart: "ice-restart"
        case .ping: "ping"
        case .unpair: "unpair"
        case .welcome: "welcome"
        case .offer: "offer"
        case .mediaState: "media-state"
        case .pong: "pong"
        case .ice: "ice"
        case .bye: "bye"
        case .error: "error"
        case .unknown(let type): type
        }
    }

    public static func error(_ code: ErrorCode, _ message: String? = nil) -> Message {
        .error(code: code.rawValue, message: message, versions: code == .unsupportedVersion ? [Preamble.currentVersion] : nil)
    }

    public static func bye(_ reason: ByeReason) -> Message { .bye(reason: reason.rawValue) }
}

extension Message: Codable {
    enum Key: String, CodingKey {
        case type, name, app, video, audio, sdp, id, candidate, sdpMid, sdpMLineIndex, reason, code, message, versions
        case deviceID = "device_id"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "paired":
            self = .paired(name: try c.decode(String.self, forKey: .name), deviceID: try c.decode(String.self, forKey: .deviceID),
                           app: try c.decodeIfPresent(String.self, forKey: .app) ?? "")
        case "hello":
            self = .hello(name: try c.decode(String.self, forKey: .name), app: try c.decodeIfPresent(String.self, forKey: .app) ?? "",
                          video: try c.decodeIfPresent(VideoFormat.self, forKey: .video))
        case "answer": self = .answer(sdp: try c.decode(String.self, forKey: .sdp))
        case "media": self = .media(video: try c.decode(Bool.self, forKey: .video), audio: try c.decode(Bool.self, forKey: .audio))
        case "ice-restart": self = .iceRestart
        case "ping": self = .ping(id: try c.decode(Int.self, forKey: .id))
        case "unpair": self = .unpair
        case "welcome":
            self = .welcome(name: try c.decode(String.self, forKey: .name), app: try c.decodeIfPresent(String.self, forKey: .app) ?? "",
                            deviceID: try c.decode(String.self, forKey: .deviceID))
        case "offer": self = .offer(sdp: try c.decode(String.self, forKey: .sdp))
        case "media-state":
            self = .mediaState(video: try c.decode(DeviceState.self, forKey: .video), audio: try c.decode(DeviceState.self, forKey: .audio))
        case "pong": self = .pong(id: try c.decode(Int.self, forKey: .id))
        case "ice":
            self = .ice(candidate: try c.decode(String.self, forKey: .candidate),
                        sdpMid: try c.decodeIfPresent(String.self, forKey: .sdpMid),
                        sdpMLineIndex: try c.decodeIfPresent(Int.self, forKey: .sdpMLineIndex))
        case "bye": self = .bye(reason: try c.decodeIfPresent(String.self, forKey: .reason) ?? "")
        case "error":
            self = .error(code: try c.decode(String.self, forKey: .code), message: try c.decodeIfPresent(String.self, forKey: .message),
                          versions: try c.decodeIfPresent([Int].self, forKey: .versions))
        default: self = .unknown(type: type)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(type, forKey: .type)
        switch self {
        case let .paired(name, deviceID, app):
            try c.encode(name, forKey: .name)
            try c.encode(deviceID, forKey: .deviceID)
            try c.encode(app, forKey: .app)
        case let .hello(name, app, video):
            try c.encode(name, forKey: .name)
            try c.encode(app, forKey: .app)
            try c.encodeIfPresent(video, forKey: .video)
        case let .answer(sdp), let .offer(sdp):
            try c.encode(sdp, forKey: .sdp)
        case let .media(video, audio):
            try c.encode(video, forKey: .video)
            try c.encode(audio, forKey: .audio)
        case let .ping(id), let .pong(id):
            try c.encode(id, forKey: .id)
        case let .welcome(name, app, deviceID):
            try c.encode(name, forKey: .name)
            try c.encode(app, forKey: .app)
            try c.encode(deviceID, forKey: .deviceID)
        case let .mediaState(video, audio):
            try c.encode(video, forKey: .video)
            try c.encode(audio, forKey: .audio)
        case let .ice(candidate, sdpMid, sdpMLineIndex):
            try c.encode(candidate, forKey: .candidate)
            try c.encodeIfPresent(sdpMid, forKey: .sdpMid)
            try c.encodeIfPresent(sdpMLineIndex, forKey: .sdpMLineIndex)
        case let .bye(reason):
            try c.encode(reason, forKey: .reason)
        case let .error(code, message, versions):
            try c.encode(code, forKey: .code)
            try c.encodeIfPresent(message, forKey: .message)
            try c.encodeIfPresent(versions, forKey: .versions)
        case .iceRestart, .unpair, .unknown:
            break
        }
    }
}

// MARK: - JSON

public enum JSON {
    public static func encode<T: Encodable>(_ value: T) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        return try encoder.encode(value)
    }

    public static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        try JSONDecoder().decode(type, from: data)
    }

    /// The `type` field of a JSON object, if it has one.
    public static func messageType(of data: Data) -> String? {
        struct Typed: Decodable { var type: String }
        return try? JSONDecoder().decode(Typed.self, from: data).type
    }
}
