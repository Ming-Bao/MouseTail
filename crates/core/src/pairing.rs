//! Pairing with a short code.
//!
//! One machine shows a 4-digit code; the user types it on the other. Both run SPAKE2 with the
//! code, so an eavesdropper learns nothing and an active attacker gets one guess per attempt.
//! The shared key then authenticates both certificate fingerprints (taken from the TLS
//! session, not from messages), after which each side pins the other's certificate.

use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

/// In-progress SPAKE2 exchange.
pub type PakeState = Spake2<Ed25519Group>;

const LABEL: &[u8] = b"kiore-pair-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Typed the code.
    Initiator,
    /// Showed the code.
    Responder,
}

pub fn new_code() -> String {
    format!("{:04}", rand::thread_rng().gen_range(0..10_000))
}

pub fn start(code: &str) -> (PakeState, Vec<u8>) {
    Spake2::<Ed25519Group>::start_symmetric(&Password::new(code.as_bytes()), &Identity::new(LABEL))
}

pub fn finish(state: PakeState, peer_msg: &[u8]) -> anyhow::Result<Vec<u8>> {
    state
        .finish(peer_msg)
        .map_err(|e| anyhow::anyhow!("pairing exchange failed: {e:?}"))
}

fn mac(key: &[u8], role: Role, initiator_fp: &str, responder_fp: &str) -> Hmac<Sha256> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    m.update(LABEL);
    m.update(match role {
        Role::Initiator => b"|initiator|",
        Role::Responder => b"|responder|",
    });
    m.update(initiator_fp.as_bytes());
    m.update(b"|");
    m.update(responder_fp.as_bytes());
    m
}

/// Confirmation tag sent by `role`.
pub fn tag(key: &[u8], role: Role, initiator_fp: &str, responder_fp: &str) -> Vec<u8> {
    mac(key, role, initiator_fp, responder_fp)
        .finalize()
        .into_bytes()
        .to_vec()
}

/// Check a tag sent by `role` (constant time).
pub fn verify(key: &[u8], role: Role, initiator_fp: &str, responder_fp: &str, tag: &[u8]) -> bool {
    mac(key, role, initiator_fp, responder_fp)
        .verify_slice(tag)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(code_a: &str, code_b: &str) -> bool {
        let (sa, ma) = start(code_a);
        let (sb, mb) = start(code_b);
        let ka = finish(sa, &mb).unwrap();
        let kb = finish(sb, &ma).unwrap();
        let t = tag(&kb, Role::Responder, "fpA", "fpB");
        verify(&ka, Role::Responder, "fpA", "fpB", &t)
    }

    #[test]
    fn same_code_pairs() {
        assert!(run("0420", "0420"));
    }

    #[test]
    fn wrong_code_fails() {
        assert!(!run("0420", "0421"));
    }

    #[test]
    fn tag_binds_fingerprints_and_role() {
        let (sa, ma) = start("1234");
        let (sb, mb) = start("1234");
        let ka = finish(sa, &mb).unwrap();
        let kb = finish(sb, &ma).unwrap();
        let t = tag(&kb, Role::Responder, "fpA", "fpB");
        assert!(!verify(&ka, Role::Responder, "fpA", "fpX", &t));
        assert!(!verify(&ka, Role::Initiator, "fpA", "fpB", &t));
    }

    #[test]
    fn codes_are_four_digits() {
        for _ in 0..100 {
            let c = new_code();
            assert_eq!(c.len(), 4);
            assert!(c.chars().all(|ch| ch.is_ascii_digit()));
        }
    }
}
