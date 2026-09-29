//! Where MouseTail keeps its files.

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
            home.join("Library/Application Support/MouseTail")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("mousetail")
        };
        let socket = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(run) if cfg!(target_os = "linux") => PathBuf::from(run).join("mousetail.sock"),
            _ => dir.join("mousetail.sock"),
        };
        migrate_from_old_name(&dir);
        Self {
            config: dir.join("config.toml"),
            dir,
            socket,
        }
    }
}

/// MouseTail used to be called Kiore, and BlindMice before that. Move the old identity,
/// pairings and layout across once, so nobody has to pair again.
fn migrate_from_old_name(dir: &std::path::Path) {
    if dir.exists() {
        return;
    }
    let Some(parent) = dir.parent() else { return };
    for name in ["Kiore", "BlindMice"] {
        let old = parent.join(if cfg!(target_os = "macos") {
            name.to_string()
        } else {
            name.to_lowercase()
        });
        if old.join("identity.crt").exists() && std::fs::rename(&old, dir).is_ok() {
            let _ = std::fs::remove_file(dir.join(format!("{}.sock", name.to_lowercase())));
            return;
        }
    }
}
