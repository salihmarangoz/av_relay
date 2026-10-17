# Windows receiver

A Rust receiver with a small native Win32 settings window and tray icon.
There is no browser UI or GUI framework runtime. It compiles the Ubuntu
receiver's crypto, pairing, session, discovery, WebRTC, decoding and adaptive
playout code directly from `client/linux/src`; both run the same protocol tests.
Windows-specific code supplies DPAPI key storage, OBS camera output and
WASAPI audio playback through a virtual cable.

The **Camera** and **Microphone** switches make the devices available; the
sender captures video only while an app reads the virtual camera, and audio
only while an app records from the cable (CABLE Output). Ending a call stops
the sender's camera and microphone after its three-second linger; the
switches stay on, and are remembered across launches.
A paired sender reconnects automatically. The existing macOS sender needs no update.

## Prerequisites

For a release installation, download `AV-Relay-<version>-Windows-x64-Setup.exe`
from the [latest public release](https://github.com/salihmarangoz/av_relay/releases/latest).
The per-user installer includes GStreamer and the Visual C++ runtime. It creates
a Start menu shortcut and an uninstaller; Rust, build tools, and the GStreamer
development package are only needed to build from source. OBS and VB-CABLE
remain separate installs. The installer is unsigned.

Windows 10/11 x64. To build from source:

1. [Rust](https://rust-lang.org/tools/install/), stable, with the default
   `x86_64-pc-windows-msvc` toolchain.
2. [Visual Studio C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/):
   select **Desktop development with C++**, including the Windows SDK.
   The full Visual Studio IDE is not needed.
3. [GStreamer for Windows](https://gstreamer.freedesktop.org/download/),
   **MSVC x86_64**, version 1.26 or newer. With 1.28+, select **Runtime and
   development headers** (`/TYPE=devel`), including all media plugins.
   Older MSI packages need both Runtime and Development installers.

Virtual devices, also needed with a prebuilt executable:

- [OBS Studio](https://obsproject.com/download/) registers **OBS Virtual Camera**.
  OBS can remain closed. Its own **Start Virtual Camera** output must be stopped:
  it shares the same camera with AV Relay. If registration is missing, see
  [OBS's repair instructions](https://obsproject.com/kb/virtual-camera-troubleshooting).
- [VB-CABLE](https://vb-audio.com/Cable/): extract the ZIP, run its x64 installer
  as administrator, and restart Windows. Install this separately; it is not
  redistributed with AV Relay.

Check dependencies from the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File client/windows/scripts/check-deps.ps1
```

## Build and start

From the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File client/windows/scripts/build.ps1
powershell -ExecutionPolicy Bypass -File client/windows/scripts/start.ps1
```

The release build also creates `client/windows/AV Relay.lnk`. Double-click
it to open settings; launching again reopens an existing tray instance.

Pass `-GStreamerRoot 'C:\path\to\gstreamer'` for a nonstandard location.
Use the launcher unless GStreamer's `bin` is already in PATH.
The scripts also recognize isolated tools under `.tools/cargo`, `.tools/rustup`,
`.tools/w64devkit` and `.tools/gstreamer`, ignored by Git. A GNU Rust build
uses the corresponding MinGW GStreamer package. Do not mix the MSVC and
MinGW development packages.

## Pair and use

1. On the sender, choose **AV Relay > Allow Pairing...**.
2. On Windows, choose **Pair with a sender…**, select the sender (or use
   **Manual** to enter its address and port), then enter its six-digit code.
3. Turn **Camera** and/or **Microphone** on. Capture follows actual use:
   the virtual camera's readers and the cable's recording sessions.
   If Windows asks for firewall access to `av-relay-windows.exe`,
   allow it on the private LAN.
4. In your call app, select **OBS Virtual Camera** as the camera and
   **CABLE Output (VB-Audio Virtual Cable)** as the microphone. AV Relay sends
   audio to **CABLE Input**, the cable's playback endpoint.
5. Leave the switches on. When the call ends, the sender stops capture after
   its normal three-second linger. **Quit** in the tray stops everything immediately.

Closing settings leaves the tray icon running. Double-click it to reopen;
right-click for device switches and Quit. Audio defaults only to CABLE Input;
it never falls back to speakers. For another cable, select its playback
endpoint, then select its recording endpoint in the call
app. Changing audio settings turns the mic off; turn it on again. Changing
the **Resolution** dropdown (720p or 1080p) also turns the camera off and
reconnects; turn it on again afterwards.

On a network drop, the camera outputs black after one second and the audio
queue empties to silence. Outputs stay available until their switches are off
or AV Relay quits. The receiver retries the last address, then Bonjour, with
backoff up to 30 seconds. Your default Windows microphone is not changed.

## Diagnostics and tests

```powershell
powershell -ExecutionPolicy Bypass -File client/windows/scripts/start.ps1 -Doctor
. ./client/windows/scripts/env.ps1
cargo test --locked --manifest-path client/windows/Cargo.toml
```

Without GStreamer, crypto/wire vectors, DPAPI storage and camera mapping tests:

```powershell
cargo test --locked --manifest-path client/windows/Cargo.toml --no-default-features
```

To test the actual OBS and VB-CABLE outputs without the Mac or GStreamer:

```powershell
cargo run --release --manifest-path client/windows/Cargo.toml --no-default-features --features windows-audio --example device-test
```

It sends a moving test picture to OBS Virtual Camera and a 440 Hz tone only
to CABLE Input for ten seconds. The `pair` example also supports headless
pairing with a host/port argument and a one-time code read from stdin.

State is in `%LOCALAPPDATA%\AV Relay`. The identity is encrypted with this
Windows account's DPAPI key; paired public keys and settings are JSON.
`av-relay.log` contains connection/media errors. `diagnostics.txt` lists camera
registration, GStreamer plugins and audio endpoints. **Forget sender** removes
the pairing and, when connected, tells the Mac to forget this PC too.

Optional start at login:

```powershell
powershell -ExecutionPolicy Bypass -File client/windows/scripts/autostart.ps1
# Undo:
powershell -ExecutionPolicy Bypass -File client/windows/scripts/autostart.ps1 -Disable
```

## Current limitations

- Camera use detection follows OBS reader handles, and microphone use the
  cable's active recording sessions; apps that keep the device open while
  muted or hiding video still count as using it.
- Camera name is OBS Virtual Camera; microphone name is the cable's name.
- No native Media Foundation camera provider or bundled audio driver yet.
- OBS and AV Relay cannot produce through the same OBS camera at once.
- Real MacBook and call-app interoperability must be checked on the user's LAN;
  local vector tests alone do not establish that hardware test.

Basic end-to-end reception was checked on Windows on 2026-10-17 against the
existing MacBook sender: code pairing, reconnect after restarting the receiver,
1080p30 video through OBS Virtual Camera, and microphone audio through VB-CABLE.
The local output test also verified the generated video pattern and a steady
440 Hz tone. A sustained conferencing call and lip-sync calibration remain
to be tested.

To uninstall a release, use Windows **Installed apps > AV Relay > Uninstall**.
The uninstaller closes AV Relay and removes its login entry and shortcuts.
Pairing and settings are preserved in `%LOCALAPPDATA%\AV Relay`.
For a source build, disable start at login and remove the checkout. OBS and VB-CABLE
are separate installations with their own uninstallers.

## Build the installer

Install [Inno Setup 6.7+](https://jrsoftware.org/isinfo.php), then run:

```powershell
./client/windows/scripts/package.ps1
```

It packages only the required GStreamer plugins and their DLL dependencies,
copies license notices, checks the staged runtime, and emits an installer and
SHA-256 checksum in `client/windows/dist`. GitHub Actions repeats this,
tests unattended installation and uninstallation, and attaches both files to
every versioned public release alongside the macOS and Ubuntu packages.
