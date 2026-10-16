// SPDX-License-Identifier: GPL-3.0-or-later
// include/RTCAudioDevice.h: libwebrtc's macOS build contains its custom
// audio device support (ObjCAudioDeviceModule, behind RTCPeerConnectionFactory's
// initWithEncoderFactory:decoderFactory:audioDevice:) but ships the
// protocol's header only for iOS. That file is the header, unchanged, from
// the iOS slice of the same WebRTC build (M155).

#import "AVRWebRTCSupport.h"
#import "RTCAudioDevice.h"

NSError *AVRSetCodecPreferences(id<RTCRtpTransceiver> transceiver, NSArray<RTCRtpCodecCapability *> *codecs) {
    NSError *error = nil;
    if ([transceiver setCodecPreferences:codecs error:&error]) {
        return nil;
    }
    return error ?: [NSError errorWithDomain:@"av-relay.webrtc" code:1 userInfo:nil];
}
