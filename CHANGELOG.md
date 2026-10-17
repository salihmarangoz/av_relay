# Changelog

What changed in each release of AV Relay. The sender and the receiver are
released together, with one version number. A sender and a receiver work
together when they speak the same protocol version; an entry here says so
when that changes.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

The first release.

### Added

- **Sender for macOS** (macOS 14 or newer, Apple silicon and Intel): a menu
  bar app that shares the Mac's camera and microphone, only while an app on
  the receiver uses them. Choose which camera and microphone, start at login,
  switch it off and on from the menu. Before the Mac sleeps it ends the
  session and closes the camera and microphone.
- **Receiver for Ubuntu 24.04**: a tray app that shows the sender's camera
  and microphone as **AV Relay Camera** (v4l2loopback) and **AV Relay
  Microphone** (PipeWire), which is the default input while the sender is
  connected. Your previous default input comes back when it goes away. When
  the sender drops off, both stay for 5 minutes with a "Waiting for …"
  picture, or for good with **Keep virtual devices on**. Camera quality
  1080p or 720p, start at login, switch it off and on from the menu.
- **Pairing** with a 6-digit code shown on the sender: pick the sender from
  those found on the network, or type its address. After that the two find
  each other on their own (Bonjour).
- **Media over WebRTC** on the local network: H.264 video (hardware-encoded
  on the Mac, up to 1080p at 30 fps) and Opus audio, with congestion
  control, retransmission and keyframe requests. Over Wi-Fi the receiver
  keeps a buffer that grows when the network stalls, so sound stays smooth.
- **Security**: the control channel is Noise IK with the keys from pairing
  (CPace, then Noise XXpsk3); media is DTLS-SRTP, authenticated through it.
  No account, no cloud.
- Protocol version 1 (`av-relay/1`, docs/protocol.md).

[Unreleased]: https://github.com/salihmarangoz/av_relay/commits/main
