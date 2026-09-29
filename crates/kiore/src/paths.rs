//! Where Kiore keeps its files.

use std::path::PathBuf;

pub struct Paths {
    /// Identity and config.
    pub dir: PathBuf,
    pub config: PathBuf,
    /// Local control socket used by the CLI, the Mac app and the Omarchy bar plugin.
    pub socket: PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let dir = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Kiore")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("kiore")
        };
        let socket = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(run) if cfg!(target_os = "linux") => PathBuf::from(run).join("kiore.sock"),
            _ => dir.join("kiore.sock"),
        };
        migrate_from_old_name(&dir);
        Self {
            config: dir.join("config.toml"),
            dir,
            socket,
        }
    }
}

/// Before release this was called BlindMice. Move its identity, pairings and layout across
/// once, so nobody has to pair again.
fn migrate_from_old_name(dir: &std::path::Path) {
    if dir.exists() {
        return;
    }
    let Some(parent) = dir.parent() else { return };
    let old = parent.join(if cfg!(target_os = "macos") {
        "BlindMice"
    } else {
        "blindmice"
    });
    if old.join("identity.crt").exists() && std::fs::rename(&old, dir).is_ok() {
        let _ = std::fs::remove_file(dir.join("blindmice.sock"));
    }
}
