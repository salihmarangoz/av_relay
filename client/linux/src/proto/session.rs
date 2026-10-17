//! The client side of a session (docs/protocol.md, section 6): a Noise IK
//! handshake with the paired sender's key, then JSON control messages, one per
//! Noise transport message.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};

use super::{ClientMsg, ServerMsg, read_frame, send_preamble, write_frame};
use crate::crypto::{SESSION_NOISE, SESSION_PROLOGUE, StaticKey};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Couldn't authenticate the sender. If AV Relay was reset on the sender, pair again.")]
    AuthFailed,
    #[error("The sender doesn't know this computer any more. Pair again.")]
    NotPaired,
    #[error("The sender is busy with another computer.")]
    Busy,
    #[error("The sender runs an AV Relay version that doesn't speak this protocol version.")]
    UnsupportedVersion,
    #[error("The sender didn't answer in time.")]
    Timeout,
    #[error("The sender sent something unexpected: {0}")]
    Protocol(String),
    #[error("Connection lost: {0}")]
    Io(#[from] io::Error),
}

/// Opens a session on `stream` (already connected to the sender). Returns the
/// two halves of the encrypted control channel.
pub async fn connect<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    me: &StaticKey,
    server_key: &[u8; 32],
) -> Result<(SessionReader<S>, SessionWriter<S>), SessionError> {
    connect_with(stream, me, server_key, None).await
}

pub(crate) async fn connect_with<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    me: &StaticKey,
    server_key: &[u8; 32],
    fixed_ephemeral: Option<&[u8; 32]>,
) -> Result<(SessionReader<S>, SessionWriter<S>), SessionError> {
    let handshake = async {
        send_preamble(&mut stream, "session").await?;
        let mut builder = snow::Builder::new(SESSION_NOISE.parse().expect("valid Noise name"))
            .local_private_key(me.private())
            .and_then(|b| b.remote_public_key(server_key))
            .and_then(|b| b.prologue(SESSION_PROLOGUE))
            .expect("valid keys");
        if let Some(e) = fixed_ephemeral {
            builder = builder.fixed_ephemeral_key_for_testing_only(e);
        }
        let mut noise = builder.build_initiator().expect("valid Noise state");
        let mut buf = vec![0u8; super::MAX_FRAME];
        let n = noise.write_message(&[], &mut buf).map_err(|_| SessionError::AuthFailed)?;
        write_frame(&mut stream, &buf[..n]).await?;

        let reply = match read_frame(&mut stream).await {
            Ok(f) => f,
            // A sender that can't decrypt message 1 (it was reset) just closes.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(SessionError::AuthFailed),
            Err(e) => return Err(e.into()),
        };
        if noise.read_message(&reply, &mut buf).is_err() {
            // Before Noise, the server may still refuse in plaintext.
            let msg: serde_json::Value = serde_json::from_slice(&reply).unwrap_or_default();
            return Err(match super::plaintext_error(&msg) {
                Some("unsupported-version") => SessionError::UnsupportedVersion,
                _ => SessionError::AuthFailed,
            });
        }
        noise.into_transport_mode().map_err(|_| SessionError::AuthFailed)
    };
    let transport = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await.map_err(|_| SessionError::Timeout)??;
    let transport = Arc::new(Mutex::new(transport));
    let (r, w) = tokio::io::split(stream);
    Ok((SessionReader { inner: r, noise: transport.clone() }, SessionWriter { inner: w, noise: transport }))
}

pub struct SessionReader<S> {
    inner: ReadHalf<S>,
    noise: Arc<Mutex<snow::TransportState>>,
}

pub struct SessionWriter<S> {
    inner: WriteHalf<S>,
    noise: Arc<Mutex<snow::TransportState>>,
}

impl<S: AsyncRead + AsyncWrite> SessionReader<S> {
    /// The next message from the sender. `error` messages for this client's
    /// pairing state come back as errors.
    pub async fn recv(&mut self) -> Result<ServerMsg, SessionError> {
        let frame = read_frame(&mut self.inner).await?;
        let mut buf = vec![0u8; frame.len()];
        let n = self
            .noise
            .lock()
            .unwrap()
            .read_message(&frame, &mut buf)
            .map_err(|_| SessionError::Protocol("a message failed to decrypt".into()))?;
        let msg: ServerMsg =
            serde_json::from_slice(&buf[..n]).map_err(|e| SessionError::Protocol(format!("bad message: {e}")))?;
        match &msg {
            ServerMsg::Error { code, .. } if code == "not-paired" => Err(SessionError::NotPaired),
            ServerMsg::Error { code, .. } if code == "busy" => Err(SessionError::Busy),
            _ => Ok(msg),
        }
    }
}

impl<S: AsyncRead + AsyncWrite> SessionWriter<S> {
    pub async fn send(&mut self, msg: &ClientMsg) -> Result<(), SessionError> {
        let json = serde_json::to_vec(msg).map_err(io::Error::from)?;
        let mut buf = vec![0u8; json.len() + 16];
        let n = self
            .noise
            .lock()
            .unwrap()
            .write_message(&json, &mut buf)
            .map_err(|e| SessionError::Protocol(format!("encrypting: {e}")))?;
        write_frame(&mut self.inner, &buf[..n]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::test_vectors::{hex32, load};
    use crate::proto::VideoFormat;
    use crate::proto::pairing::tests::fake_server;

    #[tokio::test]
    async fn session_matches_the_vector_on_the_wire() {
        let v = load("session.json");
        let (client, server) = tokio::io::duplex(1 << 16);
        let server = tokio::spawn(fake_server(server, v["wire"].as_array().unwrap().clone()));

        let me = StaticKey::from_private(hex32(&v["noise"]["client_static_private"]));
        let server_key = hex32(&v["noise"]["server_static_public"]);
        let eph = hex32(&v["noise"]["client_ephemeral_private"]);
        let (mut reader, mut writer) = connect_with(client, &me, &server_key, Some(&eph)).await.unwrap();
        writer
            .send(&ClientMsg::Hello {
                name: "ubuntu-desktop".into(),
                app: "av-relay 0.1.0 linux".into(),
                video: VideoFormat { width: 1280, height: 720, fps: 30 },
            })
            .await
            .unwrap();
        let welcome = reader.recv().await.unwrap();
        server.await.unwrap();
        let ServerMsg::Welcome { name, device_id, .. } = welcome else { panic!("{welcome:?}") };
        assert_eq!(name, "MacBook Air");
        let paired = load("pairing.json");
        assert_eq!(device_id.as_deref(), paired["device_id"]["server"].as_str());
    }

    #[tokio::test]
    async fn stale_server_key_is_reported() {
        let (client, mut server) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            // Preamble and message 1 arrive; a reset sender can't decrypt it and hangs up.
            read_frame(&mut server).await.unwrap();
            read_frame(&mut server).await.unwrap();
        });
        let Err(err) = connect(client, &StaticKey::generate(), &[9u8; 32]).await else { panic!("connected") };
        assert!(matches!(err, SessionError::AuthFailed), "{err:?}");
    }
}
