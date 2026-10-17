//! The control channel's wire format (docs/protocol.md, sections 4 and 7):
//! length-prefixed frames, the plaintext preamble, and the JSON control
//! messages. Pairing and sessions live in their own modules.

pub mod pairing;
pub mod session;

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const DEFAULT_PORT: u16 = 48100;
pub const PROTOCOL_VERSION: u32 = 1;
/// Plaintext frames (before Noise) are never longer than this.
const MAX_PLAINTEXT_FRAME: usize = 1024;
/// Noise messages are at most 65535 bytes, and a frame holds one.
pub const MAX_FRAME: usize = 65535;

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Vec<u8>> {
    let len = r.read_u16().await? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    let len = u16::try_from(payload.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too long"))?;
    let mut buf = Vec::with_capacity(2 + payload.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf).await?;
    w.flush().await
}

#[derive(Serialize)]
struct Preamble<'a> {
    proto: &'a str,
    version: u32,
    mode: &'a str,
}

async fn send_preamble<W: AsyncWrite + Unpin>(w: &mut W, mode: &str) -> io::Result<()> {
    let preamble = Preamble { proto: "av-relay", version: PROTOCOL_VERSION, mode };
    write_frame(w, &serde_json::to_vec(&preamble)?).await
}

async fn read_plaintext<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<serde_json::Value> {
    let frame = read_frame(r).await?;
    if frame.len() > MAX_PLAINTEXT_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "plaintext frame too long"));
    }
    serde_json::from_slice(&frame).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A plaintext `error` the server sends before Noise starts.
fn plaintext_error(msg: &serde_json::Value) -> Option<&str> {
    (msg["type"] == "error").then(|| msg["code"].as_str().unwrap_or("internal"))
}

/// The format the client wants for its virtual camera.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct IceCandidate {
    pub candidate: String,
    #[serde(rename = "sdpMid", default)]
    pub sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex", default)]
    pub sdp_mline_index: u32,
}

/// Messages the client sends in a session.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ClientMsg {
    Hello { name: String, app: String, video: VideoFormat },
    Answer { sdp: String },
    Ice(IceCandidate),
    Media { video: bool, audio: bool },
    IceRestart,
    Ping { id: u64 },
    Unpair,
    Bye { reason: String },
}

/// Messages the server sends in a session. Unknown types are ignored.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerMsg {
    Welcome {
        name: String,
        app: String,
        #[serde(default)]
        device_id: Option<String>,
    },
    Offer {
        sdp: String,
    },
    Ice(IceCandidate),
    MediaState {
        video: DeviceState,
        audio: DeviceState,
    },
    Pong {
        id: u64,
    },
    Bye {
        reason: String,
    },
    Error {
        code: String,
        #[serde(default)]
        message: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CaptureState {
    Off,
    Starting,
    On,
    Error,
    #[serde(other)]
    Unknown,
}

/// The state of the sender's camera or microphone, from `media-state`.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DeviceState {
    pub state: CaptureState,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub fps: Option<u32>,
}

/// Base64 (standard alphabet, padded) for binary values in JSON.
mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    pub fn encode(bytes: &[u8]) -> String {
        STANDARD.encode(bytes)
    }

    pub fn decode_exact<const N: usize>(value: &serde_json::Value) -> Option<[u8; N]> {
        STANDARD.decode(value.as_str()?).ok()?.try_into().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_as_specified() {
        let hello = ClientMsg::Hello {
            name: "ubuntu-desktop".into(),
            app: "av-relay 0.1.0 linux".into(),
            video: VideoFormat { width: 1280, height: 720, fps: 30 },
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"type":"hello","name":"ubuntu-desktop","app":"av-relay 0.1.0 linux","video":{"width":1280,"height":720,"fps":30}}"#
        );
        assert_eq!(serde_json::to_string(&ClientMsg::IceRestart).unwrap(), r#"{"type":"ice-restart"}"#);
        let ice = ClientMsg::Ice(IceCandidate { candidate: "candidate:1".into(), sdp_mid: Some("0".into()), sdp_mline_index: 0 });
        assert_eq!(
            serde_json::to_string(&ice).unwrap(),
            r#"{"type":"ice","candidate":"candidate:1","sdpMid":"0","sdpMLineIndex":0}"#
        );

        let state: ServerMsg = serde_json::from_str(
            r#"{"type":"media-state",
                "video":{"state":"on","device":"FaceTime HD Camera","width":1280,"height":720,"fps":30},
                "audio":{"state":"error","device":"MacBook Air Microphones","error":"permission-denied"}}"#,
        )
        .unwrap();
        let ServerMsg::MediaState { video, audio } = state else { panic!() };
        assert_eq!(video.state, CaptureState::On);
        assert_eq!(audio.error.as_deref(), Some("permission-denied"));

        let bare: ServerMsg = serde_json::from_str(r#"{"type":"ice","candidate":""}"#).unwrap();
        assert!(matches!(bare, ServerMsg::Ice(IceCandidate { sdp_mid: None, sdp_mline_index: 0, .. })));

        let future: ServerMsg = serde_json::from_str(r#"{"type":"something-new","x":1}"#).unwrap();
        assert_eq!(future, ServerMsg::Unknown);
        let welcome: ServerMsg =
            serde_json::from_str(r#"{"type":"welcome","name":"Mac","app":"AV Relay","extra":true}"#).unwrap();
        assert!(matches!(welcome, ServerMsg::Welcome { device_id: None, .. }));
    }
}
