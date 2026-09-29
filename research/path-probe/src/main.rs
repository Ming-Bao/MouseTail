//! Spike: can a QUIC handshake complete to one specific address of a peer?
//!   spike-path-probe <ip:port>...    tries each address on its own, reports rtt or failure

use std::net::SocketAddr;

use mousetail_core::{identity::Identity, net};

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("mousetail-path-probe");
    let identity = Identity::load_or_create(&dir).unwrap();
    let endpoint = net::endpoint(&identity, 0).unwrap();
    for arg in std::env::args().skip(1) {
        let addr: SocketAddr = arg.parse().unwrap();
        match net::connect_fastest(&[(endpoint.clone(), addr)]).await {
            Ok(conn) => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                println!("{addr}: connected, rtt {:?}", conn.rtt());
                conn.close(0u32.into(), b"probe");
            }
            Err(e) => println!("{addr}: failed ({e})"),
        }
    }
}
