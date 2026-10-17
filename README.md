<p align="center">
  <img src="assets/generated/app-icon-128.png" width="96" alt="AV Relay logo">
</p>

<h1 align="center">AV Relay</h1>

<p align="center">Use one computer's camera and microphone on another.</p>

AV Relay has two sides. The **sender** shares its camera and microphone. The
**receiver** shows them as an ordinary webcam and microphone, **AV Relay
Camera** and **AV Relay Microphone**, so every app can use them: Firefox,
Chrome, Discord, Zoom, OBS, … Today the sender runs on a Mac and the receiver
on Ubuntu, on the same network.

- **On demand.** The sender's camera and microphone run only while an app on
  the receiver uses them. Close the call and the camera light goes out.
- **Pair once.** A 6-digit code shown on the sender, typed on the receiver.
  After that the two find each other on their own, even if an address
  changes.
- **Private.** Nothing leaves your network: no account, no cloud. Everything
  between the two computers is encrypted end to end.
- **Out of the way.** Both start at login and live in the menu bar or top
  bar. While the sender is connected, its microphone is the receiver's
  default input; when it goes away, your previous one comes back.

## Pick your installation

Install the **sender** on the computer whose camera and microphone you want
to use, and the **receiver** on the computer where you want them.

| Your computer | Sender: shares its camera and microphone | Receiver: uses them as a webcam and microphone |
|---|---|---|
| **macOS** 14 or newer | [Install the sender](#sender-on-macos) | Not yet |
| **Ubuntu** 24.04 | Not yet | [Install the receiver](#receiver-on-ubuntu) |
| **Windows** | Not yet | Not yet ([notes](client/windows/README.md)) |

Download from the [latest release](../../releases/latest). Then
[pair the two](#pair).

## Sender

### Sender on macOS

You need macOS 14 or newer.

1. Open `AV-Relay-<version>-macOS.dmg` and drag **AV Relay** to
   **Applications**. (Start it from Applications: the login item remembers
   where the app is.)
2. Open it. The first time, macOS says it can't check the app for malware,
   because it isn't signed with an Apple Developer ID. Go to **System
   Settings → Privacy & Security** and click **Open Anyway**.
3. AV Relay appears in the menu bar and turns **Open at Login** on. When
   macOS asks whether it may use the **local network**, allow it; it asks for
   camera and microphone permission the first time a receiver uses them.

More, including troubleshooting: [docs/macos.md](docs/macos.md).

## Receiver

### Receiver on Ubuntu

You need Ubuntu 24.04 (GNOME) and the **v4l2loopback** kernel module, version
0.13 or newer. With Ubuntu's HWE kernel (the default on 24.04.2 and later)
that's one extra package:

```
sudo apt install ./av-relay_<version>_amd64.deb linux-modules-v4l2loopback-generic-hwe-24.04
```

Then restart (or log out and back in). AV Relay starts at every login and
shows its icon in the top bar. To start it right away, open **AV Relay** from
the app grid.

Not on the HWE kernel, or the camera says it's unavailable? See
[docs/linux.md](docs/linux.md#the-virtual-camera-needs-v4l2loopback). Other
recent Linux desktops with PipeWire may work too, but aren't tested.

More, including troubleshooting: [docs/linux.md](docs/linux.md).

## Pair

1. **On the sender:** click the AV Relay icon in the menu bar → **Allow
   Pairing…**. It shows a 6-digit code, its address and its port.
2. **On the receiver:** click the AV Relay icon in the top bar → **Pair with
   a sender…**. Under **Auto-discovery**, pick the sender (or type its
   address and port under **Manual**), click **Continue**, and type the code.

That's it. The sender's pairing window then shows **Paired with** and the
receiver's name; from now on the two connect by themselves whenever both
are on. A receiver uses one
sender; a sender can be paired with several receivers and serves one at a
time.

## Use

In your app's settings, choose **AV Relay Camera** and **AV Relay
Microphone**. The microphone is already the default input while the sender
is connected; most apps remember the camera you chose.

If the sender drops off (sleep, Wi-Fi), the camera and microphone stay for 5
minutes, the camera showing a "Waiting for …" picture, so a short drop
doesn't break your call. Turn on **Keep virtual devices on** to keep them for
good.

Each side's menu shows whether they're connected and whether the camera and
microphone are in use. **Enable AV Relay** at the top switches a side off
without quitting it, as if it were closed, and back on. On the receiver you
can also pick the camera quality (1080p or 720p), turn **Start at login**
off, pair with a different sender, or forget this one.

## How it works

```
 sender (server)                                    receiver (client)
 ─────────────────                                  ───────────────────────────────
 camera ─┐                                          ┌─► "AV Relay Camera"      (webcam)
         ├─ H.264 / Opus ── WebRTC over your LAN ───┤
 mic ────┘     encrypted, paired with a code        └─► "AV Relay Microphone"  (input)
```

When an app on the receiver opens the camera or the microphone, AV Relay asks
the sender to start it; when the app lets go, the sender stops a few seconds
later. Video is H.264, encoded in hardware on the Mac (up to 1080p at 30
fps); audio is Opus. Over Wi-Fi the receiver keeps a small buffer that grows
when the network stalls, and holds the picture back by the same amount, so
sound stays smooth and in step with the picture. The details are in
[docs/architecture.md](docs/architecture.md) and the wire protocol in
[docs/protocol.md](docs/protocol.md).

## Tested on

A MacBook Air (M4) with macOS 27 as the sender and Ubuntu 24.04 as the
receiver, over Wi-Fi.

## Building from source

- Sender for macOS: [server/macos](server/macos) (Swift)
- Receiver for Linux: [client/linux](client/linux) (Rust) and
  [docs/linux.md](docs/linux.md#building-from-source)
- Both follow [docs/protocol.md](docs/protocol.md) and test against the shared
  vectors in [tests/vectors](tests/vectors).
- What changed in each release: [CHANGELOG.md](CHANGELOG.md); how a release
  is made: [docs/releasing.md](docs/releasing.md).

## License

[GPL-3.0-or-later](LICENSE).
