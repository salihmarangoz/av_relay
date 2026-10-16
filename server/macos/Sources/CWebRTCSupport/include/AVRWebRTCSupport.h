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

NS_ASSUME_NONNULL_END
