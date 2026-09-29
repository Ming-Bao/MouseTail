//! A device's long-term identity: a self-signed certificate whose SHA-256 fingerprint is the
//! device's id. Created on first run; pinned by peers when pairing.

use std::fs;
use std::path::Path;

use anyhow::Context;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
    pub fingerprint: String,
}

impl Identity {
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Self> {
        let cert_path = dir.join("identity.crt");
        let key_path = dir.join("identity.key");
        if !cert_path.exists() || !key_path.exists() {
            fs::create_dir_all(dir)?;
            let key = rcgen::KeyPair::generate()?;
            let params = rcgen::CertificateParams::new(vec!["kiore".to_string()])?;
            let cert = params.self_signed(&key)?;
            write_private(&key_path, key.serialize_pem().as_bytes())?;
            fs::write(&cert_path, cert.pem())?;
        }
        let cert = CertificateDer::from_pem_file(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key = PrivateKeyDer::from_pem_file(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        let fingerprint = fingerprint(&cert);
        Ok(Self {
            cert,
            key,
            fingerprint,
        })
    }

    pub fn id(&self) -> String {
        id_from_fingerprint(&self.fingerprint)
    }
}

pub fn fingerprint(cert_der: &[u8]) -> String {
    hex::encode(Sha256::digest(cert_der))
}

/// Short, stable device id derived from the certificate fingerprint.
pub fn id_from_fingerprint(fp: &str) -> String {
    fp[..16].to_string()
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Ok(fs::write(path, bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reloads_same_identity() {
        let dir = std::env::temp_dir().join(format!("kiore-id-{}", std::process::id()));
        let a = Identity::load_or_create(&dir).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.id().len(), 16);
        fs::remove_dir_all(dir).unwrap();
    }
}
