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

## Tests with real use

- A real call over Wi-Fi (Discord, Firefox, OBS) with the released apps:
  no audible stutter, lips in sync. If not, the receiver's `stats:` lines
  (`journalctl --user -t io.github.salihmarangoz.avrelay.desktop`) show
  the playout delay, losses, late frames and mic underruns.
- Click **Enable AV Relay** off and on in the sender's menu bar menu while
  a receiver is connected.
- Close the Mac's lid during a call: the sender ends the session before
  sleep; the receiver shows "Waiting for …" and reconnects after wake.
- The pairing window's "No sender found" state (no sender on the network,
  or the sender switched off).

## Later

- **Cameras for portal-only apps** (low priority). PipeWire skips the
  v4l2loopback camera (it's output-only when scanned), so apps that take
  cameras only through PipeWire's camera portal (some Flatpaks, GNOME
  Snapshot) don't see it. Firefox and Chrome use V4L2 and work. Fix: a
  PipeWire `Video/Source` node fed the same frames.
- **Windows receiver**: notes only, in
  [client/windows/README.md](client/windows/README.md).
