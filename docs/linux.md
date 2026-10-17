# AV Relay on Ubuntu

The Ubuntu side of AV Relay: what it installs, how it makes the MacBook's
camera and microphone appear, and what to do when something's off.

## What the package installs

| what | where |
|---|---|
| the app | `/usr/bin/av-relay` |
| app menu entry and icons | `/usr/share/applications/`, `/usr/share/icons/hicolor/` |
| start at login, for every user | `/etc/xdg/autostart/io.github.salihmarangoz.avrelay.desktop` |
| load v4l2loopback at boot, without a dummy camera | `/etc/modules-load.d/av-relay.conf`, `/etc/modprobe.d/av-relay.conf` |
| let the logged-in user create the virtual camera | `/usr/lib/udev/rules.d/70-av-relay.rules` |

Your settings and the paired Mac are in `~/.config/av-relay/`. The file
`identity.key` there is this computer's key; it's readable only by you.

## The virtual camera needs v4l2loopback

**MacBook Camera** is a [v4l2loopback](https://github.com/v4l2loopback/v4l2loopback)
device. AV Relay needs **version 0.13 or newer**: older versions can't tell
it when an app starts using the camera, and the Mac's camera would never
turn on.

- **HWE kernel** (the default on Ubuntu 24.04.2 and later): Ubuntu ships a
  matching, signed module for the current HWE kernel.

  ```
  sudo apt install linux-modules-v4l2loopback-generic-hwe-24.04
  ```

  then restart.

- **Original 24.04 kernel** (`uname -r` shows `6.8`): Ubuntu's
  `v4l2loopback-dkms` is 0.12.7, which is too old. Switch to the HWE kernel (`sudo apt install
  linux-generic-hwe-24.04`, restart), then install the package above.

To check: `cat /sys/module/v4l2loopback/version`. When something's wrong,
the top bar menu shows **Camera unavailable: details…**, and the AV Relay
window says what to run. The microphone works either way.

The package loads v4l2loopback with `devices=0`, so it doesn't create a
dummy camera; AV Relay creates **MacBook Camera** (`/dev/video48` when that
number is free) on its own. Other software that uses v4l2loopback, such as
OBS's virtual camera, may then need its own device:
`sudo v4l2loopback-ctl add -n "OBS Virtual Camera"`.

## While the Mac is connected

- **MacBook Camera** appears in apps, and **MacBook Microphone** becomes the
  default input.
- When the Mac goes away (sleep, Wi-Fi drop), both stay for **5 minutes**:
  the camera shows a "Waiting for <your Mac>…" picture and the microphone is
  silent, so a call doesn't lose its devices over a short drop. When the Mac
  comes back, the picture and sound come back by themselves.
- After 5 minutes they go away, apps fall back to the built-in camera, and
  your previous default input comes back (also after a crash or power cut:
  the next start puts it back). If you pick another input meanwhile, AV
  Relay leaves your choice alone.
- **Keep virtual devices on** (top bar menu or the AV Relay window) keeps
  both for good, so the default input never switches.
- The Mac's camera and microphone run only while an app here uses them, and
  stop about 3 seconds after the last one lets go.

Linux has no system-wide default camera; each app remembers its own choice.
Pick **MacBook Camera** once in each app.

Apps that open cameras directly see **MacBook Camera**: Firefox and Chrome
(tested), and the same way Discord (built on Chromium), Zoom and OBS. Apps
that only take cameras through PipeWire's camera portal (some Flatpak apps,
GNOME Snapshot) don't see it yet.

A level meter can count as using the microphone: while **Settings → Sound**
shows the input level of **MacBook Microphone**, the Mac's microphone may be
on.

## Start at login

The package starts AV Relay at every login, for every user. To turn that off
for yourself, untick **Start at login** in the top bar menu (or switch it off
in the AV Relay window); this writes
`~/.config/autostart/io.github.salihmarangoz.avrelay.desktop`, which
overrides the system entry. Ticking it again removes the override.

## Troubleshooting

**There's no AV Relay icon in the top bar.** Ubuntu shows tray icons through
the *Ubuntu AppIndicators* GNOME extension, which is on by default. If it's
been turned off:

```
gnome-extensions enable ubuntu-appindicators@ubuntu.com
```

Without it, AV Relay still works; open it from the app grid to see its
window.

**The Mac isn't found when pairing.** Both computers must be on the same
network, and the Mac's pairing window must be open (it closes after 2
minutes). Type the address and port the Mac shows. A VPN or a guest Wi-Fi
that isolates devices can block it.

**"Pairing failed. Check the code…"** Each code works once. Open **Allow
Pairing…** on the Mac again for a new one.

**"Couldn't authenticate the Mac."** AV Relay was reset or reinstalled on the
Mac, so it has a new key. Forget the Mac in the top bar menu and pair again.

**The camera picture is black.** That's AV Relay's placeholder: the Mac
hasn't sent a picture yet. Check the Mac's menu for a camera permission
prompt (System Settings → Privacy & Security → Camera).

**The Mac's address changed.** Nothing to do: the Mac announces itself on
the network (Bonjour), and AV Relay finds it again.

**Test the devices** without a call: open
`/usr/share/doc/av-relay/devices-test.html` in your browser (from a source
tree: `client/linux/scripts/devices-test.html`), click **Start**, and pick
**MacBook Camera** and **MacBook Microphone**. It shows the picture, its
size and frame rate, and a level meter for the microphone.

**Logs:** start it from a terminal with `RUST_LOG=info av-relay`.

## Uninstall

```
sudo apt remove av-relay
```

Running copies quit and give your default input back. `sudo apt purge
av-relay` also removes the module settings; `~/.config/av-relay` stays until
you delete it.

## Building from source

```
git clone https://github.com/salihmarangoz/av_relay_mac_to_linux
cd av_relay_mac_to_linux/client/linux
scripts/setup-dev.sh        # build dependencies, udev rule, v4l2loopback
cargo run --release         # the app (tray); `cargo run -- run` is headless
cargo test
cargo deb                   # the .deb (cargo install cargo-deb)
```

Rust comes from [rustup](https://rustup.rs): the code uses the 2024 edition,
newer than Ubuntu's rustc. Developer commands for testing parts on their own
(the virtual camera, the microphone, the media path, pairing) are printed by
`cargo run -- help`.
