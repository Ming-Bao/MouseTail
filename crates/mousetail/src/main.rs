//! MouseTail: share one keyboard, mouse and clipboard between machines on the LAN.

mod ipc;
mod node;
mod paths;
mod platform;
mod update;

use std::io::Write;

use anyhow::Context;
use ipc::Request;
use mousetail_core::layout::Side;
use paths::Paths;
use serde_json::Value;

const USAGE: &str = "\
MouseTail — share one keyboard and mouse between machines on your network.

Usage:
  mousetail run                         Run MouseTail (normally started for you)
  mousetail status                      Show this machine and the ones it can see
  mousetail pair [machine]              Pair with a machine (it shows a code to type here)
  mousetail unpair <machine>            Forget a paired machine
  mousetail place <machine> <side> [n]  Put a machine left/right/above/below display n
                                        (default: the main display)
  mousetail release                     Bring the cursor back to this machine
  mousetail set <clipboard|audio|updates> <on|off>  Change a setting
  mousetail update                      Install the latest release now (Linux)
  mousetail watch                       Print status as JSON, one line per change
  mousetail --version                   Show the version
";

fn main() {
    // macOS delivers media controls (AirPods, media keys) on the main thread only, so there
    // the main thread runs the run loop and everything else runs beside it.
    #[cfg(target_os = "macos")]
    {
        std::thread::Builder::new()
            .name("main-async".into())
            .stack_size(8 << 20)
            .spawn(|| std::process::exit(run_async()))
            .expect("starting the async runtime");
        platform::run_main_loop();
    }
    #[cfg(not(target_os = "macos"))]
    std::process::exit(run_async());
}

fn run_async() -> i32 {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("building the async runtime");
    let result = runtime.block_on(run());
    // `process::exit` doesn't flush.
    let _ = std::io::stdout().flush();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mousetail: {e:#}");
            1
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::new();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0).unwrap_or("run") {
        "run" => {
            tracing_subscriber::fmt()
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info,quinn=warn,mdns_sd=warn".into()),
                )
                .init();
            let exit_with_parent = args.iter().any(|a| a == "--exit-with-parent");
            node::Node::run(paths, exit_with_parent).await
        }
        "status" => {
            let v = ipc::call(&paths.socket, &Request::Status).await?;
            // Ignore a closed pipe (e.g. `mousetail status | head`).
            let _ = std::io::stdout().write_all(status_text(&v).as_bytes());
            Ok(())
        }
        "pair" => pair(&paths, arg(1)).await,
        "unpair" => {
            let peer = arg(1).context("which machine?")?.to_string();
            let v = ipc::call(&paths.socket, &Request::Unpair { peer }).await?;
            println!("Forgot {}.", v["name"].as_str().unwrap_or("it"));
            Ok(())
        }
        "place" => {
            let peer = arg(1).context("which machine?")?.to_string();
            let side: Side = arg(2)
                .context("which side? left, right, above or below")?
                .parse()?;
            let display = arg(3)
                .map(str::parse)
                .transpose()
                .context("display must be a number")?;
            ipc::call(
                &paths.socket,
                &Request::Place {
                    peer,
                    side,
                    display,
                },
            )
            .await?;
            println!("Done.");
            Ok(())
        }
        "release" => {
            ipc::call(&paths.socket, &Request::Release).await?;
            Ok(())
        }
        "set" => {
            let key = arg(1)
                .context("which setting? (clipboard, audio, updates)")?
                .to_string();
            let value = match arg(2).context("on or off?")? {
                "on" | "true" | "yes" => serde_json::Value::Bool(true),
                "off" | "false" | "no" => serde_json::Value::Bool(false),
                other => anyhow::bail!("expected on or off, not {other:?}"),
            };
            ipc::call(&paths.socket, &Request::SetSetting { key, value }).await?;
            Ok(())
        }
        "watch" => watch(&paths).await,
        "update" => {
            if cfg!(target_os = "macos") {
                anyhow::bail!(
                    "on a Mac, MouseTail updates itself: MouseTail menu → Check for Updates"
                );
            }
            let v = ipc::call(&paths.socket, &Request::Update).await?;
            println!("{}", v["message"].as_str().unwrap_or("Done."));
            Ok(())
        }
        "--version" | "-V" | "version" => {
            println!("mousetail {}", mousetail_core::update::VERSION);
            Ok(())
        }
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => anyhow::bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

/// Stream status for status bars: a JSON line whenever anything changes, `{"running":false}`
/// while the daemon is down. Exits when stdout closes.
async fn watch(paths: &Paths) -> anyhow::Result<()> {
    let mut last = String::new();
    loop {
        let line = match ipc::call(&paths.socket, &Request::Status).await {
            Ok(mut v) => {
                v["running"] = serde_json::Value::Bool(true);
                // Round-trip times jitter constantly; don't wake the bar for them.
                if let Some(peers) = v["peers"].as_array_mut() {
                    for p in peers {
                        p["rtt_ms"] = serde_json::Value::Null;
                    }
                }
                v.to_string()
            }
            Err(_) => r#"{"running":false}"#.to_string(),
        };
        if line != last {
            let mut out = std::io::stdout();
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                return Ok(());
            }
            last = line;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn pair(paths: &Paths, peer: Option<&str>) -> anyhow::Result<()> {
    let v = ipc::call(
        &paths.socket,
        &Request::Pair {
            peer: peer.map(str::to_string),
        },
    )
    .await?;
    let id = v["peer"].as_str().context("no peer")?.to_string();
    let name = v["name"].as_str().unwrap_or(&id).to_string();
    print!("A code is showing on {name}. Type it here: ");
    std::io::stdout().flush()?;
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    ipc::call(
        &paths.socket,
        &Request::PairCode {
            peer: id,
            code: code.trim().to_string(),
        },
    )
    .await?;
    println!("Paired with {name}.");
    Ok(())
}

fn status_text(v: &Value) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} ({})  UDP {}",
        v["name"].as_str().unwrap_or("?"),
        v["id"].as_str().unwrap_or("?"),
        v["port"]
    );
    let mut roles = vec![];
    if v["can_control"] == true {
        roles.push("can share its keyboard and mouse");
    }
    if v["can_be_controlled"] == true {
        roles.push("can be controlled");
    }
    let _ = writeln!(out, "  {}", roles.join(", "));
    if let Some(peer) = v["controlling"].as_str() {
        let _ = writeln!(out, "  cursor is on {peer}");
    }
    if let Some(peer) = v["controlled_by"].as_str() {
        let _ = writeln!(out, "  being controlled by {peer}");
    }
    let _ = writeln!(out, "Displays:");
    for (i, d) in v["displays"].as_array().into_iter().flatten().enumerate() {
        let r = &d["rect"];
        let _ = writeln!(
            out,
            "  {i}: {}{}  {}x{} at ({}, {})",
            d["name"].as_str().unwrap_or("?"),
            if d["primary"] == true { " (main)" } else { "" },
            r["w"],
            r["h"],
            r["x"],
            r["y"]
        );
    }
    let _ = writeln!(out, "Machines:");
    let peers = v["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        let _ = writeln!(out, "  none found yet");
    }
    for p in peers {
        let state = match (p["paired"] == true, p["connected"] == true) {
            (true, true) => "paired, connected",
            (true, false) => "paired, not connected",
            (false, true) => "not paired — run `mousetail pair`",
            (false, false) => "seen",
        };
        let rtt = p["rtt_ms"]
            .as_f64()
            .map(|ms| format!(", {ms:.1} ms via {}", p["address"].as_str().unwrap_or("?")))
            .unwrap_or_default();
        let sound = match p["sound"].as_str() {
            Some("here") => ", its sound plays here",
            Some("there") => ", this machine's sound plays there",
            _ => "",
        };
        let _ = writeln!(
            out,
            "  {} ({}): {state}{rtt}{sound}",
            p["name"].as_str().unwrap_or("?"),
            p["id"].as_str().unwrap_or("?")
        );
    }
    out
}
