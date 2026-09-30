//! Persistent settings and paired peers. Everything has a sensible default; the file only
//! exists so people can dig in if they want to.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::layout::Point;
use crate::proto::DisplayInfo;

pub const DEFAULT_PORT: u16 = 24802;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Name shown to other machines. Defaults to the host name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, rename = "peer")]
    pub peers: Vec<PeerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: None,
            port: DEFAULT_PORT,
            settings: Settings::default(),
            peers: vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Carry the clipboard across when the cursor crosses.
    pub clipboard: bool,
    /// Largest clipboard payload to send, in bytes.
    pub clipboard_max_bytes: usize,
    /// Share sound: a machine with speakers plays the other's sound; a machine without
    /// sends it. Off here means neither.
    pub audio: bool,
    /// Install new releases automatically (Linux; the Mac app has its own setting).
    pub updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            clipboard: true,
            clipboard_max_bytes: 10 * 1024 * 1024,
            audio: true,
            updates: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerConfig {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    /// Offset of the peer's displays in this machine's layout. Unset = automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Point>,
    /// When the placement was last chosen by a person (Unix ms; 0 = automatic). The newest
    /// arrangement wins when two computers compare theirs.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub placement_updated: u64,
    /// Last known displays, so the arrangement can show the machine while it's offline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub displays: Vec<DisplayInfo>,
    /// Hardware addresses for waking it (Wake-on-LAN).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_macs: Vec<String>,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, toml::to_string_pretty(self)?)?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn peer(&self, id: &str) -> Option<&PeerConfig> {
        self.peers.iter().find(|p| p.id == id)
    }

    pub fn peer_mut(&mut self, id: &str) -> Option<&mut PeerConfig> {
        self.peers.iter_mut().find(|p| p.id == id)
    }

    pub fn is_paired(&self, fingerprint: &str) -> bool {
        self.peers.iter().any(|p| p.fingerprint == fingerprint)
    }

    pub fn add_peer(&mut self, peer: PeerConfig) {
        self.peers.retain(|p| p.id != peer.id);
        self.peers.push(peer);
    }

    /// Match a peer by id, id prefix or (case-insensitive) name.
    pub fn find_peer(&self, query: &str) -> Option<&PeerConfig> {
        self.peers
            .iter()
            .find(|p| p.id.starts_with(query) || p.name.eq_ignore_ascii_case(query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults() {
        let parsed: Config = toml::from_str("").unwrap();
        assert_eq!(parsed, Config::default());

        let mut c = Config::default();
        c.add_peer(PeerConfig {
            id: "abc".into(),
            name: "omarchy".into(),
            fingerprint: "ff".into(),
            placement: Some(Point::new(-1920.0, -49.0)),
            placement_updated: 0,
            displays: vec![],
            wake_macs: vec![],
        });
        let text = toml::to_string_pretty(&c).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), c);
        assert!(c.find_peer("OMARCHY").is_some());
    }
}
