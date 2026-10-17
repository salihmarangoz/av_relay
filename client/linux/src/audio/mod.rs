//! Everything the client does with PipeWire, on one thread of its own:
//!
//! - the virtual microphone, an `Audio/Source` node named "AV Relay
//!   Microphone" that apps record from, fed with the sender's audio;
//! - noticing when an app records from it (the node starts streaming), so
//!   the sender's microphone runs only then;
//! - the default input: made the virtual mic while the sender is connected, and
//!   given back afterwards (PipeWire's "default" metadata, which WirePlumber
//!   follows).
//!
//! PipeWire objects can't leave their thread, so other threads talk to it
//! through [`PipeWire`]'s commands. Audio crosses over in a lock-free ring.

mod default_input;
mod dll;
mod playout;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;
use pw::types::ObjectType;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use dll::Dll;
use playout::Playout;

pub use default_input::DefaultInput;

pub const MIC_NODE_NAME: &str = "av_relay.microphone";
/// What the node was called before (0.1.0 test builds): a default input
/// left at it by a crash is still ours to give back.
const OLD_MIC_NODE_NAMES: &[&str] = &["av_relay.macbook_mic"];

/// Whether a default-input node name is the virtual microphone's.
fn is_mic_node(name: Option<&str>) -> bool {
    name.is_some_and(|n| n == MIC_NODE_NAME || OLD_MIC_NODE_NAMES.contains(&n))
}
pub const MIC_DESCRIPTION: &str = "AV Relay Microphone";
pub const RATE: u32 = 48_000;
const DEFAULT_SOURCE_KEY: &str = "default.configured.audio.source";

/// Room for the largest cushion (see playout.rs) with plenty to spare.
const RING_CAPACITY: usize = RATE as usize;
/// Larger errors are clamped, so a burst doesn't yank the resampler. One
/// chunk: with this the loop speeds up by up to ~0.3%, enough to follow the
/// cushion as it shrinks (playout.rs) without hearing a change in pitch.
const MAX_ERROR: f64 = playout::CHUNK as f64;

/// What happened to the microphone's audio, for the stats log.
#[derive(Default)]
pub struct MicCounters {
    /// The app wanted audio and the queue ran dry.
    pub underruns: AtomicU64,
    /// Frames of silence played instead of audio (priming, underruns).
    pub silent_frames: AtomicU64,
    /// Frames thrown away because too much was queued.
    pub skipped_frames: AtomicU64,
    /// Samples the feed couldn't queue (ring full).
    pub overflow_samples: AtomicU64,
    /// The queue when the app last took audio, and its target, in frames.
    pub fill: AtomicU64,
    pub target: AtomicU64,
}

/// A snapshot of `MicCounters`, the counts reset by `take()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct MicStats {
    pub underruns: u64,
    pub silent_frames: u64,
    pub skipped_frames: u64,
    pub overflow_samples: u64,
    pub fill: u64,
    pub target: u64,
}

impl MicCounters {
    /// The queue's current target, in frames (0 before the mic first ran).
    pub fn target_frames(&self) -> u64 {
        self.target.load(Ordering::Relaxed)
    }

    /// The counts since the last call (which resets them) and the queue now.
    pub fn take(&self) -> MicStats {
        MicStats {
            underruns: self.underruns.swap(0, Ordering::Relaxed),
            silent_frames: self.silent_frames.swap(0, Ordering::Relaxed),
            skipped_frames: self.skipped_frames.swap(0, Ordering::Relaxed),
            overflow_samples: self.overflow_samples.swap(0, Ordering::Relaxed),
            fill: self.fill.load(Ordering::Relaxed),
            target: self.target.load(Ordering::Relaxed),
        }
    }
}

enum Cmd {
    StartMic {
        consumer: HeapCons<f32>,
        in_use: Arc<AtomicBool>,
        counters: Arc<MicCounters>,
        on_usage: Box<dyn Fn(bool) + Send>,
    },
    StopMic,
    /// Sets the configured default input: a node name, or `None` to unset.
    SetDefaultSource(Option<String>),
    /// Replies with the configured default input's node name (if any).
    GetDefaultSource(mpsc::Sender<Option<String>>),
    Quit,
}

/// The PipeWire thread.
pub struct PipeWire {
    cmd: pw::channel::Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

/// Feeds the virtual microphone; exists while the mic does.
pub struct MicFeed {
    producer: Arc<Mutex<HeapProd<f32>>>,
    in_use: Arc<AtomicBool>,
    counters: Arc<MicCounters>,
}

impl MicFeed {
    /// Queues mono 48 kHz samples. While no app records, they're dropped, so
    /// an app that starts recording hears fresh audio, not a stale backlog.
    pub fn push(&self, samples: &[f32]) {
        if !self.in_use.load(Ordering::Relaxed) {
            return;
        }
        let mut producer = self.producer.lock().unwrap();
        let pushed = producer.push_slice(samples);
        if pushed < samples.len() {
            self.counters.overflow_samples.fetch_add((samples.len() - pushed) as u64, Ordering::Relaxed);
        }
    }

    pub fn counters(&self) -> &MicCounters {
        &self.counters
    }
}

impl PipeWire {
    /// Connects to PipeWire on a new thread; fails if PipeWire isn't running.
    pub fn start() -> Result<Self> {
        let (cmd_tx, cmd_rx) = pw::channel::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let thread = thread::Builder::new().name("pipewire".into()).spawn(move || {
            if let Err(e) = run(cmd_rx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        ready_rx.recv().context("the PipeWire thread died")??;
        Ok(Self { cmd: cmd_tx, thread: Some(thread) })
    }

    /// Creates the virtual microphone. `on_usage` runs (on the PipeWire
    /// thread) with `true` when an app starts recording from it and `false`
    /// when the last one stops.
    pub fn start_mic(&self, on_usage: impl Fn(bool) + Send + 'static) -> Result<MicFeed> {
        let (producer, consumer) = HeapRb::<f32>::new(RING_CAPACITY).split();
        let in_use = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(MicCounters::default());
        self.send(Cmd::StartMic { consumer, in_use: in_use.clone(), counters: counters.clone(), on_usage: Box::new(on_usage) })?;
        Ok(MicFeed { producer: Arc::new(Mutex::new(producer)), in_use, counters })
    }

    pub fn stop_mic(&self) -> Result<()> {
        self.send(Cmd::StopMic)
    }

    pub fn default_source(&self) -> Result<Option<String>> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::GetDefaultSource(tx))?;
        rx.recv().context("the PipeWire thread died")
    }

    pub fn set_default_source(&self, node_name: Option<&str>) -> Result<()> {
        self.send(Cmd::SetDefaultSource(node_name.map(str::to_owned)))
    }

    fn send(&self, cmd: Cmd) -> Result<()> {
        self.cmd.send(cmd).map_err(|_| anyhow!("the PipeWire thread has stopped"))
    }
}

impl Drop for PipeWire {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The "default" metadata object, once bound, and the configured default
/// input it reported last.
#[derive(Default)]
struct Defaults {
    metadata: Option<(pw::metadata::Metadata, pw::metadata::MetadataListener)>,
    configured_source: Option<String>,
}

fn run(cmd_rx: pw::channel::Receiver<Cmd>, ready: &mpsc::Sender<Result<()>>) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("creating the PipeWire loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None).context("creating the PipeWire context")?;
    let core = context.connect_rc(None).context("connecting to PipeWire (is it running?)")?;
    let registry = core.get_registry_rc().context("getting the PipeWire registry")?;

    let defaults = Rc::new(RefCell::new(Defaults::default()));
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let registry = registry.clone();
            let defaults = defaults.clone();
            move |global| {
                if global.type_ != ObjectType::Metadata
                    || global.props.as_ref().and_then(|p| p.get("metadata.name")) != Some("default")
                {
                    return;
                }
                let metadata: pw::metadata::Metadata = match registry.bind(global) {
                    Ok(m) => m,
                    Err(e) => return log::warn!("binding PipeWire's default metadata: {e}"),
                };
                let listener = metadata
                    .add_listener_local()
                    .property({
                        let defaults = defaults.clone();
                        move |subject, key, _type, value| {
                            if subject == 0 && key == Some(DEFAULT_SOURCE_KEY) {
                                defaults.borrow_mut().configured_source = value.and_then(node_name_from_json);
                            }
                            0
                        }
                    })
                    .register();
                defaults.borrow_mut().metadata = Some((metadata, listener));
            }
        })
        .register();

    // Wait until the registry (and the metadata's properties) have arrived,
    // so the first GetDefaultSource sees the real value.
    roundtrip(&mainloop, &core)?;
    roundtrip(&mainloop, &core)?;

    let mic: Rc<RefCell<Option<Mic>>> = Rc::new(RefCell::new(None));
    let _cmd_receiver = cmd_rx.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        let core = core.clone();
        let mic = mic.clone();
        let defaults = defaults.clone();
        move |cmd| match cmd {
            Cmd::StartMic { consumer, in_use, counters, on_usage } => {
                mic.borrow_mut().take();
                match Mic::new(&core, consumer, in_use, counters, on_usage) {
                    Ok(m) => *mic.borrow_mut() = Some(m),
                    Err(e) => log::error!("creating the virtual microphone: {e}"),
                }
            }
            Cmd::StopMic => {
                if mic.borrow_mut().take().is_some() {
                    log::info!("virtual microphone removed");
                }
            }
            Cmd::SetDefaultSource(name) => set_default_source(&mut defaults.borrow_mut(), name),
            Cmd::GetDefaultSource(reply) => {
                let _ = reply.send(defaults.borrow().configured_source.clone());
            }
            Cmd::Quit => {
                mic.borrow_mut().take();
                mainloop.quit();
            }
        }
    });

    let _ = ready.send(Ok(()));
    mainloop.run();
    // Quit stopped the loop; let the server handle what's still queued (the
    // last default-input change, the mic's removal) before disconnecting.
    roundtrip(&mainloop, &core)
}

/// Runs the loop until the server has handled everything sent so far.
fn roundtrip(mainloop: &pw::main_loop::MainLoopRc, core: &pw::core::CoreRc) -> Result<()> {
    let done = Rc::new(std::cell::Cell::new(false));
    let pending = core.sync(0)?;
    let _listener = core
        .add_listener_local()
        .done({
            let done = done.clone();
            let mainloop = mainloop.clone();
            move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    done.set(true);
                    mainloop.quit();
                }
            }
        })
        .register();
    while !done.get() {
        mainloop.run();
    }
    Ok(())
}

fn set_default_source(defaults: &mut Defaults, name: Option<String>) {
    let Some((metadata, _)) = &defaults.metadata else {
        return log::warn!("PipeWire has no default metadata; is WirePlumber running?");
    };
    match &name {
        Some(name) => {
            let value = serde_json::json!({ "name": name }).to_string();
            metadata.set_property(0, DEFAULT_SOURCE_KEY, Some("Spa:String:JSON"), Some(&value));
        }
        None => metadata.set_property(0, DEFAULT_SOURCE_KEY, None, None),
    }
    log::info!("default input set to {}", name.as_deref().unwrap_or("(automatic)"));
    // The server echoes the change a moment later; until then, answer
    // GetDefaultSource with what we just set, not the old value.
    defaults.configured_source = name;
}

/// `{"name":"alsa_input..."}` → `alsa_input...`
fn node_name_from_json(value: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value).ok()?["name"].as_str().map(str::to_owned)
}

/// The virtual microphone's stream and listener; dropping it removes the node.
struct Mic {
    _listener: pw::stream::StreamListener<MicState>,
    stream: pw::stream::StreamRc,
}

struct MicState {
    consumer: HeapCons<f32>,
    on_usage: Box<dyn Fn(bool) + Send>,
    /// Shared with the feed, which drops audio while this is false.
    in_use: Arc<AtomicBool>,
    rate_match: *mut spa::sys::spa_io_rate_match,
    dll: Dll,
    playout: Playout,
    scratch: Vec<f32>,
    cycles: u64,
    counters: Arc<MicCounters>,
}

impl Drop for Mic {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
    }
}

impl Mic {
    fn new(
        core: &pw::core::CoreRc,
        consumer: HeapCons<f32>,
        in_use: Arc<AtomicBool>,
        counters: Arc<MicCounters>,
        on_usage: Box<dyn Fn(bool) + Send>,
    ) -> Result<Self> {
        let props = pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CLASS => "Audio/Source",
            *pw::keys::NODE_NAME => MIC_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => MIC_DESCRIPTION,
            *pw::keys::AUDIO_CHANNELS => "1",
            "device.icon-name" => "audio-input-microphone",
        };
        let stream = pw::stream::StreamRc::new(core.clone(), "av-relay-mic", props)?;
        let state = MicState {
            consumer,
            on_usage,
            in_use,
            rate_match: std::ptr::null_mut(),
            dll: Dll::new(dll::BW_MIN, 128, RATE),
            playout: Playout::default(),
            scratch: Vec::new(),
            cycles: 0,
            counters,
        };
        let listener = stream
            .add_local_listener_with_user_data(state)
            .state_changed(|_stream, state, _old, new| {
                let in_use = matches!(new, pw::stream::StreamState::Streaming);
                if let pw::stream::StreamState::Error(e) = &new {
                    log::error!("virtual microphone: {e}");
                }
                if in_use != state.in_use.load(Ordering::Relaxed) {
                    state.in_use.store(in_use, Ordering::Relaxed);
                    state.consumer.clear();
                    state.playout = Playout::default();
                    state.dll.reset();
                    log::info!("virtual microphone {}", if in_use { "in use" } else { "idle" });
                    (state.on_usage)(in_use);
                }
            })
            .io_changed(|_stream, state, id, area, _size| {
                if id == spa::sys::SPA_IO_RateMatch {
                    state.rate_match = area.cast();
                    log::debug!("virtual mic: rate matching {}", if area.is_null() { "gone" } else { "available" });
                }
            })
            .process(|stream, state| process(stream, state))
            .register()?;

        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.set_format(spa::param::audio::AudioFormat::F32LE);
        info.set_rate(RATE);
        info.set_channels(1);
        let mut position = [0; spa::param::audio::MAX_CHANNELS];
        position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
        info.set_position(position);
        let format = spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(spa::pod::Object {
                type_: spa::sys::SPA_TYPE_OBJECT_Format,
                id: spa::sys::SPA_PARAM_EnumFormat,
                properties: info.into(),
            }),
        )
        .map_err(|e| anyhow!("building the audio format: {e:?}"))?
        .0
        .into_inner();
        let mut params = [Pod::from_bytes(&format).context("audio format pod")?];
        // No AUTOCONNECT: a source waits for apps to link to it. No
        // RT_PROCESS: every callback then runs on this thread, so the
        // listener's state is never shared with PipeWire's data thread.
        stream.connect(spa::utils::Direction::Output, None, pw::stream::StreamFlags::MAP_BUFFERS, &mut params)?;
        log::info!("virtual microphone '{MIC_DESCRIPTION}' created");
        Ok(Self { _listener: listener, stream })
    }
}

/// Fills one buffer for the app recording from the mic, keeping the queue at
/// its target and steering PipeWire's resampler so the sender's clock and this
/// computer's audio clock don't drift apart.
fn process(stream: &pw::stream::Stream, state: &mut MicState) {
    let Some(mut buffer) = stream.dequeue_buffer() else { return };
    let requested = buffer.requested() as usize;
    let datas = buffer.datas_mut();
    let Some(data) = datas.first_mut() else { return };
    let stride = std::mem::size_of::<f32>();
    let frames = {
        let Some(slice) = data.data() else { return };
        let max = slice.len() / stride;
        let wanted = if requested > 0 { requested.min(max) } else { max };
        state.scratch.clear();
        state.scratch.resize(wanted, 0.0);

        if let Some(error) = state.playout.fill(&mut state.consumer, &mut state.scratch, &state.counters) {
            let corr = state.dll.update(error.clamp(-MAX_ERROR, MAX_ERROR));
            state.cycles += 1;
            if state.cycles % 256 == 0 {
                log::debug!("virtual mic: fill error {error}, quantum {wanted}, correction {corr:.6}");
            }
            if let Some(rm) = unsafe { state.rate_match.as_mut() } {
                rm.flags |= spa::sys::SPA_IO_RATE_MATCH_FLAG_ACTIVE;
                rm.rate = 1.0 / corr;
            }
        }
        for (dst, s) in slice.chunks_exact_mut(stride).zip(&state.scratch) {
            dst.copy_from_slice(&s.to_le_bytes());
        }
        wanted
    };
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = stride as i32;
    *chunk.size_mut() = (frames * stride) as u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_node_values() {
        assert_eq!(node_name_from_json(r#"{"name":"alsa_input.pci-0000.analog-stereo"}"#).as_deref(), Some("alsa_input.pci-0000.analog-stereo"));
        assert_eq!(node_name_from_json("not json"), None);
        assert_eq!(node_name_from_json(r#"{"other":1}"#), None);
    }
}
