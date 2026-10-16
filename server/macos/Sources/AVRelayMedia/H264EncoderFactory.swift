// SPDX-License-Identifier: GPL-3.0-or-later
// H.264 only, encoded by VideoToolbox (libwebrtc's RTCVideoEncoderH264),
// with the level raised to 5.1.
//
// libwebrtc caps the encoder's frame rate to what the negotiated level
// allows at the frame size, and the level comes from the client's SDP
// answer. An answer that just echoes the offer's level 3.1 would hold
// 1080p to about 13 fps. The client must decode up to level 5.1
// (docs/protocol.md, section 8.2), so the Mac encodes at that level
// whatever the answer says.

import WebRTC

final class H264EncoderFactory: NSObject, RTCVideoEncoderFactory {
    static let level5_1: UInt8 = 0x33
    private let h264 = RTCVideoEncoderFactoryH264()

    func supportedCodecs() -> [RTCVideoCodecInfo] {
        h264.supportedCodecs()
    }

    func createEncoder(_ info: RTCVideoCodecInfo) -> RTCVideoEncoder? {
        RTCVideoEncoderH264(codecInfo: Self.raisingLevel(of: info))
    }

    /// The same codec with profile-level-id's level at least 5.1.
    static func raisingLevel(of info: RTCVideoCodecInfo) -> RTCVideoCodecInfo {
        var parameters = info.parameters
        if let id = parameters["profile-level-id"], id.count == 6, let level = UInt8(id.suffix(2), radix: 16),
           level < level5_1 {
            parameters["profile-level-id"] = String(id.prefix(4)) + String(format: "%02x", level5_1)
        }
        return RTCVideoCodecInfo(name: info.name, parameters: parameters)
    }
}
