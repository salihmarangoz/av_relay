//! The client side of pairing (docs/protocol.md, section 5): connect, wait
//! for the Mac's `pair-start`, then (with the code the user typed) CPace and
//! a Noise XXpsk3 handshake that swaps the static keys.

use std::io;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;
use zeroize::Zeroizing;

use super::{b64, plaintext_error, read_frame, read_plaintext, send_preamble, write_frame};
use crate::crypto::cpace::{Cpace, CpaceError};
use crate::crypto::{self, PAIRING_NOISE, StaticKey};

/// The whole exchange must finish this long after the preamble; it includes
/// the user typing the code.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("The Mac isn't waiting for a pairing. On the Mac, choose Allow Pairing… in the AV Relay menu.")]
    NotPairing,
    #[error("The Mac is busy with another pairing attempt. Try again in a moment.")]
    Busy,
    #[error("The Mac runs an AV Relay version that doesn't speak this protocol version.")]
    UnsupportedVersion,
    #[error("Pairing failed. Check the code and try again; the Mac shows a new code each time.")]
    Failed,
    #[error("The pairing took too long. Start again on the Mac.")]
    Timeout,
    #[error("The code has 6 digits.")]
    BadCode,
    #[error("This isn't an AV Relay Mac, or it sent something unexpected: {0}")]
    Protocol(String),
    #[error("Connection lost: {0}")]
    Io(#[from] io::Error),
}

impl From<CpaceError> for PairError {
    fn from(_: CpaceError) -> Self {
        PairError::Failed
    }
}

impl From<snow::Error> for PairError {
    fn from(_: snow::Error) -> Self {
        PairError::Failed
    }
}

/// What the client tells the Mac about itself.
#[derive(Serialize, Debug, Clone)]
pub struct ClientInfo {
    pub name: String,
    pub app: String,
}

/// The Mac, as pairing left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedServer {
    pub public_key: [u8; 32],
    pub device_id: String,
    pub name: String,
    pub app: String,
}

/// Randomness for one pairing attempt; tests fix it.
pub(crate) struct AttemptRandom {
    pub sid_c: [u8; 16],
    pub scalar: Zeroizing<[u8; 32]>,
    pub ephemeral: Option<Zeroizing<[u8; 32]>>,
}

impl AttemptRandom {
    fn fresh() -> Self {
        let mut sid_c = [0u8; 16];
        let mut scalar = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut sid_c).expect("the system RNG failed");
        getrandom::fill(scalar.as_mut()).expect("the system RNG failed");
        Self { sid_c, scalar, ephemeral: None }
    }
}

/// A pairing connection waiting for the user to type the code.
pub struct PairingAttempt<S> {
    stream: S,
    sid_s: [u8; 16],
    deadline: tokio::time::Instant,
}

/// Opens a pairing connection on `stream` (already connected to the Mac)
/// and waits for the Mac to accept it.
pub async fn start<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) -> Result<PairingAttempt<S>, PairError> {
    let deadline = tokio::time::Instant::now() + PAIRING_TIMEOUT;
    send_preamble(&mut stream, "pair").await?;
    let msg = tokio::time::timeout_at(deadline, read_plaintext(&mut stream)).await.map_err(|_| PairError::Timeout)??;
    match plaintext_error(&msg) {
        Some("not-pairing") => return Err(PairError::NotPairing),
        Some("busy") => return Err(PairError::Busy),
        Some("unsupported-version") => return Err(PairError::UnsupportedVersion),
        Some(code) => return Err(PairError::Protocol(format!("error {code}"))),
        None => {}
    }
    if msg["type"] != "pair-start" {
        return Err(PairError::Protocol(format!("expected pair-start, got {}", msg["type"])));
    }
    let sid_s = b64::decode_exact(&msg["sid"]).ok_or_else(|| PairError::Protocol("bad sid".into()))?;
    Ok(PairingAttempt { stream, sid_s, deadline })
}

impl<S: AsyncRead + AsyncWrite + Unpin> PairingAttempt<S> {
    /// Completes the pairing with the code the user typed ("042137" or
    /// "042 137"). Uses the code up on the Mac, whatever the outcome.
    pub async fn finish(self, code: &str, me: &StaticKey, info: &ClientInfo) -> Result<PairedServer, PairError> {
        self.finish_with(code, me, info, AttemptRandom::fresh()).await
    }

    pub(crate) async fn finish_with(
        mut self,
        code: &str,
        me: &StaticKey,
        info: &ClientInfo,
        random: AttemptRandom,
    ) -> Result<PairedServer, PairError> {
        let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(PairError::BadCode);
        }
        let deadline = self.deadline;
        timeout(deadline - tokio::time::Instant::now(), self.exchange(digits.as_bytes(), me, info, random))
            .await
            .map_err(|_| PairError::Timeout)?
    }

    async fn exchange(
        &mut self,
        prs: &[u8],
        me: &StaticKey,
        info: &ClientInfo,
        random: AttemptRandom,
    ) -> Result<PairedServer, PairError> {
        let s = &mut self.stream;
        let sid = [random.sid_c, self.sid_s].concat();
        let cpace = Cpace::with_scalar(prs, &crypto::pairing_ci(), &sid, &random.scalar, true);
        let ya = cpace.share();
        #[derive(Serialize)]
        struct CpaceMsg {
            #[serde(rename = "type")]
            kind: &'static str,
            sid: String,
            #[serde(rename = "Y")]
            y: String,
        }
        let msg = CpaceMsg { kind: "cpace", sid: b64::encode(&random.sid_c), y: b64::encode(&ya) };
        write_frame(s, &serde_json::to_vec(&msg).map_err(io::Error::from)?).await?;

        let reply = read_plaintext(s).await?;
        // The window may have closed while the user typed, or someone else's
        // attempt got there first.
        match plaintext_error(&reply) {
            Some("not-pairing") => return Err(PairError::NotPairing),
            Some("busy") => return Err(PairError::Busy),
            Some(code) => return Err(PairError::Protocol(format!("error {code}"))),
            None => {}
        }
        if reply["type"] != "cpace" {
            return Err(PairError::Protocol(format!("expected cpace, got {}", reply["type"])));
        }
        let yb: [u8; 32] = b64::decode_exact(&reply["Y"]).ok_or(PairError::Failed)?;
        let isk = cpace.finish(&yb, b"", b"")?;
        let psk = crypto::pairing_psk(&isk);
        let prologue = crypto::pairing_prologue(&sid, &ya, &yb);

        let mut builder = snow::Builder::new(PAIRING_NOISE.parse().expect("valid Noise name"))
            .local_private_key(me.private())?
            .prologue(&prologue)?
            .psk(3, &psk)?;
        if let Some(e) = &random.ephemeral {
            builder = builder.fixed_ephemeral_key_for_testing_only(&e[..]);
        }
        let mut noise = builder.build_initiator()?;
        let mut buf = vec![0u8; super::MAX_FRAME];

        let n = noise.write_message(&[], &mut buf)?;
        write_frame(s, &buf[..n]).await?;
        let msg2 = read_frame(s).await?;
        noise.read_message(&msg2, &mut buf)?;
        let payload = serde_json::to_vec(info).map_err(io::Error::from)?;
        let n = noise.write_message(&payload, &mut buf)?;
        write_frame(s, &buf[..n]).await?;

        let server_key: [u8; 32] = noise
            .get_remote_static()
            .and_then(|k| k.try_into().ok())
            .ok_or_else(|| PairError::Protocol("no server key".into()))?;
        let mut noise = noise.into_transport_mode()?;
        // A Mac that used another code can't decrypt message 3 and hangs up.
        let paired = match read_frame(s).await {
            Ok(f) => f,
            Err(e) if matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset) => {
                return Err(PairError::Failed);
            }
            Err(e) => return Err(e.into()),
        };
        let n = noise.read_message(&paired, &mut buf)?;
        let paired: serde_json::Value =
            serde_json::from_slice(&buf[..n]).map_err(|e| PairError::Protocol(e.to_string()))?;
        if paired["type"] != "paired" {
            return Err(PairError::Protocol(format!("expected paired, got {}", paired["type"])));
        }
        let device_id = crypto::device_id(&server_key);
        if paired["device_id"].as_str().is_some_and(|id| id != device_id) {
            return Err(PairError::Protocol("the Mac's device ID doesn't match its key".into()));
        }
        Ok(PairedServer {
            public_key: server_key,
            device_id,
            name: paired["name"].as_str().unwrap_or("Mac").to_string(),
            app: paired["app"].as_str().unwrap_or_default().to_string(),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::crypto::test_vectors::{hex, hex32, load};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    /// Plays the Mac's side of a vector's wire list: checks every frame the
    /// client sends byte for byte and answers with the Mac's frames.
    pub async fn fake_mac(mut mac: DuplexStream, wire: Vec<serde_json::Value>) {
        for w in wire {
            let frame = hex(&w["frame"]);
            if w["dir"] == "c2s" {
                let mut got = vec![0u8; frame.len()];
                mac.read_exact(&mut got).await.unwrap();
                assert_eq!(hex::encode(&got), hex::encode(&frame), "client frame '{}'", w["what"]);
            } else {
                mac.write_all(&frame).await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn pairing_matches_the_vector_on_the_wire() {
        let v = load("pairing.json");
        let (client, mac) = tokio::io::duplex(1 << 16);
        let mac = tokio::spawn(fake_mac(mac, v["wire"].as_array().unwrap().clone()));

        let me = StaticKey::from_private(hex32(&v["noise"]["client_static_private"]));
        let random = AttemptRandom {
            sid_c: hex(&v["cpace"]["sid_c"]).try_into().unwrap(),
            scalar: Zeroizing::new(hex32(&v["cpace"]["ya"])),
            ephemeral: Some(Zeroizing::new(hex32(&v["noise"]["client_ephemeral_private"]))),
        };
        let info = ClientInfo { name: "ubuntu-desktop".into(), app: "av-relay 0.1.0 linux".into() };
        let attempt = start(client).await.unwrap();
        let code = v["code"].as_str().unwrap();
        let spaced = format!("{} {}", &code[..3], &code[3..]);
        let server = attempt.finish_with(&spaced, &me, &info, random).await.unwrap();
        mac.await.unwrap();

        assert_eq!(server.public_key.to_vec(), hex(&v["noise"]["server_static_public"]));
        assert_eq!(server.device_id, v["device_id"]["server"].as_str().unwrap());
        assert_eq!(server.name, "MacBook Air");
    }

    #[tokio::test]
    async fn wrong_code_fails_cleanly() {
        let v = load("pairing.json");
        let wire: Vec<_> = v["wire"].as_array().unwrap().iter().take(4).cloned().collect();
        let (client, mut mac) = tokio::io::duplex(1 << 16);
        let mac_side = tokio::spawn(async move {
            // pair-start and cpace as in the vector, then hang up like a Mac
            // whose code differs (it can't decrypt message 3).
            let mut buf = vec![0u8; 4096];
            for w in &wire {
                let frame = hex(&w["frame"]);
                if w["dir"] == "c2s" {
                    mac.read_exact(&mut buf[..2]).await.unwrap();
                    let len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
                    mac.read_exact(&mut buf[..len]).await.unwrap();
                } else {
                    mac.write_all(&frame).await.unwrap();
                }
            }
            let _ = mac.read(&mut buf).await;
            drop(mac);
        });
        let me = StaticKey::generate();
        let info = ClientInfo { name: "x".into(), app: "y".into() };
        let attempt = start(client).await.unwrap();
        let err = attempt.finish("123456", &me, &info).await.unwrap_err();
        mac_side.await.unwrap();
        assert!(matches!(err, PairError::Failed | PairError::Io(_)), "{err:?}");
    }

    #[tokio::test]
    async fn refusals_and_bad_codes() {
        let (client, mut mac) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut buf = [0u8; 256];
            let _ = mac.read(&mut buf).await;
            super::write_frame(&mut mac, br#"{"type":"error","code":"not-pairing"}"#).await.unwrap();
        });
        assert!(matches!(start(client).await, Err(PairError::NotPairing)));

        let (client, mut mac) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut buf = [0u8; 256];
            let _ = mac.read(&mut buf).await;
            super::write_frame(&mut mac, br#"{"type":"pair-start","sid":"YHA79qUIzXoAiuuPAx4lEA=="}"#).await.unwrap();
            let _ = mac.read(&mut buf).await;
        });
        let attempt = start(client).await.unwrap();
        let info = ClientInfo { name: "x".into(), app: "y".into() };
        let err = attempt.finish("12345", &StaticKey::generate(), &info).await.unwrap_err();
        assert!(matches!(err, PairError::BadCode));
    }
}
