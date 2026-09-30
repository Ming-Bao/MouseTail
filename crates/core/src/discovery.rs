//! LAN discovery over mDNS / DNS-SD. Every node advertises itself and browses for others.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

use crate::proto::PROTOCOL_VERSION;
use crate::update;

pub const SERVICE_TYPE: &str = "_mousetail._udp.local.";

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub id: String,
    pub name: String,
    pub addrs: Vec<SocketAddr>,
    /// The MouseTail release it runs (older releases don't say).
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Found(Found),
    Lost(String),
}

/// Keeps advertising and browsing until dropped.
pub struct Discovery {
    daemon: ServiceDaemon,
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

pub fn start(
    id: &str,
    name: &str,
    port: u16,
) -> anyhow::Result<(Discovery, mpsc::UnboundedReceiver<Event>)> {
    let daemon = ServiceDaemon::new()?;
    let host = sanitise(name);
    let instance = format!("{host}-{}", &id[..6]);
    let version = PROTOCOL_VERSION.to_string();
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance,
        &format!("{host}.local."),
        "",
        port,
        &[
            ("id", id),
            ("name", name),
            ("v", version.as_str()),
            ("app", update::VERSION),
        ][..],
    )?
    .enable_addr_auto();
    daemon.register(info)?;

    let browse = daemon.browse(SERVICE_TYPE)?;
    let (tx, rx) = mpsc::unbounded_channel();
    let own_id = id.to_string();
    std::thread::Builder::new()
        .name("discovery".into())
        .spawn(move || {
            let mut names: HashMap<String, String> = HashMap::new();
            while let Ok(event) = browse.recv() {
                let out = match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(id) = info.get_property_val_str("id").map(str::to_string) else {
                            continue;
                        };
                        if id == own_id {
                            continue;
                        }
                        let name = info.get_property_val_str("name").unwrap_or(&id).to_string();
                        let port = info.get_port();
                        let addrs = info
                            .get_addresses()
                            .iter()
                            .map(|a| a.to_ip_addr())
                            .filter(usable)
                            .map(|ip| SocketAddr::new(ip, port))
                            .collect();
                        let version = info.get_property_val_str("app").map(str::to_string);
                        names.insert(info.get_fullname().to_string(), id.clone());
                        Event::Found(Found {
                            id,
                            name,
                            addrs,
                            version,
                        })
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => match names.remove(&fullname) {
                        Some(id) => Event::Lost(id),
                        None => continue,
                    },
                    _ => continue,
                };
                if tx.send(out).is_err() {
                    break;
                }
            }
        })?;
    Ok((Discovery { daemon }, rx))
}

fn usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local(),
        // Link-local IPv6 needs a scope id we don't carry; global IPv6 is fine.
        IpAddr::V6(v6) => !v6.is_loopback() && (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

/// DNS labels: letters, digits and hyphens.
fn sanitise(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-');
    if s.is_empty() {
        "mousetail".into()
    } else {
        s.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitises_names() {
        assert_eq!(sanitise("Jo's MacBook Pro"), "Jo-s-MacBook-Pro");
        assert_eq!(sanitise("…"), "mousetail");
    }

    #[test]
    fn filters_addresses() {
        assert!(usable(&"192.168.1.20".parse().unwrap()));
        assert!(!usable(&"127.0.0.1".parse().unwrap()));
        assert!(!usable(&"fe80::1".parse().unwrap()));
        assert!(usable(&"2407:7000::1".parse().unwrap()));
    }
}
