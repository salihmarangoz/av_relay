# Windows receiver: notes, not started

There's no Windows receiver (client) yet. These notes are for whoever
starts one. The protocol ([docs/protocol.md](../../docs/protocol.md)) has
nothing Linux-specific in it, so a Windows receiver talks to the same
sender (today the macOS app).

## What it would take

A Windows receiver does what the Linux one does: pair with the code, keep a
session, answer the sender's WebRTC offer, decode H.264 and Opus, and present
the result as a camera and a microphone that every app can pick. The first
four parts carry over (libwebrtc or GStreamer both run on Windows); the
devices don't.

### Camera: free, no driver

Windows 11 has a user-mode virtual camera API: `MFCreateVirtualCamera`
(Media Foundation, Windows 11 build 22000 and later). The app registers a
media source (a COM object) that hands out the decoded frames; Windows shows
it to apps as a camera. It needs no kernel driver, so no driver signing.
Windows 10 has no equivalent without a driver.

### Microphone: the hard part

Windows has no user-mode API for a virtual microphone; an audio endpoint
needs a kernel-mode audio driver, and shipping a kernel driver needs
Microsoft's attestation signing, which needs an EV code-signing certificate
(a yearly cost). Ways around paying for that, best first:

1. **Use a virtual audio cable the user installs.** Free, already-signed
   drivers exist; VB-CABLE is a common one (donationware for end users;
   check its terms, as bundling it in another installer usually needs a
   license, so the user installs it). The receiver
   plays the sender's audio into the cable's playback side, and apps choose the
   cable's recording side as their microphone. Costs nothing; the price is
   one extra install and a device name that isn't "AV Relay Microphone".
2. **An open-source virtual audio driver that is already signed**, if one
   with a suitable license exists when the work starts. Check its license
   and how it is signed before depending on it.
3. **Our own driver** (Microsoft's SysVAD sample is the usual start), signed
   through Microsoft's attestation: the cleanest result, but it needs the
   certificate.

Making the microphone the default input (as the Linux receiver does) is
possible with Windows' audio policy interfaces, which are undocumented;
asking the user is the safe default.

## Decision so far

Not started (owner's call, 2026-10-16). Paying for driver signing is out for
now; option 1 is the likely path if a Windows receiver is built.
