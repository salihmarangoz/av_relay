//! CPace, the balanced PAKE that turns the 6-digit pairing code into a
//! strong shared key (draft-irtf-cfrg-cpace-21, suite CPACE-RISTR255-SHA512,
//! initiator-responder setting). See docs/protocol.md, section 5.3.

use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::IsIdentity;
use sha2::{Digest, Sha512};
use zeroize::{Zeroize, Zeroizing};

const DSI: &[u8] = b"CPaceRistretto255";
const DSI_ISK: &[u8] = b"CPaceRistretto255_ISK";
/// SHA-512's input block size.
const HASH_BLOCK: usize = 128;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CpaceError {
    #[error("the peer's CPace share isn't a valid ristretto255 point")]
    InvalidPoint,
    #[error("the CPace shared point is the identity")]
    Identity,
}

/// One side of a CPace run: the share to send, and the scalar to finish with.
pub struct Cpace {
    scalar: Scalar,
    sid: Vec<u8>,
    share: [u8; 32],
    initiator: bool,
}

impl Cpace {
    /// Starts a run with a fresh random scalar. `initiator` is the client.
    pub fn new(prs: &[u8], ci: &[u8], sid: &[u8], initiator: bool) -> Self {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut()).expect("the system RNG failed");
        Self::with_scalar(prs, ci, sid, &bytes, initiator)
    }

    /// Starts a run with a given scalar (little-endian; the top 4 bits are
    /// cleared). Only tests should pick the scalar.
    pub(crate) fn with_scalar(prs: &[u8], ci: &[u8], sid: &[u8], scalar: &[u8; 32], initiator: bool) -> Self {
        let mut bytes = *scalar;
        bytes[31] &= 0x0f;
        let scalar = Scalar::from_bytes_mod_order(bytes);
        bytes.zeroize();
        let share = (scalar * generator(prs, ci, sid)).compress().to_bytes();
        Self { scalar, sid: sid.to_vec(), share, initiator }
    }

    /// The public share (`Ya` for the initiator, `Yb` for the responder).
    pub fn share(&self) -> [u8; 32] {
        self.share
    }

    /// Combines the peer's share into the intermediate session key (ISK).
    /// `ad_own` and `ad_peer` are the associated data each side sent.
    pub fn finish(self, peer_share: &[u8], ad_own: &[u8], ad_peer: &[u8]) -> Result<Zeroizing<[u8; 64]>, CpaceError> {
        let k = scalar_mult_vfy(&self.scalar, peer_share)?;

        let ((ya, ada), (yb, adb)) = if self.initiator {
            ((&self.share[..], ad_own), (peer_share, ad_peer))
        } else {
            ((peer_share, ad_peer), (&self.share[..], ad_own))
        };
        let mut input = Zeroizing::new(lv_cat(&[DSI_ISK, &self.sid, &k[..]]));
        input.extend_from_slice(&transcript_ir(ya, ada, yb, adb));
        Ok(Zeroizing::new(Sha512::digest(&input[..]).into()))
    }
}

impl Drop for Cpace {
    fn drop(&mut self) {
        self.scalar.zeroize();
    }
}

/// The draft's `scalar_mult_vfy`: `scalar` times the encoded point `point`,
/// refusing points that don't decode and an identity result.
fn scalar_mult_vfy(scalar: &Scalar, point: &[u8]) -> Result<Zeroizing<[u8; 32]>, CpaceError> {
    let point = CompressedRistretto::from_slice(point)
        .ok()
        .and_then(|c| c.decompress())
        .ok_or(CpaceError::InvalidPoint)?;
    let k = scalar * point;
    if k.is_identity() {
        return Err(CpaceError::Identity);
    }
    Ok(Zeroizing::new(k.compress().to_bytes()))
}

/// The generator: ristretto255's element derivation of
/// SHA-512(generator_string(...)) (libsodium's crypto_core_ristretto255_from_hash).
pub(crate) fn generator(prs: &[u8], ci: &[u8], sid: &[u8]) -> RistrettoPoint {
    let hash = Zeroizing::new(<[u8; 64]>::from(Sha512::digest(&generator_string(prs, ci, sid)[..])));
    RistrettoPoint::from_uniform_bytes(&hash)
}

pub(crate) fn generator_string(prs: &[u8], ci: &[u8], sid: &[u8]) -> Zeroizing<Vec<u8>> {
    let zpad = HASH_BLOCK.saturating_sub(1 + len_prefix_size(prs.len()) + prs.len() + len_prefix_size(DSI.len()) + DSI.len());
    Zeroizing::new(lv_cat(&[DSI, prs, &vec![0u8; zpad], ci, sid]))
}

fn transcript_ir(ya: &[u8], ada: &[u8], yb: &[u8], adb: &[u8]) -> Vec<u8> {
    let mut t = lv_cat(&[ya, ada]);
    t.extend_from_slice(&lv_cat(&[yb, adb]));
    t
}

/// Each part with its length (LEB128) in front, concatenated.
pub fn lv_cat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        let mut len = part.len();
        loop {
            let byte = (len & 0x7f) as u8;
            len >>= 7;
            if len == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out.extend_from_slice(part);
    }
    out
}

fn len_prefix_size(len: usize) -> usize {
    let mut size = 1;
    let mut len = len >> 7;
    while len > 0 {
        size += 1;
        len >>= 7;
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::test_vectors::{hex32, load};

    #[test]
    fn lv_cat_matches_the_draft() {
        assert_eq!(hex::encode(lv_cat(&[b"1234", b"5", b"", b"678"])), "043132333401350003363738");
        let long: Vec<u8> = (0..128).collect();
        assert_eq!(&lv_cat(&[&long])[..3], &[0x80, 0x01, 0x00]);
        assert_eq!(len_prefix_size(127), 1);
        assert_eq!(len_prefix_size(128), 2);
    }

    #[test]
    fn draft_vectors() {
        let v = &load("cpace-draft.json")["vectors"];
        let h = |k: &str| hex::decode(v[k].as_str().unwrap()).unwrap();
        let (prs, ci, sid) = (h("PRS"), h("CI"), h("sid"));
        assert_eq!(generator(&prs, &ci, &sid).compress().to_bytes().to_vec(), h("g"));

        let a = Cpace::with_scalar(&prs, &ci, &sid, &hex32(&v["ya"]), true);
        let b = Cpace::with_scalar(&prs, &ci, &sid, &hex32(&v["yb"]), false);
        assert_eq!(a.share().to_vec(), h("Ya"));
        assert_eq!(b.share().to_vec(), h("Yb"));
        let (ya, yb) = (a.share(), b.share());
        let isk_a = a.finish(&yb, &h("ADa"), &h("ADb")).unwrap();
        let isk_b = b.finish(&ya, &h("ADb"), &h("ADa")).unwrap();
        assert_eq!(isk_a.to_vec(), h("ISK_IR"));
        assert_eq!(isk_b.to_vec(), h("ISK_IR"));
    }

    #[test]
    fn invalid_points_abort() {
        let v = &load("cpace-draft.json")["vectors"];
        let h = |k: &str| hex::decode(v[k].as_str().unwrap()).unwrap();
        let s = Scalar::from_bytes_mod_order(hex32(&v["valid_s"]));
        let run = |y: &[u8]| scalar_mult_vfy(&s, y).map(|k| k.to_vec());
        assert_eq!(run(&h("valid_X")), Ok(h("valid_sX")));
        assert_eq!(run(&h("invalid_Y1")), Err(CpaceError::InvalidPoint));
        assert_eq!(run(&h("invalid_Y2")), Err(CpaceError::Identity));
        assert_eq!(run(&[1, 2, 3]), Err(CpaceError::InvalidPoint));
    }

    #[test]
    fn pairing_vector() {
        let p = &load("pairing.json")["cpace"];
        let h = |k: &str| hex::decode(p[k].as_str().unwrap()).unwrap();
        let ci = lv_cat(&[b"av-relay/1 pairing", b"client", b"server"]);
        assert_eq!(ci, h("CI"));
        let sid = [h("sid_c"), h("sid_s")].concat();
        assert_eq!(generator_string(&h("PRS"), &ci, &sid).to_vec(), h("generator_string"));
        let client = Cpace::with_scalar(&h("PRS"), &ci, &sid, &hex32(&p["ya"]), true);
        assert_eq!(client.share().to_vec(), h("Ya"));
        assert_eq!(client.finish(&h("Yb"), b"", b"").unwrap().to_vec(), h("ISK"));

        let server = Cpace::with_scalar(&h("PRS"), &ci, &sid, &hex32(&p["yb"]), false);
        assert_eq!(server.finish(&h("Ya"), b"", b"").unwrap().to_vec(), h("ISK"));

        // The client's ISK when the Mac holds another code: a different key.
        let wrong_server = Cpace::with_scalar(b"042138", &ci, &sid, &hex32(&p["yb"]), false);
        let client = Cpace::with_scalar(&h("PRS"), &ci, &sid, &hex32(&p["ya"]), true);
        let isk = client.finish(&wrong_server.share(), b"", b"").unwrap();
        assert_eq!(isk.to_vec(), h("ISK_if_server_code_were_042138"));
        assert_ne!(isk.to_vec(), h("ISK"));
    }
}
