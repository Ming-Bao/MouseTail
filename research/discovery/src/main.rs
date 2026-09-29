//! Spike: mDNS discovery across the LAN plus TCP round-trip latency.
//!
//!   spike-discovery advertise     register _kiore._tcp and run a TCP echo server
//!   spike-discovery browse        find peers, then measure 1000 16-byte round trips

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

const SERVICE: &str = "_kiore._tcp.local.";

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "advertise" => advertise(),
        "browse" => browse(),
        _ => eprintln!("usage: spike-discovery advertise|browse"),
    }
}

fn host() -> String {
    let out = std::process::Command::new("hostname").output().unwrap();
    let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
    h.trim_end_matches(".local").to_string()
}

fn advertise() {
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = host();
    let mdns = ServiceDaemon::new().unwrap();
    let info = ServiceInfo::new(
        SERVICE,
        &host,
        &format!("{host}.local."),
        "",
        port,
        &[("id", "spike"), ("v", "0")][..],
    )
    .unwrap()
    .enable_addr_auto();
    mdns.register(info).unwrap();
    println!("advertising {host} on port {port}");
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        s.set_nodelay(true).unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16];
            while s.read_exact(&mut buf).is_ok() {
                if s.write_all(&buf).is_err() {
                    break;
                }
            }
        });
    }
}

fn browse() {
    let mdns = ServiceDaemon::new().unwrap();
    let rx = mdns.browse(SERVICE).unwrap();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(10);
    while Instant::now() < deadline {
        let Ok(event) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        if let ServiceEvent::ServiceResolved(info) = event {
            let addrs: Vec<IpAddr> = info
                .get_addresses()
                .iter()
                .map(|a| a.to_ip_addr())
                .collect();
            println!(
                "resolved {} after {:?}: {:?} port {}",
                info.get_fullname(),
                start.elapsed(),
                addrs,
                info.get_port()
            );
            let Some(ip) = addrs.iter().find(|a| a.is_ipv4() && !a.is_loopback()) else {
                continue;
            };
            if ip.to_string().starts_with("100.") {
                println!("  skipping tailscale address {ip}");
                continue;
            }
            ping(SocketAddr::new(*ip, info.get_port()));
            return;
        }
    }
    println!("no peer found in 10s");
}

fn ping(addr: SocketAddr) {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).unwrap();
    s.set_nodelay(true).unwrap();
    let mut rtts = Vec::with_capacity(1000);
    let mut buf = [0u8; 16];
    for i in 0..1000u32 {
        buf[..4].copy_from_slice(&i.to_le_bytes());
        let t = Instant::now();
        s.write_all(&buf).unwrap();
        s.read_exact(&mut buf).unwrap();
        rtts.push(t.elapsed());
        std::thread::sleep(Duration::from_millis(2));
    }
    rtts.sort();
    let pct = |p: usize| rtts[(rtts.len() * p / 100).min(rtts.len() - 1)];
    println!(
        "tcp rtt to {addr}: p50 {:?}  p90 {:?}  p99 {:?}  max {:?}",
        pct(50),
        pct(90),
        pct(99),
        rtts[rtts.len() - 1]
    );
}
