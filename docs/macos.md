# The AV Relay sender on macOS

On a Mac, the sender (server) side of AV Relay is a small menu bar app. It
pairs with a receiver (client), such as AV Relay on your Linux computer, and
serves it this Mac's camera and microphone, only while an app on the receiver
is using them.

## Install

1. Download `AV-Relay-<version>-macOS.dmg` from the
   [latest release](../../../releases/latest), open it, and drag **AV Relay**
   to **Applications**. Start it from there: AV Relay opens itself at login,
   and the login item remembers where the app is.
2. Open it. The first time, macOS says it can't check the app for malware,
   because it isn't signed with an Apple Developer ID. Open **System Settings
   → Privacy & Security**, scroll down, and click **Open Anyway** next to
   AV Relay; confirm with your password or Touch ID.
3. When macOS asks whether AV Relay may **find and connect to devices on your
   local network**, click **Allow**. Without it, the receiver can pair but can't
   receive the camera or microphone.
4. The first time an app on the receiver uses the camera or the microphone, macOS
   asks whether AV Relay may use it. Click **Allow** for both.

AV Relay needs macOS 14 or newer, on Apple silicon or Intel.

## The menu

AV Relay lives in the menu bar; it has no Dock icon. The icon shows its state:

| icon | meaning |
|---|---|
| lens only | no computer connected |
| lens and arcs | a paired computer is connected |
| filled lens | the camera or microphone is in use |

The menu:

- **Status:** which computer is connected, and whether the camera and
  microphone are off, starting, on, or not allowed.
- **Allow Pairing…** opens the pairing window (see below).
- **Paired Devices:** every computer paired with this Mac. Choose one to
  remove it: it can't use the camera or microphone again until you pair it
  again.
- **Camera** and **Microphone:** which ones to use. The default is the Mac's
  built-in camera and microphone, even when an iPhone (Continuity Camera),
  AirPods or a USB device is connected; pick another here if you want it.
- **Open at Login:** on by default, so AV Relay runs whenever you're logged
  in. Turn it off to start AV Relay yourself.
- **Settings…:** the port (see below) and this Mac's device ID.
- **About AV Relay**, **Quit AV Relay**.

## Pairing

Choose **Allow Pairing…**. The window shows:

- this Mac's address and port, e.g. `192.168.1.20 port 48100` (one line per
  network; normally use the Wi-Fi or Ethernet one);
- a 6-digit code, valid for 2 minutes and for one attempt.

On the receiver, choose pairing in AV Relay's menu, pick this Mac (or type
the address and port), and type the code. The pairing window then says
**Paired with** and the computer's name.

If the code was mistyped, the window says **Pairing failed**: each code works
once, so click **New Code** and try again. Pairing uses the code only to agree
on keys: it never travels over the network, and a wrong guess teaches an
eavesdropper nothing.

## When the camera and microphone run

Only while an app on the receiver uses them. The Mac's camera light and the
microphone indicator in the menu bar show it, as with any app. When the app
on the receiver lets go, AV Relay turns them off 3 seconds later (so an app that
reopens a device right away doesn't flicker the light). While nothing is
connected, AV Relay doesn't touch them at all.

## The network

- Both computers must be on the same network. AV Relay works over Wi-Fi.
- The Mac listens on **TCP port 48100**. If something else uses that port,
  pick another in **Settings…**; paired computers find the Mac again by
  themselves (Bonjour), and new pairings use the new port.
- If the macOS firewall is on, allow incoming connections for AV Relay when
  macOS asks, or in **System Settings → Network → Firewall → Options**.
  "Block all incoming connections" stops AV Relay from working.

## Troubleshooting

**The receiver can't find or reach the Mac.** Check that AV Relay runs (its icon is
in the menu bar), that both computers are on the same network, and the
firewall (above). In the pairing window, use the address on the same network
as the receiver.

**The camera or microphone says "not allowed".** Open **System Settings →
Privacy & Security → Camera** (and **Microphone**) and turn AV Relay on. After
an AV Relay update, macOS may ask again: the release is signed without an
Apple Developer ID, and macOS ties the permission to the app's signature.

**Connected, but no picture or sound on the receiver.** Check **System Settings →
Privacy & Security → Local Network**: AV Relay must be on.

**The menu bar icon is missing.** On a MacBook with a notch, too many menu
bar icons can hide some of them behind it; quit a few others or use a menu
bar manager. If AV Relay isn't running at all, open it from Applications.

**AV Relay doesn't start at login.** Check that **Open at Login** is on in
its menu, and that it's listed and on in **System Settings → General → Login
Items**. If you moved the app after the first launch, turn **Open at Login**
off and on again.

**Start over.** Quit AV Relay and delete `~/Library/Application Support/AV
Relay`: this Mac gets a new identity, and every paired computer must pair
again.

## Uninstall

1. Turn **Open at Login** off in AV Relay's menu, then quit it.
2. Move **AV Relay** from Applications to the Trash.
3. To remove its data too: delete `~/Library/Application Support/AV Relay`
   and run `defaults delete io.github.salihmarangoz.avrelay` in Terminal.

## Building from source

See [server/macos/README.md](../server/macos/README.md): build and test with
Swift Package Manager, `scripts/build-app.sh` for the app bundle, the
real-device self-test, and the development server.
