# AV Relay for macOS (server)

The Mac side of AV Relay: a menu bar app that pairs with other computers and
serves them this Mac's camera and microphone, only while an app on them uses
them. The protocol is in `docs/protocol.md`, the design in
`docs/architecture.md`.

## Install

1. Download `AV-Relay-<version>-macOS.dmg` from the project's GitHub
   Releases, open it, and drag **AV Relay** into **Applications**. Install it
   there before the first launch: AV Relay starts itself at login, and the
   login item remembers where the app is.
2. Open it. The release is signed ad hoc (no Apple Developer ID), so macOS
   refuses the first time: open **System Settings › Privacy & Security** and
   click **Open Anyway** next to AV Relay, then confirm.
3. AV Relay lives in the menu bar (no Dock icon). At first launch it turns
   **Open at Login** on, so it runs whenever you're logged in; the menu has
   the switch if you don't want that.
4. When macOS asks whether AV Relay may find devices on your **local
   network**, allow it: without it the other computer can't receive the
   camera and microphone.
5. Choose **Allow Pairing…** in the menu and enter the address, port and code
   it shows on the other computer.
6. The first time the other computer uses the camera or microphone, macOS
   asks whether AV Relay may use them: allow both. After an update, macOS may
   ask again (ad hoc signatures change with every build).

## Layout

| path | what |
|---|---|
| `Sources/AVRelayCore/` | the protocol: Noise (`Noise.swift`), CPace and the pairing parameters (`CPace.swift`), messages, framing, the control server (`RelayServer.swift`), the key and paired-device store, and a client the tests use |
| `Sources/AVRelayMedia/` | the media engine on libwebrtc (`MediaEngine.swift`): camera (`CameraCapture.swift`) and microphone (`MicrophoneDevice.swift`, libwebrtc's audio device) on demand, H.264 with the level fix, the loopback client and the self-test |
| `Sources/CWebRTCSupport/` | `RTCAudioDevice.h` (in libwebrtc's macOS binary but shipped only with its iOS headers; unchanged, from the same build) and a small shim |
| `Sources/AVRelayApp/` | the menu bar app (SwiftUI) |
| `Sources/avrelay-devserver/` | the server without the app: for interop tests and for developing a client |
| `App/Info.plist` | the bundle's Info.plist (version filled in by the build script) |
| `scripts/build-app.sh` | builds `AV Relay.app`, the `.zip` and the `.dmg` |
| `Tests/` | the shared vectors, the server end to end, the media engine in a loopback |

Dependencies, pinned in `Package.swift`: libwebrtc M155 (stasel/WebRTC,
binary, checksum-pinned) and libsodium (swift-sodium's Clibsodium).

## Build and test

Needs Xcode 16 or newer.

```
swift test                      # everything that needs no camera or microphone
scripts/build-app.sh --native   # build/AV Relay.app, .zip, .dmg for this Mac only
scripts/build-app.sh --version 1.2.3   # universal (Apple silicon + Intel), as CI does
```

The tests read `../../tests/vectors`, and the app bundle takes its icons
from `../../assets/generated`, so work inside a full checkout.

## Test the real camera and microphone

macOS lets only an app bundle with usage descriptions open the camera and
microphone, so the real devices are tested through the app:

```
open -n "build/AV Relay.app" --args --self-test /tmp/av-relay-self-test.txt [1920x1080]
cat /tmp/av-relay-self-test.txt
```

It runs both devices through the whole media path in one process
(capture, encoding, WebRTC, decoding), reports frame rate, size, encoder and
audio, checks that the microphone closes when it's no longer wanted, and
quits. The first run asks for camera and microphone access.

## The dev server

`avrelay-devserver` is the server without the app, controlled from its
standard input:

```
swift run avrelay-devserver --port 48100 [--dir STATE] [--media test]
```

Without `--media`, it answers `media` with made-up states (no WebRTC). With
`--media test`, it runs the real media engine with a moving color-bar test
pattern and a 440 Hz tone instead of the camera and microphone, so a client
can be tested end to end. Commands: `pair` (prints a 6-digit code, valid
120 seconds and for one attempt), `close`, `list`, `remove DEVICE-ID`,
`stats` (the call's WebRTC statistics), `quit`.

The Python reference client pairs and runs a session (no media):

```
python3 -m venv .venv-vectors
.venv-vectors/bin/pip install -r tests/vectors/requirements.txt
.venv-vectors/bin/python tests/interop/reference_client.py pair 127.0.0.1 48100 123456
.venv-vectors/bin/python tests/interop/reference_client.py session 127.0.0.1 48100
```

(From the repository root; replace `123456` with the code.)

## Notes

- **Signing.** Releases are signed ad hoc (no Apple Developer ID). The first
  launch needs System Settings › Privacy & Security › Open Anyway, and macOS
  asks for camera and microphone access again after each update. For your
  own builds, `scripts/build-app.sh` signs with a code-signing identity of
  yours when `AVRELAY_SIGN_IDENTITY` names one (a self-signed certificate
  is enough; see the script). The signature then stays the same from build
  to build, and macOS remembers the permissions.
- **Keys** live in `~/Library/Application Support/AV Relay/` (the folder
  0700, the key 0600), not in the Keychain, which would ask again after
  every ad hoc signed update.
- **Local Network.** macOS asks whether AV Relay may use the local network;
  without it, WebRTC can't reach the other computer.
