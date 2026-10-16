//! The crypto the control channel needs: CPace for pairing, Noise for the
//! channel itself, and the device identity (docs/protocol.md, sections 2, 5
//! and 6).

pub mod cpace;

use std::fmt;

use curve25519_dalek::montgomery::MontgomeryPoint;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const PAIRING_NOISE: &str = "Noise_XXpsk3_25519_ChaChaPoly_SHA256";
pub const SESSION_NOISE: &str = "Noise_IK_25519_ChaChaPoly_SHA256";
pub const SESSION_PROLOGUE: &[u8] = b"av-relay/1 session";
const PAIRING_PROLOGUE: &[u8] = b"av-relay/1 pair";
const PAIRING_CI_LABEL: &[u8] = b"av-relay/1 pairing";
const PSK_INFO: &[u8] = b"av-relay/1 pairing psk";

/// A device's long-term X25519 key pair: its identity.
#[derive(Clone)]
pub struct StaticKey {
    private: Zeroizing<[u8; 32]>,
    public: [u8; 32],
}

impl StaticKey {
    pub fn generate() -> Self {
        let mut private = Zeroizing::new([0u8; 32]);
        getrandom::fill(private.as_mut()).expect("the system RNG failed");
        Self::from_private(*private)
    }

    pub fn from_private(private: [u8; 32]) -> Self {
        let public = MontgomeryPoint::mul_base_clamped(private).to_bytes();
        Self { private: Zeroizing::new(private), public }
    }

    pub fn private(&self) -> &[u8; 32] {
        &self.private
    }

    pub fn public(&self) -> &[u8; 32] {
        &self.public
    }
}

impl fmt::Debug for StaticKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StaticKey({})", device_id(&self.public))
    }
}

/// The device ID: the first 8 bytes of SHA-256 over the static public key,
/// in hex.
pub fn device_id(public: &[u8; 32]) -> String {
    hex_lower(&Sha256::digest(public)[..8])
}

/// CPace's channel identifier for pairing.
pub fn pairing_ci() -> Vec<u8> {
    cpace::lv_cat(&[PAIRING_CI_LABEL, b"client", b"server"])
}

/// The Noise PSK for pairing, derived from CPace's ISK.
pub fn pairing_psk(isk: &[u8; 64]) -> Zeroizing<[u8; 32]> {
    let mut psk = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, isk).expand(PSK_INFO, psk.as_mut()).expect("32 bytes is a valid HKDF length");
    psk
}

pub fn pairing_prologue(sid: &[u8], ya: &[u8; 32], yb: &[u8; 32]) -> Vec<u8> {
    [PAIRING_PROLOGUE, sid, ya, yb].concat()
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(crate) mod test_vectors {
    use std::path::PathBuf;

    pub fn load(name: &str) -> serde_json::Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors").join(name);
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    pub fn hex(v: &serde_json::Value) -> Vec<u8> {
        ::hex::decode(v.as_str().unwrap()).unwrap()
    }

    pub fn hex32(v: &serde_json::Value) -> [u8; 32] {
        hex(v).try_into().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_vectors::{hex, hex32, load};
    use super::*;

    #[test]
    fn device_ids_and_psk() {
        let p = load("pairing.json");
        let client = StaticKey::from_private(hex32(&p["noise"]["client_static_private"]));
        let server = StaticKey::from_private(hex32(&p["noise"]["server_static_private"]));
        assert_eq!(client.public().to_vec(), hex(&p["noise"]["client_static_public"]));
        assert_eq!(server.public().to_vec(), hex(&p["noise"]["server_static_public"]));
        assert_eq!(device_id(client.public()), p["device_id"]["client"].as_str().unwrap());
        assert_eq!(device_id(server.public()), p["device_id"]["server"].as_str().unwrap());

        let isk: [u8; 64] = hex(&p["cpace"]["ISK"]).try_into().unwrap();
        assert_eq!(pairing_psk(&isk).to_vec(), hex(&p["psk"]));
        let sid = hex(&p["cpace"]["sid"]);
        let prologue = pairing_prologue(&sid, &hex32(&p["cpace"]["Ya"]), &hex32(&p["cpace"]["Yb"]));
        assert_eq!(prologue, hex(&p["noise"]["prologue"]));
    }

    /// Runs a cacophony-style vector through snow: both sides, every message.
    fn run_noise_vector(v: &serde_json::Value) {
        let params: snow::params::NoiseParams = v["protocol_name"].as_str().unwrap().parse().unwrap();
        let (init_static, resp_static) = (hex(&v["init_static"]), hex(&v["resp_static"]));
        let (init_eph, resp_eph) = (hex(&v["init_ephemeral"]), hex(&v["resp_ephemeral"]));
        let (init_prologue, resp_prologue) = (hex(&v["init_prologue"]), hex(&v["resp_prologue"]));
        let init_psk: Option<[u8; 32]> = v.get("init_psks").map(|p| hex32(&p[0]));
        let resp_psk: Option<[u8; 32]> = v.get("resp_psks").map(|p| hex32(&p[0]));
        let remote = v.get("init_remote_static").map(hex);

        let mut b = snow::Builder::new(params.clone())
            .local_private_key(&init_static)
            .unwrap()
            .fixed_ephemeral_key_for_testing_only(&init_eph)
            .prologue(&init_prologue)
            .unwrap();
        if let Some(r) = &remote {
            b = b.remote_public_key(r).unwrap();
        }
        if let Some(psk) = &init_psk {
            b = b.psk(3, psk).unwrap();
        }
        let mut init = b.build_initiator().unwrap();

        let mut b = snow::Builder::new(params)
            .local_private_key(&resp_static)
            .unwrap()
            .fixed_ephemeral_key_for_testing_only(&resp_eph)
            .prologue(&resp_prologue)
            .unwrap();
        if let Some(psk) = &resp_psk {
            b = b.psk(3, psk).unwrap();
        }
        let mut resp = b.build_responder().unwrap();

        let messages = v["messages"].as_array().unwrap();
        let mut buf = vec![0u8; 65535];
        let mut out = vec![0u8; 65535];
        let mut i = 0;
        while !(init.is_handshake_finished() && resp.is_handshake_finished()) {
            let m = &messages[i];
            let (w, r) = if i % 2 == 0 { (&mut init, &mut resp) } else { (&mut resp, &mut init) };
            let n = w.write_message(&hex(&m["payload"]), &mut buf).unwrap();
            assert_eq!(hex::encode(&buf[..n]), m["ciphertext"].as_str().unwrap(), "handshake message {i}");
            let n = r.read_message(&buf[..n], &mut out).unwrap();
            assert_eq!(out[..n].to_vec(), hex(&m["payload"]));
            i += 1;
        }
        assert_eq!(init.get_handshake_hash().to_vec(), hex(&v["handshake_hash"]));
        let mut init = init.into_transport_mode().unwrap();
        let mut resp = resp.into_transport_mode().unwrap();
        for (j, m) in messages.iter().enumerate().skip(i) {
            let (w, r) = if j % 2 == 0 { (&mut init, &mut resp) } else { (&mut resp, &mut init) };
            let n = w.write_message(&hex(&m["payload"]), &mut buf).unwrap();
            assert_eq!(hex::encode(&buf[..n]), m["ciphertext"].as_str().unwrap(), "transport message {j}");
            let n = r.read_message(&buf[..n], &mut out).unwrap();
            assert_eq!(out[..n].to_vec(), hex(&m["payload"]));
        }
    }

    #[test]
    fn cacophony_vectors() {
        let d = load("noise-cacophony.json");
        let vectors = d["vectors"].as_array().unwrap();
        let names: Vec<&str> = vectors.iter().map(|v| v["protocol_name"].as_str().unwrap()).collect();
        assert!(names.contains(&SESSION_NOISE) && names.contains(&PAIRING_NOISE));
        for v in vectors {
            run_noise_vector(v);
        }
    }
}
