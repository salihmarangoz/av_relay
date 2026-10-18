//! Receiving the sender's media (docs/protocol.md, section 8): a WebRTC peer
//! connection through GStreamer's webrtcbin, always the answerer, with the
//! H.264 video decoded into the virtual camera's format and the Opus audio
//! into the virtual microphone's.
//!
//! Signaling goes through the control channel: the caller feeds the sender's
//! offers and ICE candidates in and sends out what comes back as
//! [`MediaEvent`]s.

pub mod delay;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_rtp as gst_rtp;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;

use crate::proto::{IceCandidate, VideoFormat};

use delay::Spread;

const TWCC_URI: &str = "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";
/// H.264 level 5.1: what the client decodes (protocol.md 8.2).
const H264_RECEIVE_LEVEL: &str = "33";

/// A frame later than this is skipped rather than shown.
const VIDEO_MAX_LATENESS: Duration = Duration::from_millis(100);
/// At most one keyframe request per this long.
const KEYFRAME_REQUEST_EVERY: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub enum MediaEvent {
    /// The answer to the latest offer: send it to the sender.
    Answer(String),
    /// A local ICE candidate: send it to the sender.
    Candidate(IceCandidate),
    /// The peer connection's state changed (new, connecting, connected,
    /// disconnected, failed, closed).
    State(String),
    /// Something broke; the receiver should be torn down.
    Error(String),
}

/// Where decoded media goes.
pub struct Sinks {
    /// One YUYV frame in the requested format.
    pub video: Box<dyn Fn(&[u8]) + Send + Sync>,
    /// Mono 48 kHz samples.
    pub audio: Box<dyn Fn(&[f32]) + Send + Sync>,
}

/// One received stream's counters, from the RTP session and the stream's
/// jitter buffer. They count from the start of the session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InboundStats {
    pub ssrc: u32,
    pub video: bool,
    pub packets_received: u64,
    pub packets_lost: i64,
    /// RTP interarrival jitter, in milliseconds.
    pub jitter_ms: f64,
    /// Feedback sent to the sender: NACKs (retransmission requests) and
    /// PLIs/FIRs (keyframe requests).
    pub nacks: u64,
    pub plis: u64,
    /// Packets the jitter buffer gave up waiting for, packets that came
    /// after it had given up, and retransmissions it asked for and got.
    pub jb_lost: u64,
    pub jb_late: u64,
    pub rtx_requests: u64,
    pub rtx_ok: u64,
}

/// The received streams: their jitter buffers and delay spreads, by SSRC,
/// which SSRCs are video, and the video's sink.
struct Streams {
    jitter_buffers: Vec<(u32, gst::glib::WeakRef<gst::Element>)>,
    video: Vec<u32>,
    spreads: HashMap<u32, Spread>,
    /// Each payload type's RTP clock rate, from the offer.
    clock_rates: HashMap<u8, u32>,
    video_sink: Option<gst_app::AppSink>,
    /// A sink was added: the pipeline's latency must be worked out again
    /// (not from the streaming thread that added it).
    latency_stale: bool,
    /// Arrival times count from here.
    epoch: Instant,
}

impl Default for Streams {
    fn default() -> Self {
        Self {
            jitter_buffers: Vec::new(),
            video: Vec::new(),
            spreads: HashMap::new(),
            clock_rates: HashMap::new(),
            video_sink: None,
            latency_stale: false,
            epoch: Instant::now(),
        }
    }
}

impl Streams {
    /// webrtcbin turns retransmission requests on only when it can map a
    /// stream to its transceiver, which fails with bundling (the jitter
    /// buffers come up with do-retransmission off). Video is negotiated with
    /// NACK and RTX, so turn it on here once a stream is known to be video,
    /// for gaps only. By default the
    /// jitter buffer also expects each next packet on time and gives up on
    /// it at the deadline; Wi-Fi often holds everything back for 100-300 ms,
    /// and those packets, declared lost and then thrown away on arrival,
    /// broke the picture and set off keyframe requests.
    fn set_up_video(&self) {
        for (ssrc, jb) in &self.jitter_buffers {
            if let Some(jb) = jb.upgrade().filter(|_| self.video.contains(ssrc)) {
                if !jb.property::<bool>("do-retransmission") {
                    jb.set_property("rtx-next-seqnum", false);
                    jb.set_property("do-retransmission", true);
                    log::debug!("video jitter buffer for ssrc {ssrc}: retransmission requests on");
                }
            }
        }
    }
}

fn uint(s: &gst::StructureRef, field: &str) -> u64 {
    s.get::<u64>(field)
        .or_else(|_| s.get::<u32>(field).map(u64::from))
        .or_else(|_| s.get::<i64>(field).map(|v| v.max(0) as u64))
        .or_else(|_| s.get::<i32>(field).map(|v| v.max(0) as u64))
        .unwrap_or(0)
}

pub struct Receiver {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    video: gst_webrtc::WebRTCRTPTransceiver,
    audio: gst_webrtc::WebRTCRTPTransceiver,
    events: Arc<dyn Fn(MediaEvent) + Send + Sync>,
    rtpbin: Option<gst::Element>,
    streams: Arc<Mutex<Streams>>,
}

impl Receiver {
    /// Sets up the peer connection. `local_ip` is this end of the control
    /// connection: ICE gathers candidates only there (UDP, host only), so
    /// media takes the path the user chose. `delay` is the playout delay to
    /// start with (see [`Receiver::set_delay`]).
    pub fn new(
        local_ip: Option<IpAddr>,
        video: VideoFormat,
        delay: Duration,
        sinks: Sinks,
        events: impl Fn(MediaEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        gst::init().context("initialising GStreamer")?;
        let events: Arc<dyn Fn(MediaEvent) + Send + Sync> = Arc::new(events);
        let pipeline = gst::Pipeline::with_name("av-relay-media");
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .property_from_str("bundle-policy", "max-bundle")
            .property("latency", delay.as_millis() as u32)
            .build()
            .context("webrtcbin is missing (install gstreamer1.0-plugins-bad and gstreamer1.0-nice)")?;
        pipeline.add(&webrtc)?;

        // Each received stream gets a jitter buffer; keep them (for stats()
        // and the delay), and time what reaches them (for the spreads).
        let rtpbin = webrtc.downcast_ref::<gst::Bin>().and_then(|b| b.by_name("rtpbin"));
        let streams = Arc::new(Mutex::new(Streams::default()));
        if let Some(rtpbin) = &rtpbin {
            rtpbin.connect("new-jitterbuffer", true, {
                let streams = streams.clone();
                move |values| {
                    let jb = values[1].get::<gst::Element>().ok()?;
                    let ssrc = values[3].get::<u32>().ok()?;
                    log::debug!(
                        "jitter buffer for ssrc {ssrc}: latency {} ms, do-lost {}, do-retransmission {}, mode {:?}",
                        jb.property::<u32>("latency"),
                        jb.property::<bool>("do-lost"),
                        jb.property::<bool>("do-retransmission"),
                        jb.property_value("mode"),
                    );
                    if let Some(pad) = jb.static_pad("sink") {
                        let streams = streams.clone();
                        pad.add_probe(gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST, move |_, info| {
                            let mut streams = streams.lock().unwrap();
                            let streams = &mut *streams;
                            let arrival = streams.epoch.elapsed().as_secs_f64();
                            let mut observe = |buffer: &gst::BufferRef| {
                                let Ok(rtp) = gst_rtp::RTPBuffer::from_buffer_readable(buffer) else { return };
                                // Padding-only packets are bandwidth probes.
                                if rtp.payload_size() == 0 {
                                    return;
                                }
                                let Some(&rate) = streams.clock_rates.get(&rtp.payload_type()) else { return };
                                let spread = streams.spreads.entry(ssrc).or_insert_with(|| Spread::new(rate));
                                spread.observe(rtp.seq(), rtp.timestamp(), arrival);
                            };
                            match &info.data {
                                Some(gst::PadProbeData::Buffer(b)) => observe(b),
                                Some(gst::PadProbeData::BufferList(list)) => list.iter().for_each(&mut observe),
                                _ => {}
                            }
                            gst::PadProbeReturn::Ok
                        });
                    }
                    let mut streams = streams.lock().unwrap();
                    streams.jitter_buffers.push((ssrc, jb.downgrade()));
                    streams.set_up_video();
                    None
                }
            });
        }

        let ice = webrtc.property::<gst::glib::Object>("ice-agent");
        ice.set_property("ice-tcp", false);
        if let Some(ip) = local_ip {
            if !ice.emit_by_name::<bool>("add-local-ip-address", &[&ip.to_string()]) {
                log::warn!("ICE wouldn't restrict itself to {ip}; using all addresses");
            }
        }

        // Accept only what the spec offers, so the answer never picks another
        // codec: H.264 (packetization mode 1) with NACK/PLI, and Opus. Each
        // offer adds its transport-cc extension (see handle_offer).
        let recvonly = gst_webrtc::WebRTCRTPTransceiverDirection::Recvonly;
        let video_t = webrtc.emit_by_name::<gst_webrtc::WebRTCRTPTransceiver>("add-transceiver", &[&recvonly, &video_caps(None)]);
        video_t.set_property("do-nack", true);
        let audio_t = webrtc.emit_by_name::<gst_webrtc::WebRTCRTPTransceiver>("add-transceiver", &[&recvonly, &audio_caps(None)]);

        webrtc.connect("on-ice-candidate", false, {
            let events = events.clone();
            let webrtc = webrtc.downgrade();
            move |values| {
                let mline = values[1].get::<u32>().ok()?;
                let candidate = values[2].get::<String>().ok()?;
                let sdp_mid = webrtc.upgrade().and_then(|w| mid_of(&w, mline));
                events(MediaEvent::Candidate(IceCandidate { candidate, sdp_mid, sdp_mline_index: mline }));
                None
            }
        });
        webrtc.connect_notify(Some("connection-state"), {
            let events = events.clone();
            move |w, _| {
                let state = w.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
                let name = format!("{state:?}").to_lowercase();
                log::info!("media connection {name}");
                events(MediaEvent::State(name));
            }
        });

        let sinks = Arc::new(sinks);
        webrtc.connect_pad_added({
            let pipeline = pipeline.downgrade();
            let events = events.clone();
            let streams = streams.clone();
            move |_, pad| {
                let Some(pipeline) = pipeline.upgrade() else { return };
                match add_decoder(&pipeline, pad, video, &sinks) {
                    Ok(added) => {
                        let mut streams = streams.lock().unwrap();
                        streams.latency_stale = true;
                        if let Some((ssrc, sink)) = added {
                            streams.video.extend(ssrc);
                            streams.video_sink = Some(sink);
                            streams.set_up_video();
                        }
                    }
                    Err(e) => events(MediaEvent::Error(format!("setting up a decoder: {e:#}"))),
                }
            }
        });

        let bus = pipeline.bus().context("the pipeline has no bus")?;
        bus.set_sync_handler({
            let events = events.clone();
            move |_, msg| {
                match msg.view() {
                    gst::MessageView::Error(err) => {
                        let text = format!("{} ({})", err.error(), err.debug().unwrap_or_default());
                        log::error!("media pipeline: {text}");
                        events(MediaEvent::Error(text));
                    }
                    gst::MessageView::Warning(w) => log::warn!("media pipeline: {}", w.error()),
                    _ => {}
                }
                gst::BusSyncReply::Drop
            }
        });

        pipeline.set_state(gst::State::Playing).context("starting the media pipeline")?;
        Ok(Self { pipeline, webrtc, video: video_t, audio: audio_t, events, rtpbin, streams })
    }

    /// Takes the sender's offer and answers it (the answer comes back as
    /// [`MediaEvent::Answer`]).
    pub fn handle_offer(&self, sdp: &str) -> Result<()> {
        log::debug!("offer:\n{sdp}");
        let offer = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).map_err(|_| anyhow!("the offer isn't valid SDP"))?;
        // webrtcbin answers only the header extensions its codec preferences
        // list, and without transport-cc the sender's congestion control (GCC)
        // can't see queueing delay. Echo the offer's transport-cc ids.
        let twcc = |kind: &str| twcc_id(&offer, kind);
        self.video.set_property("codec-preferences", video_caps(twcc("video")));
        self.audio.set_property("codec-preferences", audio_caps(twcc("audio")));
        let profiles = h264_profiles(&offer);
        self.streams.lock().unwrap().clock_rates.extend(clock_rates(&offer));
        let offer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, offer);
        let webrtc = self.webrtc.clone();
        let events = self.events.clone();
        let promise = gst::Promise::with_change_func(move |reply| {
            if let Err(e) = remote_description_set(&webrtc, reply, &events, profiles) {
                log::error!("answering the offer: {e:#}");
                events(MediaEvent::Error(format!("answering the offer: {e:#}")));
            }
        });
        self.webrtc.emit_by_name::<()>("set-remote-description", &[&offer, &promise]);
        Ok(())
    }

    /// The received streams' statistics, audio first. Read from the RTP
    /// session and the jitter buffers: webrtcbin's own get-stats mixes the
    /// streams up when they are bundled.
    pub fn stats(&self) -> Vec<InboundStats> {
        let Some(session) = self
            .rtpbin
            .as_ref()
            .and_then(|r| r.emit_by_name::<Option<gst::glib::Object>>("get-internal-session", &[&0u32]))
        else {
            return Vec::new();
        };
        let session_stats = session.property::<gst::Structure>("stats");
        let sources: Vec<gst::Structure> = session_stats
            .get::<gst::glib::ValueArray>("source-stats")
            .map(|a| a.iter().filter_map(|v| v.get::<gst::Structure>().ok()).collect())
            .unwrap_or_default();
        let mut streams: Vec<InboundStats> = self
            .streams
            .lock()
            .unwrap()
            .jitter_buffers
            .iter()
            .filter_map(|(ssrc, jb)| {
                let jb = jb.upgrade()?;
                let src = sources.iter().find(|s| s.get::<u32>("ssrc") == Ok(*ssrc))?;
                let clock_rate = src.get::<i32>("clock-rate").unwrap_or(0);
                let j = jb.property::<gst::Structure>("stats");
                Some(InboundStats {
                    ssrc: *ssrc,
                    video: clock_rate == 90000,
                    packets_received: uint(src, "packets-received"),
                    packets_lost: src.get::<i32>("packets-lost").map(i64::from).unwrap_or(0),
                    jitter_ms: if clock_rate > 0 { uint(src, "jitter") as f64 * 1000.0 / clock_rate as f64 } else { 0.0 },
                    nacks: uint(src, "sent-nack-count"),
                    plis: uint(src, "sent-pli-count") + uint(src, "sent-fir-count"),
                    jb_lost: uint(&j, "num-lost"),
                    jb_late: uint(&j, "num-late"),
                    rtx_requests: uint(&j, "rtx-count"),
                    rtx_ok: uint(&j, "rtx-success-count"),
                })
            })
            .collect();
        streams.sort_by_key(|s| (s.video, s.ssrc));
        streams
    }

    /// The delay spreads (see [`delay::Spread`]) of the audio and the video
    /// streams, in seconds: the largest per kind.
    pub fn spreads(&self) -> (Option<f64>, Option<f64>) {
        let streams = self.streams.lock().unwrap();
        let max = |video: bool| {
            streams
                .spreads
                .iter()
                .filter(|(ssrc, _)| streams.video.contains(ssrc) == video)
                .filter_map(|(_, s)| s.spread())
                .reduce(f64::max)
        };
        (max(false), max(true))
    }

    /// Plays everything `delay` after it was captured (both jitter buffers'
    /// latency, which the synchronized sinks add to every timestamp; RTCP
    /// lines the two streams up), video a further `video_offset` later to
    /// match the audio's own queue after its sink.
    pub fn set_delay(&self, delay: Duration, video_offset: Duration) {
        let latency = delay.as_millis() as u32;
        let mut changed = false;
        {
            let mut streams = self.streams.lock().unwrap();
            changed |= std::mem::take(&mut streams.latency_stale);
            for (_, jb) in &streams.jitter_buffers {
                if let Some(jb) = jb.upgrade() {
                    if jb.property::<u32>("latency") != latency {
                        jb.set_property("latency", latency);
                        changed = true;
                    }
                }
            }
            if let Some(sink) = &streams.video_sink {
                let offset = video_offset.as_millis() as i64 * 1_000_000;
                if sink.property::<i64>("ts-offset") != offset {
                    sink.set_property("ts-offset", offset);
                }
            }
        }
        // For jitter buffers still to come.
        if self.webrtc.property::<u32>("latency") != latency {
            self.webrtc.set_property("latency", latency);
        }
        if changed {
            // The sinks take the new latency from here on.
            let _ = self.pipeline.recalculate_latency();
        }
    }

    /// Adds one of the sender's ICE candidates; an empty candidate ends them.
    pub fn add_candidate(&self, c: &IceCandidate) {
        if c.candidate.is_empty() {
            return;
        }
        self.webrtc.emit_by_name::<()>("add-ice-candidate", &[&c.sdp_mline_index, &c.candidate]);
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// After set-remote-description: creates the answer, sets it, and hands it
/// out.
fn remote_description_set(
    webrtc: &gst::Element,
    reply: Result<Option<&gst::StructureRef>, gst::PromiseError>,
    events: &Arc<dyn Fn(MediaEvent) + Send + Sync>,
    profiles: Vec<(String, String)>,
) -> Result<()> {
    check_promise(reply)?;
    tune_rtcp(webrtc);
    let webrtc_answer = webrtc.clone();
    let events = events.clone();
    let promise = gst::Promise::with_change_func(move |reply| {
        let result = (|| -> Result<()> {
            let reply = check_promise(reply)?.context("create-answer gave no reply")?;
            let answer = reply
                .value("answer")
                .map_err(|_| anyhow!("create-answer failed: {reply}"))?
                .get::<gst_webrtc::WebRTCSessionDescription>()?;
            let text = fix_h264_fmtp(&answer.sdp().as_text()?, &profiles);
            log::debug!("answer:\n{text}");
            let sdp = gst_sdp::SDPMessage::parse_buffer(text.as_bytes()).map_err(|_| anyhow!("re-parsing the answer"))?;
            let answer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, sdp);
            webrtc_answer.emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);
            events(MediaEvent::Answer(text));
            Ok(())
        })();
        if let Err(e) = result {
            log::error!("creating the answer: {e:#}");
            events(MediaEvent::Error(format!("creating the answer: {e:#}")));
        }
    });
    webrtc.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
    Ok(())
}

/// GStreamer 1.24's RTP session (unchanged upstream so far) times its next
/// regular RTCP report from the last RTCP packet of any kind, and only
/// regular reports carry report blocks. TWCC feedback goes out as early RTCP
/// on every video frame, far more often than the regular interval (~0.7 s),
/// so regular reports, and with them the sender's loss and RTT figures, never
/// came. Sending TWCC on a fixed 50 ms interval (as libwebrtc's own receiver
/// does at these bitrates) and allowing regular reports every 40 ms lets
/// them interleave: ~26 reports a second with report blocks, ~40 kbit/s of
/// RTCP in all, nothing on a LAN.
fn tune_rtcp(webrtc: &gst::Element) {
    const TWCC_INTERVAL: Duration = Duration::from_millis(50);
    const RTCP_MIN_INTERVAL: Duration = Duration::from_millis(40);
    const RTCP_BANDWIDTH: i32 = 1_000_000;
    let Some(rtpbin) = webrtc.downcast_ref::<gst::Bin>().and_then(|b| b.by_name("rtpbin")) else {
        return log::warn!("webrtcbin has no rtpbin; RTCP left at its defaults");
    };
    // With max-bundle everything is session 0; 1 exists only without bundling.
    for id in 0u32..2 {
        let Some(session) = rtpbin.emit_by_name::<Option<gst::glib::Object>>("get-internal-session", &[&id]) else { continue };
        let has = |name: &str| session.find_property(name).is_some();
        if has("twcc-feedback-interval") {
            session.set_property("twcc-feedback-interval", TWCC_INTERVAL.as_nanos() as u64);
        }
        if has("rtcp-min-interval") && has("rtcp-rr-bandwidth") && has("rtcp-rs-bandwidth") {
            session.set_property("rtcp-rr-bandwidth", RTCP_BANDWIDTH);
            session.set_property("rtcp-rs-bandwidth", RTCP_BANDWIDTH);
            session.set_property("rtcp-min-interval", RTCP_MIN_INTERVAL.as_nanos() as u64);
        }
    }
}

fn check_promise(reply: Result<Option<&gst::StructureRef>, gst::PromiseError>) -> Result<Option<&gst::StructureRef>> {
    let reply = reply.map_err(|e| anyhow!("promise failed: {e:?}"))?;
    if let Some(r) = reply {
        if let Ok(err) = r.get::<glib_error::Error>("error") {
            bail!("{err}");
        }
    }
    Ok(reply)
}

mod glib_error {
    pub use gstreamer::glib::Error;
}

fn video_caps(twcc: Option<u32>) -> gst::Caps {
    let mut caps = gst::Caps::builder("application/x-rtp")
        .field("media", "video")
        .field("encoding-name", "H264")
        .field("clock-rate", 90000i32)
        .field("packetization-mode", "1")
        .build();
    add_extmap(&mut caps, twcc);
    caps
}

fn audio_caps(twcc: Option<u32>) -> gst::Caps {
    let mut caps = gst::Caps::builder("application/x-rtp")
        .field("media", "audio")
        .field("encoding-name", "OPUS")
        .field("clock-rate", 48000i32)
        .build();
    add_extmap(&mut caps, twcc);
    caps
}

fn add_extmap(caps: &mut gst::Caps, id: Option<u32>) {
    if let (Some(id), Some(s)) = (id, caps.make_mut().structure_mut(0)) {
        s.set(format!("extmap-{id}"), TWCC_URI);
    }
}

/// The transport-cc header extension id of the offer's first `kind` section.
fn twcc_id(offer: &gst_sdp::SDPMessageRef, kind: &str) -> Option<u32> {
    let media = offer.medias().find(|m| m.media() == Some(kind))?;
    media.attributes().filter(|a| a.key() == "extmap").find_map(|a| {
        let value = a.value()?;
        let (id, uri) = value.split_once(' ')?;
        (uri.trim() == TWCC_URI).then(|| id.split('/').next()?.parse().ok()).flatten()
    })
}

/// Every payload type's RTP clock rate (a=rtpmap:<pt> <codec>/<rate>).
fn clock_rates(offer: &gst_sdp::SDPMessageRef) -> HashMap<u8, u32> {
    offer
        .medias()
        .flat_map(|m| m.attributes())
        .filter(|a| a.key() == "rtpmap")
        .filter_map(|a| {
            let (pt, codec) = a.value()?.split_once(' ')?;
            Some((pt.parse().ok()?, codec.split('/').nth(1)?.parse().ok()?))
        })
        .collect()
}

/// (payload type, profile-level-id) of every H.264 payload in the offer.
fn h264_profiles(offer: &gst_sdp::SDPMessageRef) -> Vec<(String, String)> {
    offer
        .medias()
        .flat_map(|m| m.attributes())
        .filter(|a| a.key() == "fmtp")
        .filter_map(|a| {
            let (pt, params) = a.value()?.split_once(' ')?;
            let plid = params.split(';').find_map(|p| p.trim().strip_prefix("profile-level-id="))?;
            Some((pt.to_string(), plid.to_string()))
        })
        .collect()
}

/// The H.264 fmtp of the answer must say what this client can receive:
/// the offered profile at level 5.1 (protocol.md 8.2), and
/// level-asymmetry-allowed=1 so the sender may send at that level whatever
/// level it offered. Without a profile-level-id the default (level 3.1)
/// would cap it at 720p.
fn fix_h264_fmtp(sdp: &str, offered: &[(String, String)]) -> String {
    let mut out = String::with_capacity(sdp.len() + 64);
    for line in sdp.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let Some(rest) = body.strip_prefix("a=fmtp:").filter(|_| body.contains("packetization-mode")) else {
            out.push_str(line);
            continue;
        };
        let (pt, params) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut params: Vec<String> = params
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty() && !p.starts_with("profile-level-id=") && !p.starts_with("level-asymmetry-allowed="))
            .map(str::to_owned)
            .collect();
        let profile = offered.iter().find(|(p, _)| p == pt).map(|(_, plid)| plid.as_str()).unwrap_or("42e01f");
        if let Some(profile) = profile.get(..4) {
            params.push(format!("profile-level-id={profile}{H264_RECEIVE_LEVEL}"));
        }
        params.push("level-asymmetry-allowed=1".to_owned());
        out.push_str(&format!("a=fmtp:{pt} {}", params.join(";")));
        out.push_str(&line[body.len()..]);
    }
    out
}

/// The mid of m-line `mline` in the current local description, if any.
fn mid_of(webrtc: &gst::Element, mline: u32) -> Option<String> {
    let desc = webrtc.property::<Option<gst_webrtc::WebRTCSessionDescription>>("local-description")?;
    let media = desc.sdp().media(mline)?;
    media.attribute_val("mid").map(str::to_owned)
}

/// Builds the decoding branch for a new stream from webrtcbin. For video,
/// returns the stream's SSRC and its sink.
fn add_decoder(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    video: VideoFormat,
    sinks: &Arc<Sinks>,
) -> Result<Option<(Option<u32>, gst_app::AppSink)>> {
    let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None))).context("the stream has no caps")?;
    log::debug!("new stream: {caps}");
    let media = caps.structure(0).and_then(|s| s.get::<&str>("media").ok()).unwrap_or_default().to_owned();
    let ssrc = caps.structure(0).and_then(|s| s.get::<u32>("ssrc").ok());
    let (elements, sink) = match media.as_str() {
        "video" => video_branch(video, sinks.clone())?,
        "audio" => audio_branch(sinks.clone())?,
        other => {
            log::warn!("ignoring a {other} stream");
            return Ok(None);
        }
    };
    let mut all: Vec<&gst::Element> = elements.iter().collect();
    let sink_el = sink.upcast_ref::<gst::Element>().clone();
    all.push(&sink_el);
    pipeline.add_many(all.iter().copied())?;
    gst::Element::link_many(all.iter().copied())?;
    for e in &all {
        e.sync_state_with_parent()?;
    }
    let first = elements.first().context("empty branch")?;
    pad.link(&first.static_pad("sink").context("no sink pad")?)?;
    log::info!("receiving {media}");
    Ok((media == "video").then_some((ssrc, sink)))
}

/// The camera's pixel format and its bytes per pixel pair: YUYV for
/// v4l2loopback; NV12 on Windows, which OBS Virtual Camera reads as is.
#[cfg(not(windows))]
const CAMERA_FORMAT: (&str, usize) = ("YUY2", 4);
#[cfg(windows)]
const CAMERA_FORMAT: (&str, usize) = ("NV12", 3);

/// H.264 → decoded → scaled (keeping the aspect ratio) into the camera's
/// exact format (CAMERA_FORMAT).
fn video_branch(video: VideoFormat, sinks: Arc<Sinks>) -> Result<(Vec<gst::Element>, gst_app::AppSink)> {
    let depay = gst::ElementFactory::make("rtph264depay")
        // Ask for a keyframe on loss and show nothing broken meanwhile.
        .property("request-keyframe", true)
        .property("wait-for-keyframe", true)
        .build()?;
    // The depayloader asks for "all headers", which the RTP session sends as
    // a FIR with the same sequence number every time, and the sender's WebRTC
    // ignores a FIR it has seen: after the first loss the picture stayed
    // frozen until the next natural keyframe. Ask with a PLI instead, and
    // not for every missing packet: each keyframe is a burst of ~100
    // packets, which a struggling Wi-Fi link doesn't need more of. The
    // depayloader asks once per loss; if the request or the keyframe is lost
    // too, ask again until a keyframe comes through.
    #[derive(Default)]
    struct Keyframes {
        last_request: Option<std::time::Instant>,
        waiting: bool,
        since: Option<std::time::Instant>,
    }
    let keyframes = Arc::new(Mutex::new(Keyframes::default()));
    let request = || {
        gst::event::CustomUpstream::new(gst::Structure::builder("GstForceKeyUnit").field("all-headers", false).build())
    };
    if let Some(pad) = depay.static_pad("sink") {
        let state = keyframes.clone();
        pad.add_probe(gst::PadProbeType::EVENT_UPSTREAM, move |_, info| {
            if let Some(gst::PadProbeData::Event(event)) = &mut info.data {
                if event.structure().is_some_and(|s| s.name() == "GstForceKeyUnit") {
                    let mut state = state.lock().unwrap();
                    state.waiting = true;
                    state.since.get_or_insert_with(std::time::Instant::now);
                    if state.last_request.is_some_and(|t| t.elapsed() < KEYFRAME_REQUEST_EVERY) {
                        // Swallowed. (Drop would free it twice: GStreamer
                        // logs a critical for every one.)
                        return gst::PadProbeReturn::Handled;
                    }
                    state.last_request = Some(std::time::Instant::now());
                    log::debug!("asking for a keyframe");
                    *event = request();
                }
            }
            gst::PadProbeReturn::Ok
        });
        let state = keyframes.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
            let again = {
                let state = state.lock().unwrap();
                state.waiting && state.last_request.is_none_or(|t| t.elapsed() >= KEYFRAME_REQUEST_EVERY)
            };
            if again {
                pad.push_event(request());
            }
            gst::PadProbeReturn::Ok
        });
    }
    if let Some(pad) = depay.static_pad("src") {
        let state = keyframes.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(gst::PadProbeData::Buffer(buffer)) = &info.data {
                if !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                    let mut state = state.lock().unwrap();
                    if let Some(since) = state.since.take() {
                        log::debug!("keyframe after {} ms", since.elapsed().as_millis());
                    }
                    state.waiting = false;
                }
            }
            gst::PadProbeReturn::Ok
        });
    }
    let parse = gst::ElementFactory::make("h264parse").build()?;
    // Software decoding: 1080p30 is cheap, and it never wakes a discrete GPU
    // or depends on the driver. Slice threads only: frame threads would
    // delay every frame.
    let decode = gst::ElementFactory::make("avdec_h264").property_from_str("thread-type", "slice").build()?;
    // What the sender actually sends (the sink scales it to the camera's
    // format): a lower resolution here means the sender adapted down.
    if let Some(pad) = decode.static_pad("src") {
        pad.connect_notify(Some("caps"), |pad, _| {
            if let Some(s) = pad.current_caps().as_ref().and_then(|c| c.structure(0)) {
                if let (Ok(w), Ok(h)) = (s.get::<i32>("width"), s.get::<i32>("height")) {
                    log::info!("receiving video at {w}x{h}");
                }
            }
        });
    }
    let convert = gst::ElementFactory::make("videoconvertscale").property("add-borders", true).build()?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", CAMERA_FORMAT.0)
        .field("width", video.width as i32)
        .field("height", video.height as i32)
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    let frame_size = video.width as usize * video.height as usize * CAMERA_FORMAT.1 / 2;
    // Synchronized: a frame goes out at its timestamp plus the pipeline's
    // latency (the playout delay), plus the offset that matches the audio's
    // own queue. A frame late by more than a few frames is skipped, so the
    // picture catches up at once after a stall longer than the delay.
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(true)
        .max_lateness(VIDEO_MAX_LATENESS.as_nanos() as i64)
        .qos(false)
        .max_buffers(2)
        .drop(true)
        .callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    if map.len() == frame_size {
                        (sinks.video)(&map);
                    } else {
                        log::warn!("decoded frame is {} bytes, expected {frame_size}", map.len());
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        )
        .build();
    Ok((vec![depay, parse, decode, convert], sink))
}

/// Opus → mono 48 kHz floats.
fn audio_branch(sinks: Arc<Sinks>) -> Result<(Vec<gst::Element>, gst_app::AppSink)> {
    let depay = gst::ElementFactory::make("rtpopusdepay").build()?;
    let decode = gst::ElementFactory::make("opusdec").property("plc", true).property("use-inband-fec", true).build()?;
    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("rate", crate::audio::RATE as i32)
        .field("channels", 1i32)
        .field("layout", "interleaved")
        .build();
    let scratch = Mutex::new(Vec::<f32>::new());
    // Synchronized, like the video: audio goes out at its timestamp plus
    // the playout delay. Late audio still goes out (the mic's queue drops
    // what it can't use).
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(true)
        .callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let mut samples = scratch.lock().unwrap();
                    samples.clear();
                    samples.extend(map.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
                    (sinks.audio)(&samples);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        )
        .build();
    Ok((vec![depay, decode, convert, resample], sink))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_answer_fmtp() {
        let offered = vec![("102".to_string(), "640c1f".to_string())];
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=fmtp:102 packetization-mode=1\r\na=fmtp:103 apt=102\r\n";
        let out = fix_h264_fmtp(sdp, &offered);
        assert!(out.contains("a=fmtp:102 packetization-mode=1;profile-level-id=640c33;level-asymmetry-allowed=1\r\n"), "{out}");
        assert!(out.contains("a=fmtp:103 apt=102\r\n"));
        assert_eq!(fix_h264_fmtp(&out, &offered), out);
    }

    #[test]
    fn finds_twcc_ids_and_profiles() {
        let sdp = "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n\
                   m=video 9 UDP/TLS/RTP/SAVPF 102\r\na=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\n\
                   a=extmap:4 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n\
                   a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
                   m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=extmap:5 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n";
        gst::init().unwrap();
        let msg = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).unwrap();
        assert_eq!(twcc_id(&msg, "video"), Some(4));
        assert_eq!(twcc_id(&msg, "audio"), Some(5));
        assert_eq!(h264_profiles(&msg), vec![("102".to_string(), "42e01f".to_string())]);
    }
}
