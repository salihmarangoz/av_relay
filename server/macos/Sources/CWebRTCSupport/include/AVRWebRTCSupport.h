// SPDX-License-Identifier: GPL-3.0-or-later
// Small helpers for libwebrtc calls that don't import well into Swift.

#import <Foundation/Foundation.h>
#import <WebRTC/RTCRtpCodecCapability.h>
#import <WebRTC/RTCRtpTransceiver.h>

NS_ASSUME_NONNULL_BEGIN

/// [transceiver setCodecPreferences:codecs error:]: Swift sees two
/// overloads with the same name (one deprecated) and picks the wrong one.
/// Returns nil on success.
NSError *_Nullable AVRSetCodecPreferences(id<RTCRtpTransceiver> transceiver, NSArray<RTCRtpCodecCapability *> *codecs);

/// How many earlier Opus frames each RED audio packet carries (1 to 9;
/// libwebrtc's default is 1). Process-wide: call it before creating the
/// first RTCPeerConnectionFactory. It sets libwebrtc's field trial
/// WebRTC-Audio-Red-For-Opus, which the Objective-C API can only set
/// through a deprecated global (its replacement is C++ only).
void AVRSetAudioRedundancy(NSInteger frames);

NS_ASSUME_NONNULL_END
