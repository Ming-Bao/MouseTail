//! QUIC transport. Every node is both client and server on one UDP port.
//!
//! TLS proves each side holds the private key for its certificate; *which* certificates are
//! trusted is decided afterwards by the application from the pinned fingerprints, so an
//! unpaired device can connect far enough to ask to pair and nothing more.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::identity::{Identity, fingerprint};
use crate::proto::{self, ALPN, MAX_FRAME, Message};

const SERVER_NAME: &str = "mousetail";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// After the first path connects, how long to wait for a possibly faster one.
const PATH_GRACE: Duration = Duration::from_millis(150);

/// Send Wake-on-LAN magic packets for `macs` ("aa:bb:cc:dd:ee:ff") to the broadcast address
/// of every local network. Returns how many packets went out.
pub fn wake_on_lan(macs: &[String]) -> usize {
    let mut sent = 0;
    let broadcasts: Vec<(Ipv4Addr, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() => v4.broadcast.map(|b| (v4.ip, b)),
            _ => None,
        })
        .collect();
    for mac in macs.iter().filter_map(|m| parse_mac(m)) {
        let mut packet = vec![0xFF; 6];
        for _ in 0..16 {
            packet.extend_from_slice(&mac);
        }
        for (ip, broadcast) in &broadcasts {
            let Ok(socket) = std::net::UdpSocket::bind((*ip, 0)) else {
                continue;
            };
            if socket.set_broadcast(true).is_err() {
                continue;
            }
            for port in [9, 7] {
                if socket.send_to(&packet, (*broadcast, port)).is_ok() {
                    sent += 1;
                }
            }
        }
    }
    sent
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<u8> = s
        .split([':', '-'])
        .map(|p| u8::from_str_radix(p, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    parts.try_into().ok()
}

/// TLS and transport settings shared by every endpoint of a node.
#[derive(Clone)]
pub struct Crypto {
    server: quinn::ServerConfig,
    client: quinn::ClientConfig,
}

impl Crypto {
    pub fn new(identity: &Identity) -> anyhow::Result<Self> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());

        let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(Arc::new(AnyKeyHolder(provider.clone())))
            .with_single_cert(vec![identity.cert.clone()], identity.key.clone_key())?;
        server.alpn_protocols = vec![ALPN.to_vec()];

        let mut client = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyKeyHolder(provider)))
            .with_client_auth_cert(vec![identity.cert.clone()], identity.key.clone_key())?;
        client.alpn_protocols = vec![ALPN.to_vec()];

        let transport = Arc::new(transport_config());
        let mut server_config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server)?));
        server_config.transport_config(transport.clone());
        let mut client_config =
            quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client)?));
        client_config.transport_config(transport);
        Ok(Self {
            server: server_config,
            client: client_config,
        })
    }

    /// An endpoint (client and server) on one socket.
    pub fn bind(&self, addr: SocketAddr) -> anyhow::Result<Endpoint> {
        let mut endpoint = Endpoint::server(self.server.clone(), addr)
            .with_context(|| format!("binding UDP {addr}"))?;
        endpoint.set_default_client_config(self.client.clone());
        Ok(endpoint)
    }
}

/// Create a single endpoint on all interfaces (tests and tools).
pub fn endpoint(identity: &Identity, port: u16) -> anyhow::Result<Endpoint> {
    Crypto::new(identity)?.bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
}

/// A local IPv4 address with its subnet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct LocalAddr {
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
}

impl LocalAddr {
    fn same_subnet(&self, other: Ipv4Addr) -> bool {
        let m = u32::from(self.netmask);
        u32::from(self.ip) & m == u32::from(other) & m
    }
}

fn local_addrs() -> Vec<LocalAddr> {
    let mut out: Vec<LocalAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() && !v4.ip.is_link_local() => {
                Some(LocalAddr {
                    ip: v4.ip,
                    netmask: v4.netmask,
                })
            }
            _ => None,
        })
        .collect();
    out.sort_by_key(|a| a.ip);
    out.dedup();
    out
}

/// One endpoint per local IPv4 address, all on the same port.
///
/// A single wildcard socket answers every packet from the machine's primary address, so a
/// peer that dialled our Ethernet address gets replies from our Wi-Fi address, which stateful
/// firewalls drop. Binding each address separately makes replies leave from the address that
/// was dialled (and, on macOS, through that address's interface), so every path works and the
/// fastest can win.
pub struct Endpoints {
    crypto: Crypto,
    port: u16,
    bound: std::sync::Mutex<Vec<(LocalAddr, Endpoint)>>,
}

impl Endpoints {
    /// Bind every current address, preferring `port` (any free port if it's taken).
    pub fn new(identity: &Identity, port: u16) -> anyhow::Result<Self> {
        let crypto = Crypto::new(identity)?;
        let addrs = local_addrs();
        anyhow::ensure!(!addrs.is_empty(), "no network connection");
        // Settle the port on the first address, then use it everywhere.
        let first = crypto
            .bind(SocketAddr::from((addrs[0].ip, port)))
            .or_else(|_| crypto.bind(SocketAddr::from((addrs[0].ip, 0))))?;
        let port = first.local_addr()?.port();
        let endpoints = Self {
            crypto,
            port,
            bound: std::sync::Mutex::new(vec![(addrs[0], first)]),
        };
        endpoints.refresh();
        Ok(endpoints)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Follow network changes: bind new addresses, close vanished ones. Returns the newly
    /// bound endpoints so the caller can accept on them.
    pub fn refresh(&self) -> Vec<Endpoint> {
        let now = local_addrs();
        let mut bound = self.bound.lock().unwrap();
        bound.retain(|(a, e)| {
            let keep = now.contains(a);
            if !keep {
                e.close(0u32.into(), b"address gone");
            }
            keep
        });
        let mut added = vec![];
        for a in now {
            if bound.iter().any(|(b, _)| *b == a) {
                continue;
            }
            match self.crypto.bind(SocketAddr::from((a.ip, self.port))) {
                Ok(e) => {
                    bound.push((a, e.clone()));
                    added.push(e);
                }
                Err(e) => tracing::debug!("{e:#}"),
            }
        }
        added
    }

    pub fn all(&self) -> Vec<Endpoint> {
        self.bound
            .lock()
            .unwrap()
            .iter()
            .map(|(_, e)| e.clone())
            .collect()
    }

    /// Which of our endpoints to try each remote address from: those on the same subnet, or
    /// all of them if none is.
    pub fn attempts(&self, remote: &[SocketAddr]) -> Vec<(Endpoint, SocketAddr)> {
        let bound = self.bound.lock().unwrap();
        let mut out = vec![];
        for addr in remote {
            let std::net::IpAddr::V4(ip) = addr.ip() else {
                continue;
            };
            let local: Vec<&Endpoint> = bound
                .iter()
                .filter(|(a, _)| a.same_subnet(ip))
                .map(|(_, e)| e)
                .collect();
            let from = if local.is_empty() {
                bound.iter().map(|(_, e)| e).collect()
            } else {
                local
            };
            out.extend(from.into_iter().map(|e| (e.clone(), *addr)));
        }
        out
    }

    pub fn close(&self) {
        for (_, e) in self.bound.lock().unwrap().iter() {
            e.close(0u32.into(), b"shutdown");
        }
    }
}

fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    // Notice a dead peer quickly so the cursor comes home within about a second.
    t.keep_alive_interval(Some(Duration::from_millis(250)));
    t.max_idle_timeout(Some(
        Duration::from_millis(1500)
            .try_into()
            .expect("small timeout"),
    ));
    t.datagram_receive_buffer_size(Some(64 * 1024));
    t
}

/// Race a connection over every (local endpoint, remote address) path. Once one succeeds,
/// give the others a moment to finish too, then keep the lowest-latency path (e.g. wired over
/// Wi-Fi) and drop the rest.
pub async fn connect_fastest(attempts: &[(Endpoint, SocketAddr)]) -> anyhow::Result<Connection> {
    let mut set = tokio::task::JoinSet::new();
    let mut last = anyhow::anyhow!("no usable addresses");
    for (endpoint, addr) in attempts {
        // Paths our socket can't use (e.g. IPv6 on an IPv4 socket) are simply skipped.
        match endpoint.connect(*addr, SERVER_NAME) {
            Ok(connecting) => {
                set.spawn(async move { tokio::time::timeout(CONNECT_TIMEOUT, connecting).await });
            }
            Err(e) => last = e.into(),
        }
    }
    let mut best: Option<Connection> = None;
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let next = match deadline {
            Some(d) => match tokio::time::timeout_at(d, set.join_next()).await {
                Ok(next) => next,
                Err(_) => break,
            },
            None => set.join_next().await,
        };
        let Some(res) = next else { break };
        match res {
            Ok(Ok(Ok(conn))) => {
                tracing::debug!(
                    "path {:?} → {} connected, rtt {:?}",
                    conn.local_ip(),
                    conn.remote_address(),
                    conn.rtt()
                );
                deadline.get_or_insert_with(|| tokio::time::Instant::now() + PATH_GRACE);
                best = Some(match best.take() {
                    Some(b) if b.rtt() <= conn.rtt() => {
                        conn.close(0u32.into(), b"slower path");
                        b
                    }
                    Some(b) => {
                        b.close(0u32.into(), b"slower path");
                        conn
                    }
                    None => conn,
                });
            }
            Ok(Ok(Err(e))) => {
                tracing::debug!("path failed: {e}");
                last = e.into();
            }
            Ok(Err(_)) => last = anyhow::anyhow!("timed out"),
            Err(e) => last = e.into(),
        }
    }
    set.abort_all();
    best.ok_or(last)
}

/// SHA-256 fingerprint of the peer's certificate.
pub fn peer_fingerprint(conn: &Connection) -> Option<String> {
    let certs = conn
        .peer_identity()?
        .downcast::<Vec<CertificateDer<'static>>>()
        .ok()?;
    certs.first().map(|c| fingerprint(c))
}

pub async fn read_message(recv: &mut RecvStream) -> anyhow::Result<Message> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame too large ({len} bytes)");
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    proto::decode(&buf)
}

pub async fn write_message(send: &mut SendStream, msg: &Message) -> anyhow::Result<()> {
    send.write_all(&proto::frame(msg)).await?;
    Ok(())
}

/// Accepts any certificate but still checks the handshake signature, proving the peer
/// holds the matching private key. Trust is decided by fingerprint afterwards.
#[derive(Debug)]
struct AnyKeyHolder(Arc<CryptoProvider>);

impl AnyKeyHolder {
    fn verify13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl ServerCertVerifier for AnyKeyHolder {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for AnyKeyHolder {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(tag: &str) -> Identity {
        let dir = std::env::temp_dir().join(format!("mousetail-net-{tag}-{}", std::process::id()));
        let id = Identity::load_or_create(&dir).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        id
    }

    #[test]
    fn parses_macs() {
        assert_eq!(
            parse_mac("02:00:5e:10:00:01"),
            Some([0x02, 0x00, 0x5e, 0x10, 0x00, 0x01])
        );
        assert_eq!(parse_mac("nope"), None);
        assert_eq!(parse_mac("02:00:5e"), None);
    }

    #[tokio::test]
    async fn mutual_fingerprints_and_messages() {
        let (a, b) = (identity("a"), identity("b"));
        let ea = endpoint(&a, 0).unwrap();
        let eb = endpoint(&b, 0).unwrap();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, eb.local_addr().unwrap().port()));

        let server = tokio::spawn(async move {
            let conn = eb.accept().await.unwrap().await.unwrap();
            let fp = peer_fingerprint(&conn).unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let msg = read_message(&mut recv).await.unwrap();
            write_message(&mut send, &Message::Leave).await.unwrap();
            let dgram = conn.read_datagram().await.unwrap();
            (fp, msg, dgram)
        });

        let conn = connect_fastest(&[(ea.clone(), addr)]).await.unwrap();
        assert_eq!(peer_fingerprint(&conn).unwrap(), b.fingerprint);
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        write_message(&mut send, &Message::PairRequest)
            .await
            .unwrap();
        assert_eq!(read_message(&mut recv).await.unwrap(), Message::Leave);
        conn.send_datagram(bytes::Bytes::from_static(b"hi"))
            .unwrap();

        let (fp, msg, dgram) = server.await.unwrap();
        assert_eq!(fp, a.fingerprint);
        assert_eq!(msg, Message::PairRequest);
        assert_eq!(&dgram[..], b"hi");
    }
}
