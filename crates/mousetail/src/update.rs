//! Keeping MouseTail up to date on Linux. (The Mac app updates itself with Sparkle.)
//!
//! Every few hours, soon after starting, and whenever another computer advertises a newer
//! release, this checks the latest release's `latest.json`. A newer release is downloaded,
//! its signature checked against the release key, unpacked, and test-run; then, once nobody
//! is using another computer through this one, the binary and the Omarchy bar plugin are
//! swapped in and MouseTail restarts itself. The previous binary is kept alongside.
//!
//! Only installs made by the installer (`~/.local/bin/mousetail`) update themselves;
//! development builds and packaged installs are left alone.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use mousetail_core::update::{self, Manifest, VERSION};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::node::Node;

const PLUGIN_ID: &str = "nz.galengreen.mousetail";
/// How often to look for a new release.
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Checks prompted by other computers are spaced at least this far apart.
const MIN_GAP: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
pub struct Updater {
    wake: Notify,
    /// What the last check found, for `mousetail status` and the bar.
    state: Mutex<State>,
    last_check: Mutex<Option<Instant>>,
    /// A check or install is under way.
    busy: tokio::sync::Mutex<()>,
}

#[derive(Clone, Debug, Default)]
enum State {
    #[default]
    Idle,
    UpToDate,
    /// Downloaded and checked; installs once this computer isn't in use.
    Waiting(String),
    Failed(String),
}

/// The outcome of a check, for `mousetail update`.
pub enum Checked {
    UpToDate,
    /// Installing now or as soon as this computer is free; MouseTail restarts itself.
    Installing(String),
}

impl Updater {
    /// Look for an update soon (another computer runs a newer release, say).
    pub fn nudge(&self) {
        let recent = self
            .last_check
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() < MIN_GAP);
        if !recent {
            self.wake.notify_one();
        }
    }

    pub fn status(&self) -> Value {
        match &*self.state.lock().unwrap() {
            State::Idle => json!({"state": if supported() { "idle" } else { "unsupported" }}),
            State::UpToDate => json!({"state": "up_to_date"}),
            State::Waiting(v) => json!({"state": "waiting", "version": v}),
            State::Failed(e) => json!({"state": "failed", "error": e}),
        }
    }
}

/// Whether this copy of MouseTail can update itself.
pub fn supported() -> bool {
    cfg!(target_os = "linux") && installed_binary().is_some()
}

/// The installer's binary, if that's what's running and we can replace it.
fn installed_binary() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let expected = home.join(".local/bin/mousetail");
    let running = std::env::current_exe().ok()?.canonicalize().ok()?;
    (running == expected.canonicalize().ok()?).then_some(expected)
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("mousetail")
}

/// Runs for the life of the daemon.
pub async fn run(node: Arc<Node>) {
    if !supported() {
        if cfg!(target_os = "linux") {
            info!("not updating automatically: not installed by the MouseTail installer");
        }
        return;
    }
    // Let the network settle after starting before the first check.
    let mut wait = Duration::from_secs(90);
    loop {
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = node.updater.wake.notified() => {}
        }
        wait = INTERVAL;
        if !node
            .settings()
            .get("updates")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            continue;
        }
        if let Err(e) = check(&node).await {
            warn!("update check failed: {e:#}");
        }
    }
}

/// Check now; download, verify and (once idle) install anything newer.
pub async fn check(node: &Arc<Node>) -> anyhow::Result<Checked> {
    if !supported() {
        bail!(
            "this copy of MouseTail wasn't installed by the installer, so it can't update itself"
        );
    }
    let updater = &node.updater;
    let Ok(_busy) = updater.busy.try_lock() else {
        return Ok(match &*updater.state.lock().unwrap() {
            State::Waiting(v) => Checked::Installing(v.clone()),
            _ => Checked::UpToDate,
        });
    };
    *updater.last_check.lock().unwrap() = Some(Instant::now());
    let result = prepare().await;
    match &result {
        Ok(None) => *updater.state.lock().unwrap() = State::UpToDate,
        Ok(Some(staged)) => *updater.state.lock().unwrap() = State::Waiting(staged.version.clone()),
        Err(e) => *updater.state.lock().unwrap() = State::Failed(format!("{e:#}")),
    }
    let Some(staged) = result? else {
        return Ok(Checked::UpToDate);
    };
    let version = staged.version.clone();
    let node = node.clone();
    tokio::spawn(async move {
        // Never swap things out from under someone using another computer through this one.
        while node.in_use() {
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
        // Give `mousetail update` time to hear back before we restart.
        tokio::time::sleep(Duration::from_secs(1)).await;
        match install(&staged) {
            Ok(binary) => restart(&binary),
            Err(e) => {
                warn!("couldn't install {}: {e:#}", staged.version);
                *node.updater.state.lock().unwrap() = State::Failed(format!("{e:#}"));
            }
        }
    });
    Ok(Checked::Installing(version))
}

/// An unpacked, verified, test-run release.
struct Staged {
    version: String,
    /// The release's folder: `mousetail`, `omarchy-plugin/`, …
    dir: PathBuf,
}

async fn prepare() -> anyhow::Result<Option<Staged>> {
    tokio::task::spawn_blocking(|| {
        // Overridable for testing; downloads must be signed by the release key either way.
        let url = std::env::var("MOUSETAIL_UPDATE_MANIFEST")
            .unwrap_or_else(|_| update::MANIFEST_URL.to_string());
        let manifest: Manifest =
            serde_json::from_slice(&fetch(&url, 30)?).context("reading latest.json")?;
        if !update::is_newer(&manifest.version, VERSION) {
            return Ok(None);
        }
        let arch = std::env::consts::ARCH;
        let download = manifest
            .linux
            .get(arch)
            .with_context(|| format!("release {} has no {arch} build", manifest.version))?;
        info!("downloading MouseTail {}", manifest.version);
        let archive = fetch(&download.url, 600)?;
        update::verify(&archive, &download.signature)?;

        let work = state_dir().join("update");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let tarball = work.join("release.tar.gz");
        std::fs::write(&tarball, &archive)?;
        run_ok(
            Command::new("tar")
                .arg("-xzf")
                .arg(&tarball)
                .arg("-C")
                .arg(&work),
        )
        .context("unpacking")?;
        let dir = work.join(format!("mousetail-linux-{arch}"));

        // Make sure it actually runs here (libraries present, right architecture) and is the
        // release it claims to be, so a bad download can't replace a working install.
        let out = Command::new(dir.join("mousetail"))
            .arg("--version")
            .output()
            .context("running the new version")?;
        let reported = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || reported.split_whitespace().nth(1) != Some(&*manifest.version) {
            bail!("the new version doesn't run here: {}", reported.trim());
        }
        Ok(Some(Staged {
            version: manifest.version,
            dir,
        }))
    })
    .await?
}

/// Swap in the new binary (keeping the old one) and the Omarchy plugin. Returns the binary.
fn install(staged: &Staged) -> anyhow::Result<PathBuf> {
    let binary = installed_binary().context("the installed binary has gone")?;
    let state = state_dir();
    std::fs::copy(&binary, state.join("mousetail.previous")).context("keeping the old version")?;
    replace_file(&staged.dir.join("mousetail"), &binary)?;

    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    let plugin = config.join("omarchy/plugins").join(PLUGIN_ID);
    let new_plugin = staged.dir.join("omarchy-plugin").join(PLUGIN_ID);
    if plugin.is_dir() && new_plugin.is_dir() {
        replace_dir(&new_plugin, &plugin).context("updating the Omarchy bar plugin")?;
    }
    info!("installed MouseTail {}", staged.version);
    Ok(binary)
}

/// Copy beside the destination, then rename over it, so it's never half-written.
fn replace_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let temp = to.with_file_name(".mousetail.new");
    std::fs::copy(from, &temp)?;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&temp, to)?;
    Ok(())
}

fn replace_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    let name = to.file_name().context("no name")?.to_string_lossy();
    let temp = to.with_file_name(format!(".{name}.new"));
    let old = to.with_file_name(format!(".{name}.old"));
    let _ = std::fs::remove_dir_all(&temp);
    let _ = std::fs::remove_dir_all(&old);
    std::fs::create_dir_all(&temp)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        std::fs::copy(entry.path(), temp.join(entry.file_name()))?;
    }
    std::fs::rename(to, &old)?;
    std::fs::rename(&temp, to)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Become the new version: same process, so the user service carries on as if nothing happened.
fn restart(binary: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(binary)
            .args(std::env::args_os().skip(1))
            .exec();
        warn!("couldn't restart into the new version: {err}");
    }
    #[cfg(not(unix))]
    let _ = binary;
}

fn fetch(url: &str, timeout_secs: u32) -> anyhow::Result<Vec<u8>> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--retry",
            "2",
            "--max-time",
            &timeout_secs.to_string(),
            url,
        ])
        .output()
        .context("running curl")?;
    if !out.status.success() {
        bail!(
            "downloading {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

fn run_ok(cmd: &mut Command) -> anyhow::Result<()> {
    let out = cmd.output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
