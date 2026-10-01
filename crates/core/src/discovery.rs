//! LAN discovery over mDNS / DNS-SD. Every node advertises itself and browses for others.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, IfPredicate, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

use crate::proto::PROTOCOL_VERSION;
use crate::update;

pub const SERVICE_TYPE: &str = "_mousetail._udp.local.";

/// Searches asked for closer together than this are folded into one.
const SEARCH_AGAIN_GAP: Duration = Duration::from_secs(5);

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

/// Ask the network again who's there.
///
/// Browsing on its own asks less and less often (in the end once an hour), and a computer
/// that went quiet (asleep, or its Wi-Fi dropped) is forgotten until it's asked about again.
/// So search afresh whenever there's reason to think something changed: waking from sleep,
/// a network change, or a paired computer nowhere to be seen.
#[derive(Clone, Default)]
pub struct Search {
    wanted: Arc<AtomicBool>,
    last: Arc<Mutex<Option<Instant>>>,
}

impl Search {
    pub fn again(&self) {
        let mut last = self.last.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < SEARCH_AGAIN_GAP) {
            return;
        }
        *last = Some(Instant::now());
        self.wanted.store(true, Ordering::Relaxed);
    }

    fn take(&self) -> bool {
        self.wanted.swap(false, Ordering::Relaxed)
    }
}

/// Advertise ourselves and browse for others; `search` asks for a fresh search.
pub fn start(
    id: &str,
    name: &str,
    port: u16,
    search: Search,
) -> anyhow::Result<(Discovery, mpsc::UnboundedReceiver<Event>)> {
    let daemon = ServiceDaemon::new()?;
    // One interface per IPv4 network. A computer on Wi-Fi and Ethernet to the same router
    // otherwise hears itself on the other one with a different address, takes that for
    // another computer using its name, and renames itself ("… (2)"), leaving others with a
    // muddle of names and addresses. IPv6 and loopback addresses are never dialled, so they're
    // left out too.
    daemon.disable_interface(IfKind::All)?;
    daemon.enable_interface(IfKind::Predicate(IfPredicate::new(|intf| {
        match intf.addr {
            if_addrs::IfAddr::V4(ref v4) => advertise_on(&intf.name, v4.ip),
            _ => false,
        }
    })))?;
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

    let mut browse = daemon.browse(SERVICE_TYPE)?;
    let (tx, rx) = mpsc::unbounded_channel();
    let own_id = id.to_string();
    let daemon2 = daemon.clone();
    let search2 = search;
    std::thread::Builder::new()
        .name("discovery".into())
        .spawn(move || {
            let mut seen = Seen::default();
            loop {
                if search2.take() {
                    // Browsing again starts the questions over from once a second (and hands
                    // over what's already known); the old listener is let go.
                    match daemon2.browse(SERVICE_TYPE) {
                        Ok(b) => browse = b,
                        Err(e) => tracing::debug!("searching again: {e}"),
                    }
                }
                let event = match browse.recv_timeout(Duration::from_millis(500)) {
                    Ok(event) => event,
                    Err(mdns_sd::RecvTimeoutError::Timeout) => continue,
                    Err(mdns_sd::RecvTimeoutError::Disconnected) => {
                        if daemon2.status().is_err() {
                            break; // shut down
                        }
                        // Replaced, or dropped: browse again rather than go deaf.
                        std::thread::sleep(Duration::from_secs(1));
                        search2.wanted.store(true, Ordering::Relaxed);
                        continue;
                    }
                };
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
                        seen.found(
                            info.get_fullname(),
                            Found {
                                id,
                                name,
                                addrs,
                                version,
                            },
                        )
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => match seen.removed(&fullname) {
                        Some(event) => event,
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

/// Every advertisement seen, by its full name. One computer can have several at once: an
/// older MouseTail renames itself on a second network connection ("… (2)"), and its old
/// name lingers in caches for a while. Merge them, and only call a computer gone once all of
/// its advertisements are.
#[derive(Default)]
struct Seen {
    by_name: HashMap<String, Found>,
}

impl Seen {
    fn found(&mut self, fullname: &str, found: Found) -> Event {
        let id = found.id.clone();
        self.by_name.insert(fullname.to_string(), found);
        Event::Found(self.merged(&id).expect("just added"))
    }

    fn removed(&mut self, fullname: &str) -> Option<Event> {
        let id = self.by_name.remove(fullname)?.id;
        Some(match self.merged(&id) {
            Some(found) => Event::Found(found),
            None => Event::Lost(id),
        })
    }

    /// Everything known about `id`, newest advertisement's name and version first.
    fn merged(&self, id: &str) -> Option<Found> {
        let mut all: Vec<&Found> = self.by_name.values().filter(|f| f.id == id).collect();
        // Stable order, so the same set merges the same way each time.
        all.sort_by(|a, b| a.addrs.cmp(&b.addrs));
        let first = *all.first()?;
        let mut addrs: Vec<SocketAddr> = vec![];
        for a in all.iter().flat_map(|f| &f.addrs) {
            if !addrs.contains(a) {
                addrs.push(*a);
            }
        }
        Some(Found {
            id: first.id.clone(),
            name: first.name.clone(),
            addrs,
            version: all.iter().find_map(|f| f.version.clone()),
        })
    }
}

/// Whether to advertise and browse on this address: not loopback or link-local, and the first
/// interface (by name) on its network, so each network gets exactly one.
fn advertise_on(name: &str, ip: Ipv4Addr) -> bool {
    let all: Vec<(String, Ipv4Addr, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some((i.name, v4.ip, v4.netmask)),
            _ => None,
        })
        .collect();
    first_on_network(&all, name, ip)
}

fn first_on_network(all: &[(String, Ipv4Addr, Ipv4Addr)], name: &str, ip: Ipv4Addr) -> bool {
    if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() {
        return false;
    }
    let Some(&(_, _, mask)) = all.iter().find(|(n, a, _)| n == name && *a == ip) else {
        // Gone already, or not visible to us: let mdns-sd decide.
        return true;
    };
    let net = |a: Ipv4Addr, m: Ipv4Addr| u32::from(a) & u32::from(m);
    let mut names: Vec<&str> = all
        .iter()
        .filter(|(_, a, _)| !a.is_loopback() && !a.is_link_local())
        .filter(|(_, a, m)| net(*a, mask) == net(ip, mask) || net(ip, *m) == net(*a, *m))
        .map(|(n, _, _)| n.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    names.first() == Some(&name)
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
    fn one_interface_per_network() {
        let ip = |s: &str| -> Ipv4Addr { s.parse().unwrap() };
        let mask22 = ip("255.255.252.0");
        let all = vec![
            ("lo0".to_string(), ip("127.0.0.1"), ip("255.0.0.0")),
            ("en0".to_string(), ip("192.168.68.64"), mask22),
            ("en11".to_string(), ip("192.168.68.75"), mask22),
            (
                "utun4".to_string(),
                ip("100.89.52.22"),
                ip("255.255.255.255"),
            ),
        ];
        assert!(first_on_network(&all, "en0", ip("192.168.68.64")));
        assert!(!first_on_network(&all, "en11", ip("192.168.68.75")));
        assert!(first_on_network(&all, "utun4", ip("100.89.52.22")));
        assert!(!first_on_network(&all, "lo0", ip("127.0.0.1")));
        // Wi-Fi gone: Ethernet takes over.
        assert!(first_on_network(&all[2..], "en11", ip("192.168.68.75")));
    }

    #[test]
    fn merges_a_computers_advertisements() {
        let found = |addr: &str| Found {
            id: "mac".into(),
            name: "Mac".into(),
            addrs: vec![addr.parse().unwrap()],
            version: Some("0.2.5".into()),
        };
        let mut seen = Seen::default();
        seen.found("Mac-1._mousetail._udp.local.", found("10.0.0.1:24802"));
        let Event::Found(both) =
            seen.found("Mac-1 (2)._mousetail._udp.local.", found("10.0.0.2:24802"))
        else {
            panic!("expected found");
        };
        assert_eq!(both.addrs.len(), 2);
        // One goes: still there, at the other's address.
        let Some(Event::Found(one)) = seen.removed("Mac-1._mousetail._udp.local.") else {
            panic!("expected found");
        };
        assert_eq!(one.addrs, vec!["10.0.0.2:24802".parse().unwrap()]);
        assert_eq!(
            seen.removed("Mac-1 (2)._mousetail._udp.local."),
            Some(Event::Lost("mac".into()))
        );
        assert_eq!(seen.removed("Mac-1 (2)._mousetail._udp.local."), None);
    }

    #[test]
    fn filters_addresses() {
        assert!(usable(&"192.168.1.20".parse().unwrap()));
        assert!(!usable(&"127.0.0.1".parse().unwrap()));
        assert!(!usable(&"fe80::1".parse().unwrap()));
        assert!(usable(&"2407:7000::1".parse().unwrap()));
    }
}
