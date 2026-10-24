# TODO

Open work after the 0.1.0 release, most important first. When something here
is done, remove it, and add a line to `CHANGELOG.md` under `## [Unreleased]`
if users notice it (see [docs/releasing.md](docs/releasing.md)).

## Audio redundancy (RED)

Opus with redundancy, so a lost audio packet is rebuilt from the next ones
instead of concealed.

- **Sender half: done, not merged.** Branch `task/audio-red`: the offer's
  audio lists `red/48000/2` (`a=fmtp:63 111/111`) before Opus, and each RED
  packet carries the current Opus frame plus the two before it (about
  100 kbit/s instead of ~32). A receiver that answers Opus alone still gets
  plain Opus, so it can be merged before the receiver half. Review it first.
- **Receiver half: to do** (`client/linux/src/media/mod.rs`): put `red`
  before `opus` in the audio transceiver's codec preferences, and decode RED
  before the jitter buffer (`rtpredec`; check whether webrtcbin does it once
  red is negotiated, e.g. through the transceiver's `fec-type`, or add it as
  an aux receiver). The redundant blocks must come out as packets with their
  own sequence number and timestamp, so the jitter buffer fills holes with
  them. `protocol.md` 8.2: the receiver SHOULD accept red.
- **Proof:** a run whose `stats:` lines show audio packets lost but no mic
  underruns.

## Playout delay (receiver)

- **Apply a rise at once.** A rise of the playout delay pauses both sinks
  by the step. It's applied on the next 250 ms tick, so after a long stall
  it can make a second, separate gap. Applying it from the jitter buffer
  probe, as soon as the stall's late packets show the bigger spread, merges
  the pause with the stall's own gap (`media/delay.rs`,
  `Receiver::set_delay`).
- **Lip-sync calibration (optional).** Measured with the sender's sync test
  source, audio reaches apps ~29 ms after the picture, the same at every
  delay. Adding that to the video sink's offset (`video_offset` in
  `client/linux/src/app/mod.rs`) would bring it to ~0. It's below what
  people notice.
- How to measure: run the sender with `AVRELAY_TEST_SOURCE=sync` (a white
  frame and a 50 ms beep captured at the same instant every 2 s) and compare
  flash and beep at the receiver's camera and microphone, read in one
  GStreamer pipeline so they share a clock.

## Windows receiver (review, 2026-10-17)

Fixes for the tray, the camera restart, the playout delay, the window
flicker, audio clicks, remembered on-demand switches and reconnecting are in
CHANGELOG `[Unreleased]`. Tested on
2026-10-18 with a headless Edge reading OBS Virtual Camera, open/close
cycles against the real sender.

- **Old reports of "A lot of buffers are being dropped".** A 0.3.0 log had
  17 s of video frames dropped as late right after the camera started.
  Not reproduced in 14 cycles with this build; watch for it.

## Microphone stall (Ubuntu receiver)

The watchdog in `client/linux/src/audio/mod.rs` replaces the virtual mic's
stream when it stops taking audio; why it stops is still open.

- **Seen** on 2026-10-23 (Ubuntu 24.04, PipeWire 1.0.5, receiver 0.2.1):
  a Bluetooth headset connected (A2DP, quantum 256) 4 s into a Discord
  call recording from the mic. Within 10 s the stream got one last process
  call with `requested` 0 (so it wrote the whole 16384-frame buffer) and was
  never called again; `pw-top` counted an error on the node every cycle.
  Each time Discord linked again, the stream got exactly one call, again
  with `requested` 0. Forcing the quantum to 1024 and back
  (`pw-metadata -n settings 0 clock.force-quantum 1024`) cured it.
- **Not it:** moving the mic between drivers, changing the graph's rate or
  quantum, one whole-buffer write, or a bad rate in the rate-match area did
  not reproduce it in a container (PipeWire 1.0.5, null sinks).
- **Next time:** the `virtual microphone stalled (...)` warning logs the
  stream's free and queued buffers and the size PipeWire asked for; free
  buffers at 0 points at buffers never coming back, a size of 0 at the
  converter asking for nothing. A Bluetooth headset connecting and
  disconnecting during a call is the way to try reproducing it.

## Tests with real use

- **Playout delay after the 15 s window (Unreleased).** In a real call over
  Wi-Fi: after a one-off stall, one gap and no `playout delay` rise in the
  log; after stalls within 15 s of each other, a rise, and the `stats:`
  delay back at 200 ms within ~10 s of talking (`ms caught up` in the
  `stats:` lines). Listen while it comes down: speech up to 10% faster at
  the same pitch should sound normal, no warble or clipped words, lips in
  step. If not, lower `CATCH_UP` or raise `MIN_LIKENESS`
  (`audio/playout.rs`, `audio/stretch.rs`).

- **Ubuntu receiver after the Windows review.** Code it shares with Windows
  changed: the playout delay ignores a restarted stream's start-up burst
  (`media/delay.rs`), and `media/mod.rs` logs the decoded size
  (`receiving video at WxH`) and picks the camera's pixel format per platform
  (YUY2 on Linux, as before). CI builds and unit-tests it; run a real call on
  Ubuntu: camera opened and closed a few times with the microphone on and
  off, the picture's colours right, no rise in `playout delay` at restarts.
- A real call over Wi-Fi (Discord, Firefox, OBS) with the released apps:
  no audible stutter, lips in sync. If not, the receiver's `stats:` lines
  (`journalctl --user -t io.github.salihmarangoz.avrelay.desktop`) show
  the playout delay, losses, late frames and mic underruns.
- Click **Enable AV Relay** off and on in the sender's menu bar menu while
  a receiver is connected.
- Close the Mac's lid during a call: the sender ends the session before
  sleep; the receiver's camera shows "Waiting for connection…" and reconnects after wake.
- The pairing window's "No sender found" state (no sender on the network,
  or the sender switched off).

## Later

- **Cameras for portal-only apps** (low priority). PipeWire skips the
  v4l2loopback camera (it's output-only when scanned), so apps that take
  cameras only through PipeWire's camera portal (some Flatpaks, GNOME
  Snapshot) don't see it. Firefox and Chrome use V4L2 and work. Fix: a
  PipeWire `Video/Source` node fed the same frames.
- **Windows receiver**: initial Rust implementation in
  [client/windows](client/windows), with an installer and camera use detection.
  Add automatic microphone use detection and a native Media Foundation camera
  provider; verify sustained calls against the sender.
