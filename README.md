<p align="center">
  <img src="assets/generated/app-icon-128.png" width="96" alt="AV Relay logo">
</p>

<h1 align="center">AV Relay</h1>

<p align="center">Use your MacBook's camera and microphone on your Ubuntu computer.</p>

AV Relay turns a MacBook into a webcam and microphone for a Linux PC on the
same network. On Ubuntu they show up as **MacBook Camera** and **MacBook
Microphone**, like any USB webcam, so every app can use them: Firefox,
Chrome, Discord, Zoom, OBS, …

- **On demand.** The Mac's camera and microphone run only while an app on
  Ubuntu uses them. Close the call and the Mac's camera light goes out.
- **Pair once.** A 6-digit code shown on the Mac, typed on Ubuntu. After
  that the two find each other on their own, even if the Mac's address
  changes.
- **Private.** Nothing leaves your network: no account, no cloud. Everything
  between the two computers is encrypted end to end.
- **Out of the way.** It starts at login and lives in the menu bar on the Mac
  and in the top bar on Ubuntu. While the Mac is connected, its microphone is
  Ubuntu's default input; when it goes away, your previous one comes back.

## How it works

```
 MacBook (server)                                  Ubuntu (client)
 ─────────────────                                 ───────────────────────────────
 camera ─┐                                          ┌─► "MacBook Camera"  (webcam)
         ├─ H.264 / Opus ── WebRTC over your LAN ───┤
 mic ────┘     encrypted, paired with a code        └─► "MacBook Microphone" (input)
```

When an app on Ubuntu opens the camera or the microphone, AV Relay asks the
Mac to start it; when the app lets go, the Mac stops a few seconds later.
Video is H.264, encoded in hardware on the Mac (up to 1080p at 30 fps); audio
is Opus. The details are in [docs/architecture.md](docs/architecture.md) and
the wire protocol in [docs/protocol.md](docs/protocol.md).

## What you need

- A Mac with **macOS 14** or newer.
- A PC with **Ubuntu 24.04** (GNOME). Other recent Linux desktops with
  PipeWire may work too, but aren't tested.
- Both on the **same network** (Wi-Fi is fine).
- On Ubuntu, the **v4l2loopback** kernel module, version 0.13 or newer. With
  Ubuntu's HWE kernel (the default on 24.04.2 and later) it's one package;
  see below.

## Install

Download both apps from the [latest release](../../releases/latest).

### On the Mac

1. Open `AV-Relay-<version>-macOS.dmg` and drag **AV Relay** to
   **Applications**. (Start it from Applications: the login item remembers
   where the app is.)
2. Open it. The first time, macOS says it can't check the app for malware,
   because it isn't signed with an Apple Developer ID. Go to **System
   Settings → Privacy & Security** and click **Open Anyway**.
3. AV Relay appears in the menu bar and turns **Open at Login** on. When
   macOS asks whether it may use the **local network**, allow it; it asks for
   camera and microphone permission the first time Ubuntu uses them.

More in [docs/macos.md](docs/macos.md).

### On Ubuntu

```
sudo apt install ./av-relay_<version>_amd64.deb linux-modules-v4l2loopback-generic-hwe-24.04
```

Then restart (or log out and back in). AV Relay starts at every login and
shows its icon in the top bar. To start it right away, open **AV Relay** from
the app grid.

Not on the HWE kernel, or the camera says it's unavailable? See
[docs/linux.md](docs/linux.md#the-virtual-camera-needs-v4l2loopback).

## Pair

1. **On the Mac:** click the AV Relay icon in the menu bar → **Allow
   Pairing…**. It shows a 6-digit code, its address and its port.
2. **On Ubuntu:** click the AV Relay icon in the top bar → **Pair with a
   Mac…**. Pick your Mac from the list (or type its address and port), click
   **Continue**, and type the code.

That's it. The Mac's pairing window closes on its own, and from now on the
two connect by themselves whenever both are on.

## Use

In your app's settings, choose **MacBook Camera** and **MacBook Microphone**.
The microphone is already the default input while the Mac is connected; most
apps remember the camera you chose.

If the Mac drops off (sleep, Wi-Fi), the camera and microphone stay for 5
minutes, the camera showing a "Waiting for your Mac" picture, so a short drop
doesn't break your call. Turn on **Keep virtual devices on** to keep them for
good.

The top bar menu shows whether the Mac is connected and whether the camera and
microphone are in use. From it you can also pick the camera quality (1080p or
720p), keep the virtual devices on, turn **Start at login** off, pair another
Mac, or forget this one.

## Troubleshooting

- [Ubuntu: the camera is missing, the Mac isn't found, no top-bar icon, …](docs/linux.md#troubleshooting)
- [Mac: permissions, Open Anyway, the menu bar, …](docs/macos.md)

## Tested on

Ubuntu 24.04 and a MacBook Air (M4) with macOS 27, over Wi-Fi.

## Building from source

- Ubuntu app: [client/linux](client/linux) (Rust) and
  [docs/linux.md](docs/linux.md#building-from-source)
- Mac app: [server/macos](server/macos) (Swift)
- Both follow [docs/protocol.md](docs/protocol.md) and test against the shared
  vectors in [tests/vectors](tests/vectors).

## License

[GPL-3.0-or-later](LICENSE).
