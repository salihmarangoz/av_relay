//! Receiving the Mac's media (docs/protocol.md, section 8): a WebRTC peer
//! connection through GStreamer's webrtcbin, always the answerer, with the
//! H.264 video decoded into the virtual camera's format and the Opus audio
//! into the virtual microphone's.
//!
//! Signaling goes through the control channel: the caller feeds the Mac's
//! offers and ICE candidates in and sends out what comes back as
//! [`MediaEvent`]s.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;

use crate::proto::{IceCandidate, VideoFormat};

const TWCC_URI: &str = "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";
/// H.264 level 5.1: what the client decodes (protocol.md 8.2).
const H264_RECEIVE_LEVEL: &str = "33";

/// Jitter buffer depth. On a LAN a retransmission takes a few ms, so this
/// leaves room for several NACK rounds and Wi-Fi hiccups without adding much
/// delay.
const JITTER_BUFFER_MS: u32 = 60;

#[derive(Debug)]
pub enum MediaEvent {
    /// The answer to the latest offer: send it to the Mac.
    Answer(String),
    /// A local ICE candidate: send it to the Mac.
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

pub struct Receiver {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    video: gst_webrtc::WebRTCRTPTransceiver,
    audio: gst_webrtc::WebRTCRTPTransceiver,
    events: Arc<dyn Fn(MediaEvent) + Send + Sync>,
}

impl Receiver {
    /// Sets up the peer connection. `local_ip` is this end of the control
    /// connection: ICE gathers candidates only there (UDP, host only), so
    /// media takes the path the user chose.
    pub fn new(
        local_ip: Option<IpAddr>,
        video: VideoFormat,
        sinks: Sinks,
        events: impl Fn(MediaEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        gst::init().context("initialising GStreamer")?;
        let events: Arc<dyn Fn(MediaEvent) + Send + Sync> = Arc::new(events);
        let pipeline = gst::Pipeline::with_name("av-relay-media");
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .property_from_str("bundle-policy", "max-bundle")
            .property("latency", JITTER_BUFFER_MS)
            .build()
            .context("webrtcbin is missing (install gstreamer1.0-plugins-bad and gstreamer1.0-nice)")?;
        pipeline.add(&webrtc)?;

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
            move |_, pad| {
                let Some(pipeline) = pipeline.upgrade() else { return };
                if let Err(e) = add_decoder(&pipeline, pad, video, &sinks) {
                    events(MediaEvent::Error(format!("setting up a decoder: {e:#}")));
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
        Ok(Self { pipeline, webrtc, video: video_t, audio: audio_t, events })
    }

    /// Takes the Mac's offer and answers it (the answer comes back as
    /// [`MediaEvent::Answer`]).
    pub fn handle_offer(&self, sdp: &str) -> Result<()> {
        let offer = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).map_err(|_| anyhow!("the offer isn't valid SDP"))?;
        // webrtcbin answers only the header extensions its codec preferences
        // list, and without transport-cc the Mac's congestion control (GCC)
        // can't see queueing delay. Echo the offer's transport-cc ids.
        let twcc = |kind: &str| twcc_id(&offer, kind);
        self.video.set_property("codec-preferences", video_caps(twcc("video")));
        self.audio.set_property("codec-preferences", audio_caps(twcc("audio")));
        let profiles = h264_profiles(&offer);
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

    /// Adds one of the Mac's ICE candidates; an empty candidate ends them.
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
/// so regular reports, and with them the Mac's loss and RTT figures, never
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
/// level-asymmetry-allowed=1 so the Mac may send at that level whatever
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

/// Builds the decoding branch for a new stream from webrtcbin.
fn add_decoder(pipeline: &gst::Pipeline, pad: &gst::Pad, video: VideoFormat, sinks: &Arc<Sinks>) -> Result<()> {
    let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None))).context("the stream has no caps")?;
    let media = caps.structure(0).and_then(|s| s.get::<&str>("media").ok()).unwrap_or_default().to_owned();
    let (elements, sink) = match media.as_str() {
        "video" => video_branch(video, sinks.clone())?,
        "audio" => audio_branch(sinks.clone())?,
        other => {
            log::warn!("ignoring a {other} stream");
            return Ok(());
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
    Ok(())
}

/// H.264 → decoded → scaled (keeping the aspect ratio) into the camera's
/// exact YUYV format.
fn video_branch(video: VideoFormat, sinks: Arc<Sinks>) -> Result<(Vec<gst::Element>, gst_app::AppSink)> {
    let depay = gst::ElementFactory::make("rtph264depay")
        // Ask for a keyframe on loss and show nothing broken meanwhile.
        .property("request-keyframe", true)
        .property("wait-for-keyframe", true)
        .build()?;
    let parse = gst::ElementFactory::make("h264parse").build()?;
    // Software decoding: 1080p30 is cheap, and it never wakes a discrete GPU
    // or depends on the driver. Slice threads only: frame threads would
    // delay every frame.
    let decode = gst::ElementFactory::make("avdec_h264").property_from_str("thread-type", "slice").build()?;
    let convert = gst::ElementFactory::make("videoconvertscale").property("add-borders", true).build()?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "YUY2")
        .field("width", video.width as i32)
        .field("height", video.height as i32)
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    let frame_size = video.width as usize * video.height as usize * 2;
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(false)
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
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(false)
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
