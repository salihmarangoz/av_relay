# AV Relay for macOS (server)

The Mac side of AV Relay: it pairs with clients and serves them the Mac's
camera and microphone. The protocol is in `docs/protocol.md`, the design in
`docs/architecture.md`.

## Layout

| path | what |
|---|---|
| `Sources/AVRelayCore/` | the protocol: Noise (`Noise.swift`), CPace and pairing parameters (`CPace.swift`), messages, framing, the control server (`RelayServer.swift`), the identity and paired-client store, and a client used by the tests |
| `Sources/avrelay-devserver/` | the server headless, with made-up media states and no camera, mic or WebRTC |
| `Tests/AVRelayCoreTests/` | the shared vectors in `tests/vectors`, and the server end to end over loopback TCP |

The menu bar app and the media engine (libwebrtc) are next.

## Build and test

Needs Xcode 16 or newer (Swift 6).

```
swift build
swift test
```

`swift test` reads the vectors from `../../tests/vectors`, so run it inside
a full checkout.

## Try it with a client

Start the dev server (state goes to a temporary folder unless you pass `--dir`):

```
swift run avrelay-devserver --port 48100
```

Type `pair` to open the pairing window; it prints a 6-digit code that is
valid for 120 seconds and for one attempt. Other commands: `close`,
`list`, `remove DEVICE-ID`, `quit`.

From another terminal (or another computer), the Python reference client
pairs and runs a session:

```
python3 -m venv .venv-vectors
.venv-vectors/bin/pip install -r tests/vectors/requirements.txt
.venv-vectors/bin/python tests/interop/reference_client.py pair 127.0.0.1 48100 123456
.venv-vectors/bin/python tests/interop/reference_client.py session 127.0.0.1 48100
```

(Run these from the repository root; replace `123456` with the code.)
