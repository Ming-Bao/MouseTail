//! Updates: comparing release versions, and checking the signature every download carries.
//!
//! Each release is signed with one Ed25519 key that only the release workflow holds. The Mac
//! app checks it through Sparkle (the same public key is in its Info.plist); Linux checks it
//! here before installing anything.

use std::collections::HashMap;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;

/// Public half of the release signing key: raw 32 bytes, base64.
pub const PUBLIC_KEY: &str = "NDzgZDj84kmm1DcgHgjy8o1FrMfrky/TtcIZwpjF9Yg=";

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The newest release describes itself here (a file attached to every GitHub release).
pub const MANIFEST_URL: &str =
    "https://github.com/galengreen/MouseTail/releases/latest/download/latest.json";

/// `latest.json`: the version, and a download per Linux architecture.
#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub linux: HashMap<String, Download>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Download {
    pub url: String,
    /// Ed25519 signature of the file, base64.
    pub signature: String,
}

/// "1.2.3", with or without a leading "v". Anything after a "-" is ignored.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('-').next()?;
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// Whether `candidate` is a later release than `current`. Unreadable versions never are.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Check a download against the release signing key.
pub fn verify(bytes: &[u8], signature: &str) -> anyhow::Result<()> {
    verify_with(PUBLIC_KEY, bytes, signature)
}

fn verify_with(public_key: &str, bytes: &[u8], signature: &str) -> anyhow::Result<()> {
    let key = BASE64.decode(public_key).context("bad public key")?;
    let signature = BASE64
        .decode(signature.trim())
        .context("the signature isn't base64")?;
    let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key);
    if key.verify(bytes, &signature).is_err() {
        bail!("the download isn't signed by MouseTail's release key");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.2.1"), Some((0, 2, 1)));
        assert_eq!(parse_version("v1.10.0"), Some((1, 10, 0)));
        assert_eq!(parse_version("1.2.3-beta"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("garbage", "0.2.0"));
    }

    #[test]
    fn signatures() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = BASE64.encode(pair.public_key().as_ref());
        let signature = BASE64.encode(pair.sign(b"release").as_ref());
        assert!(verify_with(&public, b"release", &signature).is_ok());
        assert!(verify_with(&public, b"tampered", &signature).is_err());
        assert!(
            verify(b"release", &signature).is_err(),
            "another key's signature"
        );
    }

    /// Signed by the real release key with `openssl pkeyutl -sign -rawin`, as the release
    /// workflow does, so this fails if the key here and the workflow's ever drift apart.
    #[test]
    fn release_key() {
        let signature = "tP6poWd7+wDzMEpOKHUR+daA/9jfHqwrZqKEf+XEh6acvV8VQaGbITcwyN7VmgC7sYKVn5i+TNAIX9VfUFPNCg==";
        assert!(verify(b"MouseTail release key check\n", signature).is_ok());
    }

    #[test]
    fn manifest() {
        let m: Manifest = serde_json::from_str(
            r#"{"version":"0.2.1","linux":{"x86_64":{"url":"https://x/a.tar.gz","signature":"c2ln"}}}"#,
        )
        .unwrap();
        assert_eq!(m.version, "0.2.1");
        assert_eq!(m.linux["x86_64"].signature, "c2ln");
    }
}
