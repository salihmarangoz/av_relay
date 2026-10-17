# AV Relay receiver on Ubuntu

The receiver for Ubuntu: what it installs, how it makes the sender's camera
and microphone appear, and what to do when something's off. The sender is
the computer whose camera and microphone you use; today that's a Mac running
[AV Relay for macOS](macos.md).

## What the package installs

| what | where |
|---|---|
| the app | `/usr/bin/av-relay` |
| app menu entry and icons | `/usr/share/applications/`, `/usr/share/icons/hicolor/` |
| start at login, for every user | `/etc/xdg/autostart/io.github.salihmarangoz.avrelay.desktop` |
| load v4l2loopback at boot, without a dummy camera | `/etc/modules-load.d/av-relay.conf`, `/etc/modprobe.d/av-relay.conf` |
| let the logged-in user create the virtual camera | `/usr/lib/udev/rules.d/70-av-relay.rules` |

Your settings and the paired sender are in `~/.config/av-relay/`. The file
`identity.key` there is this computer's key; it's readable only by you.

## The virtual camera needs v4l2loopback

**AV Relay Camera** is a [v4l2loopback](https://github.com/v4l2loopback/v4l2loopback)
device. AV Relay needs **version 0.13 or newer**: older versions can't tell
it when an app starts using the camera, and the sender's camera would never
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
dummy camera; AV Relay creates **AV Relay Camera** (`/dev/video48` when that
number is free) on its own. Other software that uses v4l2loopback, such as
OBS's virtual camera, may then need its own device:
`sudo v4l2loopback-ctl add -n "OBS Virtual Camera"`.

## While the sender is connected

- **AV Relay Camera** appears in apps, and **AV Relay Microphone** becomes the
  default input.
- When the sender goes away (sleep, Wi-Fi drop), both stay for **5 minutes**:
  the camera shows a "Waiting for <sender>…" picture and the microphone is
  silent, so a call doesn't lose its devices over a short drop. When the sender
  comes back, the picture and sound come back by themselves.
- After 5 minutes they go away, apps fall back to the built-in camera, and
  your previous default input comes back (also after a crash or power cut:
  the next start puts it back). If you pick another input meanwhile, AV
  Relay leaves your choice alone.
- **Keep virtual devices on** (top bar menu or the AV Relay window) keeps
  both for good, so the default input never switches.
- The sender's camera and microphone run only while an app here uses them, and
  stop about 3 seconds after the last one lets go.

Linux has no system-wide default camera; each app remembers its own choice.
Pick **AV Relay Camera** once in each app.

Apps that open cameras directly see **AV Relay Camera**: Firefox and Chrome
(tested), and the same way Discord (built on Chromium), Zoom and OBS. Apps
that only take cameras through PipeWire's camera portal (some Flatpak apps,
GNOME Snapshot) don't see it yet.

A level meter can count as using the microphone: while **Settings → Sound**
shows the input level of **AV Relay Microphone**, the sender's microphone may be
on.

## Switching it off

**Enable AV Relay**, at the top of the menu (and in the AV Relay window),
switches AV Relay off without quitting it: no connection to the sender, no
virtual camera or microphone (even with **Keep virtual devices on**), and
your previous default input back, as if it were closed. The icon shows the
lens struck through. It stays off after a restart until you switch it on
again.

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

**The sender isn't found when pairing.** Both computers must be on the same
network, and AV Relay must run and be switched on there. Or use the
**Manual** tab: type the address and port the sender's pairing window
shows. A VPN or a guest Wi-Fi that isolates devices can block it.

**"Pairing failed. Check the code…"** Each code works once. Open **Allow
Pairing…** on the sender again for a new one.

**"Couldn't authenticate the sender."** AV Relay was reset or reinstalled on
the sender, so it has a new key. Forget the sender in the top bar menu and
pair again.

**The camera picture is black.** That's AV Relay's placeholder: the sender
hasn't sent a picture yet. Check the sender's menu for a camera permission
prompt (System Settings → Privacy & Security → Camera).

**The sender's address changed.** Nothing to do: the sender announces itself on
the network (Bonjour), and AV Relay finds it again.

**Test the devices** without a call: open
`/usr/share/doc/av-relay/devices-test.html` in your browser (from a source
tree: `client/linux/scripts/devices-test.html`), click **Start**, and pick
**AV Relay Camera** and **AV Relay Microphone**. It shows the picture, its
size and frame rate, and a level meter for the microphone.

**Sound or picture drops out now and then.** Usually that's Wi-Fi: a
Wi-Fi network can go quiet for 0.1-0.5 seconds now and then (some do it
every 20-30 seconds). AV Relay plays sound and picture a little after they
were captured, the same for both so lips stay in sync, and sets that delay
from how late packets arrived over the last minute: 0.2 seconds on a calm
network, up to 2 seconds on a stalling one. After a stall longer than
the delay you may hear one gap; the delay then grows so the next one passes,
and shrinks again slowly once the network is calm. It starts each call with
the delay the last one ended with. For less delay, bring the sender closer
to the router or use a cable on either side. To see whether it's the network, ping the router from either
computer for a minute (`ping -c 60 <router address>`) and look for
replies slower than 100 ms.

**Logs:** started from the app grid or at login, AV Relay logs to the
journal:

```
journalctl --user -t io.github.salihmarangoz.avrelay.desktop -f
```

While an app uses the camera or microphone, a `stats:` line every 10
seconds shows what arrived (packets lost, recovered by resending, given up
on), how evenly frames reached the camera, and how often the microphone ran
dry. Or start it from a terminal with `RUST_LOG=info av-relay`.

## Uninstall

```
sudo apt remove av-relay
```

Running copies quit and give your default input back. `sudo apt purge
av-relay` also removes the module settings; `~/.config/av-relay` stays until
you delete it.

## Building from source

```
git clone https://github.com/salihmarangoz/av_relay
cd av_relay/client/linux
scripts/setup-dev.sh        # build dependencies, udev rule, v4l2loopback
cargo run --release         # the app (tray); `cargo run -- run` is headless
cargo test
cargo deb                   # the .deb (cargo install cargo-deb)
```

Rust comes from [rustup](https://rustup.rs): the code uses the 2024 edition,
newer than Ubuntu's rustc. Developer commands for testing parts on their own
(the virtual camera, the microphone, the media path, pairing) are printed by
`cargo run -- help`.
