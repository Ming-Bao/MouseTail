//! Local control socket: one JSON request per line, one JSON response per line.
//! Used by the CLI now, and by the Mac menu bar app and the Omarchy bar plugin later.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use mousetail_core::layout::Side;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::warn;

use crate::node::Node;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// Ask a machine to show a pairing code. No peer = the only unpaired one.
    Pair {
        peer: Option<String>,
    },
    /// Finish pairing with the code shown on the other machine.
    PairCode {
        peer: String,
        code: String,
    },
    Unpair {
        peer: String,
    },
    Place {
        peer: String,
        side: Side,
        display: Option<usize>,
    },
    /// Every machine's displays and positions, for the arrangement view.
    Layout,
    /// Drop a machine at a layout position; it snaps to the nearest valid spot.
    PlaceAt {
        peer: String,
        x: f64,
        y: f64,
    },
    SetSetting {
        key: String,
        value: Value,
    },
    /// Bring the cursor home.
    Release,
    /// Look for a new release now and install it.
    Update,
    /// Stop the daemon (the Mac app replacing one left over from another version).
    Shutdown,
}

pub async fn serve(node: Arc<Node>, path: PathBuf) {
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            warn!("control socket {} unavailable: {e}", path.display());
            return;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    while let Ok((stream, _)) = listener.accept().await {
        let node = node.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let response = match serde_json::from_str::<Request>(&line) {
                    Ok(req) => handle(&node, req).await,
                    Err(e) => json!({"ok": false, "error": format!("bad request: {e}")}),
                };
                let mut out = response.to_string();
                out.push('\n');
                if write.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}

async fn handle(node: &Arc<Node>, req: Request) -> Value {
    let result: Result<Value, String> = match req {
        Request::Status => Ok(node.status()),
        Request::Pair { peer } => node.resolve_peer(peer.as_deref(), true).map(|id| {
            node.request_pairing(&id);
            json!({"peer": id, "name": node.status()["peers"]
                    .as_array()
                    .and_then(|ps| ps.iter().find(|p| p["id"] == id.as_str()))
                    .map(|p| p["name"].clone())})
        }),
        Request::PairCode { peer, code } => node
            .pair_with_code(&peer, code.trim())
            .await
            .map(|()| json!({})),
        Request::Unpair { peer } => node.unpair(&peer).map(|name| json!({"name": name})),
        Request::Place {
            peer,
            side,
            display,
        } => node
            .place(&peer, side, display)
            .map(|offset| json!({"offset": offset})),
        Request::Layout => Ok(node.layout()),
        Request::PlaceAt { peer, x, y } => node
            .place_at(&peer, mousetail_core::layout::Point::new(x, y))
            .map(|offset| json!({"offset": offset})),
        Request::SetSetting { key, value } => {
            node.set_setting(&key, &value).map(|()| node.settings())
        }
        Request::Release => {
            node.release();
            Ok(json!({}))
        }
        Request::Shutdown => {
            node.shut_down();
            Ok(json!({}))
        }
        Request::Update => match crate::update::check(node).await {
            Ok(crate::update::Checked::UpToDate) => Ok(
                json!({"message": format!("MouseTail {} is up to date.", mousetail_core::update::VERSION)}),
            ),
            Ok(crate::update::Checked::Installing(v)) => Ok(json!({"message": if node.in_use() {
                format!("MouseTail {v} is ready; it installs as soon as this computer is free.")
            } else {
                format!("Installing MouseTail {v}; it restarts in a moment.")
            }})),
            Err(e) => Err(format!("{e:#}")),
        },
    };
    match result {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v
        }
        Err(e) => json!({"ok": false, "error": e}),
    }
}

/// Send one request to the running daemon.
pub async fn call(path: &Path, req: &Request) -> anyhow::Result<Value> {
    let stream = UnixStream::connect(path)
        .await
        .context("MouseTail isn't running (start it with `mousetail run`)")?;
    let (read, mut write) = stream.into_split();
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    write.write_all(line.as_bytes()).await?;
    let mut lines = BufReader::new(read).lines();
    let reply = lines.next_line().await?.context("no reply")?;
    let v: Value = serde_json::from_str(&reply)?;
    if v["ok"] == json!(true) {
        Ok(v)
    } else {
        anyhow::bail!("{}", v["error"].as_str().unwrap_or("failed"))
    }
}
