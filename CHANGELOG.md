# Changelog

What changed in each release of AV Relay. The sender and the receiver are
released together, with one version number. A sender and a receiver work
together when they speak the same protocol version; an entry here says so
when that changes.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Sender: when the camera starts again while the microphone stays on, video
  comes back at full quality at once, instead of 320x180 at 10–15 fps for
  about 20 seconds.

## [0.3.0] - 2026-10-17

### Added

- Initial Windows receiver in Rust with native settings and tray controls,
  code pairing, automatic reconnect and the Ubuntu receiver's shared
  protocol and GStreamer media implementation. Video uses an installed
  OBS Virtual Camera; microphone audio uses a separately installed virtual
  cable. Windows identity keys are protected by DPAPI.
- Windows installer with bundled GStreamer and Visual C++ runtime, Start menu
  and optional desktop shortcuts, optional start at login, and an uninstaller.
  GitHub Actions tests and publishes the installer and SHA-256 checksum.
- Windows settings and pairing pages follow the Ubuntu client, with the same
  app logo, four tray states, theme-aware tray icons, and 720p/1080p presets.

### Fixed

- Windows: sender camera capture now follows actual OBS Virtual Camera readers.
  Ending camera use can stop the sender camera while microphone capture continues.
  The microphone switch remains manual in this initial release.

## [0.2.1] - 2026-10-17

### Fixed

- Receiver: while the sender is away, the camera's picture says "Waiting for
  connection…" instead of showing the sender's name to everyone in the call.

## [0.2.0] - 2026-10-17

### Added

- Receiver: **Start minimized**, on by default. At login a paired AV Relay
  shows only its icon in the top bar and opens no window; opening it from
  the app grid still shows the window.

### Fixed

- Receiver: locking the screen no longer opens the AV Relay window. GNOME
  hides tray icons on the lock screen, and AV Relay took that for a desktop
  without a tray. Its window now stands in for a missing tray icon only when
  the icon has been gone for 10 seconds with the screen unlocked.

## [0.1.0] - 2026-10-17

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
  control, retransmission and keyframe requests. The receiver plays sound
  and picture with one delay, set from the network's measured delay over
  the last minute (0.2 s on a calm network, up to 2 s on a stalling Wi-Fi)
  and remembered for the next call, so both stay smooth and in sync (audio
  within ~30 ms of the picture).
- **Security**: the control channel is Noise IK with the keys from pairing
  (CPace, then Noise XXpsk3); media is DTLS-SRTP, authenticated through it.
  No account, no cloud.
- Protocol version 1 (`av-relay/1`, docs/protocol.md).

[Unreleased]: https://github.com/salihmarangoz/av_relay/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/salihmarangoz/av_relay/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/salihmarangoz/av_relay/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/salihmarangoz/av_relay/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/salihmarangoz/av_relay/releases/tag/v0.1.0
