//! The MouseTail node: one per machine. Discovers peers, keeps a connection to each, pairs,
//! and routes input between the local capture/emulation backends and the network.
//!
//! Connection direction doesn't matter: both sides dial each other and the first
//! authenticated connection wins (ties broken deterministically), so a firewall that blocks
//! one direction is fine.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context;
use mousetail_core::audio::{self, AudioPacket};
use mousetail_core::config::{Config, PeerConfig};
use mousetail_core::controller::{Action, Controller};
use mousetail_core::discovery::{self, Found};
use mousetail_core::identity::{Identity, id_from_fingerprint};
use mousetail_core::keys::{CommandRemap, ev};
use mousetail_core::layout::{Point, Rect, Side};
use mousetail_core::net;
use mousetail_core::net::Endpoints;
use mousetail_core::pairing::{self, PakeState, Role};
use mousetail_core::proto::{
    self, Datagram, DisplayInfo, Hello, Message, Motion, PROTOCOL_VERSION, Platform,
};
use mousetail_core::quinn::{Connection, Endpoint};
use mousetail_core::update;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use crate::paths::Paths;
use crate::platform;

/// How long a new connection has to say hello.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a shown pairing code stays valid.
const CODE_LIFETIME: Duration = Duration::from_secs(120);
/// Minimum gap between pairing codes shown on this machine.
const PAIR_REQUEST_INTERVAL: Duration = Duration::from_secs(5);
/// Wrong codes allowed per window before pairing locks for the rest of it. With 4 digits,
/// guessing then takes weeks, with a notification on screen for every try.
const PAIR_MAX_FAILURES: usize = 3;
const PAIR_FAILURE_WINDOW: Duration = Duration::from_secs(600);

/// Machine-wide pairing limits.
#[derive(Default)]
struct PairGuard {
    last_shown: Option<Instant>,
    failures: std::collections::VecDeque<Instant>,
}

impl PairGuard {
    fn allow(&mut self) -> Result<(), String> {
        while self
            .failures
            .front()
            .is_some_and(|t| t.elapsed() > PAIR_FAILURE_WINDOW)
        {
            self.failures.pop_front();
        }
        if self.failures.len() >= PAIR_MAX_FAILURES {
            return Err("too many wrong codes; try again in a few minutes".into());
        }
        if self
            .last_shown
            .is_some_and(|t| t.elapsed() < PAIR_REQUEST_INTERVAL)
        {
            return Err("a code was just shown; wait a moment".into());
        }
        self.last_shown = Some(Instant::now());
        Ok(())
    }

    fn failed(&mut self) {
        self.failures.push_back(Instant::now());
    }

    fn succeeded(&mut self) {
        self.failures.pop_back();
    }
}

/// Locks are only ever taken in this order: config, peers, discovered, controller, target,
/// pairing. Nothing is sent to a peer (which takes `peers`) while a later lock is held.
/// `pair_guard`, `left_peer`, `keyboard_warned` and `displays` are leaves: nothing else is
/// locked while holding them.
pub struct Node {
    id: String,
    name: String,
    identity: Identity,
    config: Mutex<Config>,
    config_path: PathBuf,
    endpoints: Endpoints,
    port: u16,
    peers: Mutex<HashMap<String, Peer>>,
    discovered: Mutex<HashMap<String, Discovered>>,
    pairing: Mutex<HashMap<String, Pairing>>,
    displays: Mutex<Vec<DisplayInfo>>,
    controller: Arc<Mutex<Controller>>,
    /// Set once input capture is running (it may wait for the user to grant permission).
    capture: OnceLock<platform::Capture>,
    capture_error: Mutex<Option<String>>,
    /// Set once input injection is available (it may wait for permission, as on macOS).
    target: OnceLock<Mutex<Target>>,
    /// Hash of the clipboard as last sent or received, so unchanged contents aren't resent
    /// and received contents aren't echoed back.
    clipboard_hash: Arc<Mutex<Option<[u8; 32]>>>,
    keyboard_warned: Mutex<Option<Instant>>,
    pair_guard: Mutex<PairGuard>,
    /// The peer the cursor most recently left and when, so its clipboard is accepted.
    left_peer: Mutex<Option<(String, Instant)>>,
    /// Latest pointer position sent, re-sent once the pointer rests (see `settle_loop`).
    settle: watch::Sender<Option<(String, Motion)>>,
    last_wake: Mutex<HashMap<String, Instant>>,
    /// Plays other machines' sound here (macOS today).
    player: Option<platform::AudioPlayer>,
    /// Our sound going to another machine: (peer, connection, speaker).
    audio_out: Mutex<Option<(String, usize, platform::AudioSource)>>,
    /// Ourselves, for handing work to background threads from `&self` methods.
    me: OnceLock<std::sync::Weak<Node>>,
    /// Keeps this machine up to date (Linux; the Mac app updates itself).
    pub updater: crate::update::Updater,
}

#[derive(Clone)]
struct Peer {
    conn: Connection,
    tx: mpsc::UnboundedSender<Message>,
    hello: Hello,
    fingerprint: String,
    initiator: String,
    paired: bool,
}

struct Discovered {
    found: Found,
    dialing: bool,
    failures: u32,
    next_attempt: Instant,
}

enum Pairing {
    /// We showed a code and wait for the other side to use it. Each code allows exactly one
    /// attempt: after that (right or wrong) a new code is needed, so it can't be guessed.
    Shown {
        code: String,
        expires: Instant,
        key: Option<Vec<u8>>,
    },
    /// We typed a code and wait for the exchange to finish.
    Typed {
        state: Option<PakeState>,
        key: Option<Vec<u8>>,
        done: Option<oneshot::Sender<Result<(), String>>>,
    },
}

/// Receiving side: turns messages from the active controller into injected input.
struct Target {
    emulator: platform::Emulator,
    remap: CommandRemap,
    active: Option<String>,
    remap_active: bool,
    last_seq: u64,
}

impl Node {
    pub async fn run(paths: Paths) -> anyhow::Result<()> {
        let config = Config::load(&paths.config).context("reading config")?;
        let identity = Identity::load_or_create(&paths.dir).context("loading identity")?;
        let id = identity.id();
        let name = config.name.clone().unwrap_or_else(platform::machine_name);
        let endpoints = Endpoints::new(&identity, config.port)?;
        let port = endpoints.port();
        let displays = platform::displays();

        let (action_tx, action_rx) = mpsc::unbounded_channel();
        let controller = Arc::new(Mutex::new(Controller::new(&id, displays.clone())));

        let node = Arc::new(Node {
            id: id.clone(),
            name: name.clone(),
            identity,
            config: Mutex::new(config),
            config_path: paths.config.clone(),
            endpoints,
            port,
            peers: Mutex::default(),
            discovered: Mutex::default(),
            pairing: Mutex::default(),
            displays: Mutex::new(displays),
            controller,
            capture: OnceLock::new(),
            capture_error: Mutex::new(None),
            target: OnceLock::new(),
            clipboard_hash: Arc::default(),
            keyboard_warned: Mutex::new(None),
            pair_guard: Mutex::default(),
            left_peer: Mutex::new(None),
            settle: watch::Sender::new(None),
            last_wake: Mutex::default(),
            player: platform::AudioPlayer::start().ok(),
            audio_out: Mutex::new(None),
            me: OnceLock::new(),
            updater: Default::default(),
        });
        info!(
            "MouseTail {} as {name:?} ({id}) on UDP {port}",
            env!("CARGO_PKG_VERSION"),
        );
        let _ = node.me.set(Arc::downgrade(&node));
        node.add_offline_peers();
        if platform::Capture::supported() {
            tokio::spawn(node.clone().start_capture(action_tx));
        }
        tokio::spawn(node.clone().start_target());

        let (_discovery, found_rx) = discovery::start(&id, &name, port)?;

        for endpoint in node.endpoints.all() {
            tokio::spawn(node.clone().accept_loop(endpoint));
        }
        tokio::spawn(node.clone().network_loop());
        tokio::spawn(node.clone().discovery_loop(found_rx));
        tokio::spawn(node.clone().dial_loop());
        tokio::spawn(node.clone().action_loop(action_rx));
        tokio::spawn(node.clone().settle_loop());
        tokio::spawn(node.clone().display_loop());
        tokio::spawn(crate::ipc::serve(node.clone(), paths.socket.clone()));
        tokio::spawn(crate::update::run(node.clone()));

        shutdown_signal().await;
        info!("shutting down");
        let actions = node.controller.lock().unwrap().release();
        node.apply_actions(actions);
        if let Some(t) = node.target.get() {
            t.lock().unwrap().emulator.release_all();
        }
        node.endpoints.close();
        let _ = std::fs::remove_file(&paths.socket);
        Ok(())
    }

    /// Put paired computers in the layout at their remembered spots before they connect, so
    /// pushing towards one that's asleep can wake it.
    fn add_offline_peers(&self) {
        let peers = self.config.lock().unwrap().peers.clone();
        let mut controller = self.controller.lock().unwrap();
        for p in peers {
            if let Some(offset) = p.placement
                && !p.displays.is_empty()
            {
                controller.set_peer(&p.id, p.displays, offset);
            }
        }
    }

    /// Start injecting input (being controlled), waiting quietly for permission if needed.
    async fn start_target(self: Arc<Self>) {
        let mut logged = false;
        loop {
            match platform::Emulator::start() {
                Ok(emulator) => {
                    let _ = self.target.set(Mutex::new(Target {
                        emulator,
                        remap: CommandRemap::new(platform::command_super_keys()),
                        active: None,
                        remap_active: false,
                        last_seq: 0,
                    }));
                    info!("accepting input from other computers");
                    self.announce();
                    return;
                }
                Err(e) if !platform::Emulator::supported() => {
                    info!("not accepting input here: {e:#}");
                    return;
                }
                Err(e) => {
                    if !logged {
                        warn!("can't accept input yet: {e:#}");
                        logged = true;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Tell connected computers what we can do now (a role just became available).
    fn announce(&self) {
        let hello = Message::Hello(self.hello());
        let ids: Vec<String> = self.peers.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.send(&id, hello.clone());
        }
    }

    /// Start capturing input, waiting quietly for permission if it hasn't been granted yet.
    async fn start_capture(self: Arc<Self>, actions: mpsc::UnboundedSender<Action>) {
        let mut prompt = true;
        loop {
            match platform::Capture::start(self.controller.clone(), actions.clone(), prompt) {
                Ok(c) => {
                    let _ = self.capture.set(c);
                    *self.capture_error.lock().unwrap() = None;
                    info!("capturing keyboard and mouse");
                    let ids: Vec<String> = self.peers.lock().unwrap().keys().cloned().collect();
                    for id in ids {
                        self.peer_up(&id);
                    }
                    self.refresh_edges();
                    self.announce();
                    return;
                }
                Err(e) => {
                    if prompt {
                        warn!("can't capture input yet: {e:#}");
                    }
                    *self.capture_error.lock().unwrap() = Some(format!("{e:#}"));
                }
            }
            prompt = false;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    fn hello(&self) -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            name: self.name.clone(),
            platform: Platform::current(),
            displays: self.displays.lock().unwrap().clone(),
            can_control: platform::Capture::supported(),
            can_be_controlled: self.target.get().is_some(),
            wake_macs: platform::wake_macs(),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Connections

    /// Follow network changes (Wi-Fi joins, cables plugged in) and accept on new addresses.
    async fn network_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            for endpoint in self.endpoints.refresh() {
                debug!("listening on {:?}", endpoint.local_addr());
                tokio::spawn(self.clone().accept_loop(endpoint));
            }
        }
    }

    async fn accept_loop(self: Arc<Self>, endpoint: Endpoint) {
        while let Some(incoming) = endpoint.accept().await {
            let node = self.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => node.run_connection(conn, false).await,
                    Err(e) => debug!("incoming connection failed: {e}"),
                }
            });
        }
    }

    async fn discovery_loop(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<discovery::Event>) {
        while let Some(event) = rx.recv().await {
            let mut discovered = self.discovered.lock().unwrap();
            match event {
                discovery::Event::Found(found) => {
                    debug!(
                        "discovered {} ({}) at {:?}",
                        found.name, found.id, found.addrs
                    );
                    let entry = discovered.entry(found.id.clone()).or_insert(Discovered {
                        found: found.clone(),
                        dialing: false,
                        failures: 0,
                        next_attempt: Instant::now(),
                    });
                    if entry.found.addrs != found.addrs {
                        entry.failures = 0;
                        entry.next_attempt = Instant::now();
                    }
                    let newer = found
                        .version
                        .as_deref()
                        .is_some_and(|v| update::is_newer(v, update::VERSION));
                    entry.found = found;
                    if newer {
                        self.updater.nudge();
                    }
                }
                discovery::Event::Lost(id) => {
                    debug!("{id} stopped advertising");
                    discovered.remove(&id);
                }
            }
        }
    }

    /// Dial every discovered peer we aren't connected to, backing off on failure.
    async fn dial_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let connected: HashSet<String> = self.peers.lock().unwrap().keys().cloned().collect();
            let due: Vec<(String, Vec<SocketAddr>)> = {
                let mut discovered = self.discovered.lock().unwrap();
                discovered
                    .iter_mut()
                    .filter(|(id, d)| {
                        !connected.contains(*id) && !d.dialing && d.next_attempt <= Instant::now()
                    })
                    .map(|(id, d)| {
                        d.dialing = true;
                        (id.clone(), d.found.addrs.clone())
                    })
                    .collect()
            };
            for (id, addrs) in due {
                let node = self.clone();
                tokio::spawn(async move {
                    let result = net::connect_fastest(&node.endpoints.attempts(&addrs)).await;
                    {
                        let mut discovered = node.discovered.lock().unwrap();
                        if let Some(d) = discovered.get_mut(&id) {
                            d.dialing = false;
                            if result.is_err() {
                                d.failures += 1;
                                // Stay eager: on a LAN a failure is usually a brief blip
                                // (Wi-Fi hopping channels, a laptop waking up).
                                let backoff = 1u64 << d.failures.min(2);
                                d.next_attempt = Instant::now() + Duration::from_secs(backoff);
                            } else {
                                d.failures = 0;
                            }
                        }
                    }
                    match result {
                        Ok(conn) => node.run_connection(conn, true).await,
                        Err(e) => debug!("couldn't reach {id}: {e:#}"),
                    }
                });
            }
        }
    }

    async fn run_connection(self: Arc<Self>, conn: Connection, we_dialed: bool) {
        let remote = conn.remote_address();
        if let Err(e) = self.clone().connection(conn, we_dialed).await {
            debug!("connection with {remote} ended: {e:#}");
        }
    }

    async fn connection(self: Arc<Self>, conn: Connection, we_dialed: bool) -> anyhow::Result<()> {
        let fingerprint = net::peer_fingerprint(&conn).context("peer sent no certificate")?;
        let id = id_from_fingerprint(&fingerprint);
        if id == self.id {
            conn.close(0u32.into(), b"self");
            return Ok(());
        }
        let handshake = async {
            let (mut send, mut recv) = if we_dialed {
                conn.open_bi().await?
            } else {
                conn.accept_bi().await?
            };
            net::write_message(&mut send, &Message::Hello(self.hello())).await?;
            let Message::Hello(hello) = net::read_message(&mut recv).await? else {
                anyhow::bail!("expected hello");
            };
            Ok((send, recv, hello))
        };
        let (mut send, mut recv, hello) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
            .await
            .context("handshake timed out")??;
        anyhow::ensure!(
            hello.protocol == PROTOCOL_VERSION,
            "{} speaks protocol {}, we speak {PROTOCOL_VERSION}",
            hello.name,
            hello.protocol
        );
        let initiator = if we_dialed {
            self.id.clone()
        } else {
            id.clone()
        };
        let paired = self.config.lock().unwrap().is_paired(&fingerprint);

        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let peer = Peer {
            conn: conn.clone(),
            tx,
            hello: hello.clone(),
            fingerprint,
            initiator: initiator.clone(),
            paired,
        };
        let replaced = {
            let mut peers = self.peers.lock().unwrap();
            let mut replaced = false;
            if let Some(existing) = peers.get(&id) {
                // Both sides apply the same rule: keep the connection started by the smaller
                // id; a newer connection from the same initiator replaces a stale one.
                let keep_new = existing.initiator == initiator || initiator < existing.initiator;
                if !keep_new {
                    conn.close(0u32.into(), b"duplicate");
                    return Ok(());
                }
                existing.conn.close(0u32.into(), b"replaced");
                replaced = true;
            }
            peers.insert(id.clone(), peer);
            replaced
        };
        if replaced {
            // Whatever was in flight on the old link is lost (key releases included), so
            // start clean: cursor home, remote keys released.
            self.peer_down(&id);
        }
        info!(
            "connected to {} ({id}) via {} [{}]",
            hello.name,
            conn.remote_address(),
            if paired { "paired" } else { "not paired" }
        );
        self.peer_up(&id);

        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if net::write_message(&mut send, &msg).await.is_err() {
                    break;
                }
            }
        });

        let stable_id = conn.stable_id();
        let datagrams = {
            let node = self.clone();
            let conn = conn.clone();
            let id = id.clone();
            async move {
                while let Ok(bytes) = conn.read_datagram().await {
                    match proto::decode::<Datagram>(&bytes) {
                        Ok(Datagram::Motion(motion)) => node.on_motion(&id, stable_id, motion),
                        Ok(Datagram::Audio(packet)) => node.on_audio(&id, stable_id, packet),
                        Err(_) => {}
                    }
                }
            }
        };
        let reader = {
            let node = self.clone();
            let id = id.clone();
            async move {
                loop {
                    let msg = net::read_message(&mut recv).await?;
                    node.on_message(&id, stable_id, msg);
                }
                #[allow(unreachable_code)]
                Ok::<(), anyhow::Error>(())
            }
        };
        let result = tokio::select! {
            r = reader => r,
            _ = datagrams => Ok(()),
        };
        writer.abort();

        let removed = {
            let mut peers = self.peers.lock().unwrap();
            match peers.get(&id) {
                Some(p) if p.conn.stable_id() == conn.stable_id() => peers.remove(&id).is_some(),
                _ => false,
            }
        };
        if removed {
            info!("disconnected from {}", hello.name);
            self.peer_down(&id);
        }
        result
    }

    fn peer(&self, id: &str) -> Option<Peer> {
        self.peers.lock().unwrap().get(id).cloned()
    }

    fn send(&self, id: &str, msg: Message) {
        if let Some(p) = self.peers.lock().unwrap().get(id) {
            let _ = p.tx.send(msg);
        }
    }

    /// A paired peer is connected (or its displays changed): put it in the layout.
    fn peer_up(&self, id: &str) {
        let Some(peer) = self.peer(id) else { return };
        if !peer.paired {
            return;
        }
        // Remember what it looks like, so it can be shown and woken while offline.
        let known = self
            .config
            .lock()
            .unwrap()
            .peer(id)
            .map(|p| (p.displays.clone(), p.wake_macs.clone(), p.name.clone()));
        let now = (
            peer.hello.displays.clone(),
            peer.hello.wake_macs.clone(),
            peer.hello.name.clone(),
        );
        if known.is_some_and(|k| k != now) {
            self.update_config(|c| {
                if let Some(p) = c.peer_mut(id) {
                    (p.displays, p.wake_macs, p.name) = now;
                }
            });
        }
        self.request_audio(id, &peer);
        self.share_placement(id);
        if self.capture.get().is_none() || !peer.hello.can_be_controlled {
            return;
        }
        let placement = self
            .config
            .lock()
            .unwrap()
            .peer(id)
            .and_then(|p| p.placement);
        let mut controller = self.controller.lock().unwrap();
        let displays = peer.hello.displays.clone();
        let mut actions = controller.set_peer(id, displays.clone(), placement.unwrap_or_default());
        // First time: put it to the left of the primary display. Easy to change later.
        let first_placement = placement
            .is_none()
            .then(|| controller.offset_beside(id, Side::Left, None))
            .flatten();
        if let Some(offset) = first_placement {
            actions.extend(controller.set_peer(id, displays, offset));
        }
        actions.extend(controller.set_reachable(id, true));
        drop(controller);
        self.refresh_edges();
        if let Some(offset) = first_placement {
            self.update_config(|c| {
                if let Some(p) = c.peer_mut(id) {
                    p.placement = Some(offset);
                }
            });
        }
        self.apply_actions(actions);
    }

    /// Tell the capture backend which of our edges lead to other computers.
    fn refresh_edges(&self) {
        let Some(capture) = self.capture.get() else {
            return;
        };
        let displays = self.displays.lock().unwrap().clone();
        let sides = self.controller.lock().unwrap().layout().exit_sides(0);
        let edges = sides
            .into_iter()
            .filter_map(|(d, side)| {
                displays.get(d).map(|display| platform::Edge {
                    display: display.id.clone(),
                    side,
                })
            })
            .collect();
        capture.set_edges(edges);
    }

    /// Tell a peer where it sits in our arrangement, so it can mirror it.
    fn share_placement(&self, id: &str) {
        let placed = self
            .config
            .lock()
            .unwrap()
            .peer(id)
            .and_then(|p| p.placement.map(|o| (o, p.placement_updated)));
        if let Some((o, updated)) = placed {
            self.send(
                id,
                Message::Placement {
                    x: o.x,
                    y: o.y,
                    updated,
                },
            );
        }
    }

    /// A peer told us where we sit in its arrangement. Mirror it if it's newer than ours (or
    /// we have none), so arranging on either computer arranges both. Ties between two
    /// automatic placements go to the computer with the smaller id, so both agree.
    fn adopt_placement(&self, id: &str, us_in_theirs: Point, updated: u64) {
        let mirror = Point::new(-us_in_theirs.x, -us_in_theirs.y);
        let (own, own_updated) = match self.config.lock().unwrap().peer(id) {
            Some(p) => (p.placement, p.placement_updated),
            None => return,
        };
        let newer = updated > own_updated || (updated == own_updated && id < self.id.as_str());
        if own == Some(mirror) {
            return;
        }
        if own.is_some() && !newer {
            // Ours wins. Say so, in case they never heard it (e.g. it arrived before they
            // finished pairing), so both computers end up with the same arrangement.
            self.share_placement(id);
            return;
        }
        debug!("adopting {id}'s arrangement");
        self.update_config(|c| {
            if let Some(p) = c.peer_mut(id) {
                p.placement = Some(mirror);
                p.placement_updated = updated;
            }
        });
        self.peer_up(id);
    }

    fn peer_down(&self, id: &str) {
        self.stop_audio(Some(id));
        let actions = self.controller.lock().unwrap().set_reachable(id, false);
        self.apply_actions(actions);
        let was_controlling_us = self.target.get().is_some_and(|t| {
            let mut t = t.lock().unwrap();
            let active = t.active.as_deref() == Some(id);
            if active {
                t.leave();
            }
            active
        });
        if was_controlling_us {
            self.controller.lock().unwrap().set_suspended(false);
        }
        self.pairing.lock().unwrap().remove(id);
    }

    // -----------------------------------------------------------------------------------------
    // Controlling side

    async fn action_loop(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Action>) {
        while let Some(action) = rx.recv().await {
            self.apply_actions(vec![action]);
        }
    }

    fn apply_actions(&self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Grab | Action::Release { .. } => {
                    if let Some(c) = self.capture.get() {
                        c.apply(&action);
                    }
                }
                Action::Send { peer, msg } => {
                    if matches!(msg, Message::Leave) {
                        *self.left_peer.lock().unwrap() = Some((peer.clone(), Instant::now()));
                    }
                    let entering = matches!(msg, Message::Enter { .. });
                    self.send(&peer, msg);
                    if entering {
                        debug!("cursor → {peer}");
                        // Our clipboard travels with the cursor. After Enter on the same
                        // ordered stream, so the other side knows it's part of the crossing.
                        self.push_clipboard(&peer);
                        self.warn_if_keyboard_blocked();
                        // The other side starts each visit with Caps Lock off; match ours.
                        if platform::caps_lock_on() {
                            for down in [true, false] {
                                self.send(
                                    &peer,
                                    Message::Key {
                                        code: ev::CAPSLOCK,
                                        down,
                                    },
                                );
                            }
                        }
                    }
                }
                Action::Motion { peer, motion } => {
                    self.send_motion(&peer, motion);
                    self.settle.send_replace(Some((peer, motion)));
                }
                Action::Wake { peer } => self.wake(&peer),
            }
        }
    }

    /// Someone pushed the cursor towards a paired computer that's offline: send it a
    /// Wake-on-LAN packet (at most every 10 s; the edge gets pushed many times a second).
    fn wake(&self, id: &str) {
        let Some(peer) = self.config.lock().unwrap().peer(id).cloned() else {
            return;
        };
        {
            let mut last = self.last_wake.lock().unwrap();
            if last
                .get(id)
                .is_some_and(|t: &Instant| t.elapsed() < Duration::from_secs(10))
            {
                return;
            }
            last.insert(id.to_string(), Instant::now());
        }
        if peer.wake_macs.is_empty() {
            debug!("{} is offline and can't be woken remotely", peer.name);
            return;
        }
        info!("waking {}", peer.name);
        tokio::task::spawn_blocking(move || net::wake_on_lan(&peer.wake_macs));
    }

    fn send_motion(&self, peer: &str, motion: Motion) {
        if let Some(p) = self.peers.lock().unwrap().get(peer) {
            let _ = p
                .conn
                .send_datagram(proto::encode(&Datagram::Motion(motion)).into());
        }
    }

    /// Motion travels as unreliable datagrams where the newest wins, so a lost final packet
    /// would leave the remote cursor short of where it should be until the next move. Once
    /// the pointer rests, send the last position again (the receiver ignores repeats).
    async fn settle_loop(self: Arc<Self>) {
        let mut rx = self.settle.subscribe();
        while rx.changed().await.is_ok() {
            loop {
                tokio::select! {
                    changed = rx.changed() => if changed.is_err() { return },
                    _ = tokio::time::sleep(Duration::from_millis(40)) => break,
                }
            }
            let last = rx.borrow_and_update().clone();
            if let Some((peer, motion)) = last {
                for _ in 0..2 {
                    self.send_motion(&peer, motion);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }
        }
    }

    /// Watch for display changes (plugging in a monitor, changing resolution).
    async fn display_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            let now = tokio::task::spawn_blocking(platform::displays)
                .await
                .unwrap_or_default();
            if now.is_empty() || *self.displays.lock().unwrap() == now {
                continue;
            }
            info!("displays changed");
            *self.displays.lock().unwrap() = now.clone();
            self.controller
                .lock()
                .unwrap()
                .set_local_displays(now.clone());
            self.refresh_edges();
            if let Some(t) = self.target.get()
                && let Some(bounds) = Rect::bounding(now.iter().map(|d| d.rect))
            {
                t.lock().unwrap().emulator.set_bounds(bounds);
            }
            let ids: Vec<String> = self.peers.lock().unwrap().keys().cloned().collect();
            for id in ids {
                self.send(&id, Message::Displays(now.clone()));
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Messages

    fn on_motion(&self, id: &str, conn: usize, motion: Motion) {
        if !self.is_current(id, conn) {
            return;
        }
        if let Some(t) = self.target.get() {
            let mut t = t.lock().unwrap();
            if t.active.as_deref() == Some(id) && motion.seq > t.last_seq {
                t.last_seq = motion.seq;
                t.emulator.motion(motion.x, motion.y);
            }
        }
    }

    /// Is `conn` the connection we're using for `id`? Messages from a replaced connection are
    /// dropped (its certificate may not even be the one we have on record).
    fn is_current(&self, id: &str, conn: usize) -> bool {
        self.peers
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|p| p.conn.stable_id() == conn)
    }

    fn on_message(&self, id: &str, conn: usize, msg: Message) {
        let Some(peer) = self.peer(id) else { return };
        if peer.conn.stable_id() != conn {
            return;
        }
        match msg {
            Message::PairRequest => self.pair_show_code(id, &peer),
            Message::PairSpake(m) => self.pair_spake(id, &peer, m),
            Message::PairConfirm(tag) => self.pair_confirm(id, &peer, tag),
            Message::PairFailed(reason) => {
                debug!("pairing with {} failed: {reason}", peer.hello.name);
                self.pair_finish(id, Err(reason));
            }
            _ if !peer.paired => debug!("ignoring message from unpaired {}", peer.hello.name),
            Message::Hello(hello) => {
                if let Some(p) = self.peers.lock().unwrap().get_mut(id) {
                    p.hello = hello;
                }
                self.peer_up(id);
            }
            Message::Displays(displays) => {
                if let Some(p) = self.peers.lock().unwrap().get_mut(id) {
                    p.hello.displays = displays;
                }
                self.peer_up(id);
            }
            Message::Clipboard { mime, data } => self.receive_clipboard(id, &peer, mime, data),
            Message::Placement { x, y, updated } => {
                self.adopt_placement(id, Point::new(x, y), updated)
            }
            Message::AudioWanted(wanted) => {
                // Setting up the speaker can take a moment; keep this connection's input
                // flowing meanwhile.
                if let Some(node) = self.me.get().and_then(std::sync::Weak::upgrade) {
                    let id = id.to_string();
                    tokio::task::spawn_blocking(move || node.audio_wanted(&id, &peer, wanted));
                }
            }
            Message::Leave => {
                // The cursor is leaving us: our clipboard goes with it.
                let was_active = self
                    .target
                    .get()
                    .is_some_and(|t| t.lock().unwrap().active.as_deref() == Some(id));
                self.on_input(id, &peer, Message::Leave);
                if was_active {
                    self.controller.lock().unwrap().set_suspended(false);
                    self.push_clipboard(id);
                }
            }
            input => {
                if matches!(input, Message::Enter { .. }) && self.target.get().is_some() {
                    // Being controlled: our own capture stands down until they leave.
                    self.controller.lock().unwrap().set_suspended(true);
                }
                self.on_input(id, &peer, input)
            }
        }
    }

    fn on_input(&self, id: &str, peer: &Peer, msg: Message) {
        let Some(t) = self.target.get() else { return };
        let mut t = t.lock().unwrap();
        match msg {
            Message::Enter { x, y } => {
                if t.active.as_deref() != Some(id) {
                    t.leave();
                }
                t.active = Some(id.to_string());
                t.remap_active = peer.hello.platform == Platform::MacOs
                    && Platform::current() != Platform::MacOs;
                t.last_seq = 0;
                platform::on_enter();
                t.emulator.motion(x, y);
            }
            _ if t.active.as_deref() != Some(id) => {}
            Message::Leave => t.leave(),
            Message::Button { code, down, x, y } => {
                t.emulator.motion(x, y);
                for (code, down) in t.map(code, down) {
                    t.emulator.button_or_key(code, down);
                }
            }
            Message::Key { code, down } => {
                tracing::trace!("key {code} {}", if down { "down" } else { "up" });
                for (code, down) in t.map(code, down) {
                    t.emulator.button_or_key(code, down);
                }
            }
            Message::Scroll(s) => t.emulator.scroll(s),
            _ => {}
        }
    }

    // -----------------------------------------------------------------------------------------
    // Sound: the machine with the speakers asks; the other streams into it.

    /// If we can play sound, ask a paired machine that can't to send us its sound (or stop).
    fn request_audio(&self, id: &str, peer: &Peer) {
        if self.player.is_none() || !peer.paired || peer.hello.platform == Platform::current() {
            return;
        }
        let wanted = self.config.lock().unwrap().settings.audio;
        self.send(id, Message::AudioWanted(wanted));
    }

    fn on_audio(&self, id: &str, conn: usize, packet: AudioPacket) {
        let Some(player) = &self.player else { return };
        let paired = self
            .peers
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|p| p.paired && p.conn.stable_id() == conn);
        if paired && self.config.lock().unwrap().settings.audio {
            player.play(packet);
        }
    }

    fn audio_wanted(&self, id: &str, peer: &Peer, wanted: bool) {
        if !wanted || !self.config.lock().unwrap().settings.audio {
            self.stop_audio(Some(id));
            return;
        }
        let conn = peer.conn.clone();
        {
            let current = self.audio_out.lock().unwrap();
            if current
                .as_ref()
                .is_some_and(|(p, c, _)| p == id && *c == conn.stable_id())
            {
                return; // already streaming to them
            }
        }
        self.stop_audio(None);
        let (speaker, pcm) = match platform::AudioSource::start(id, &peer.hello.name) {
            Ok(s) => s,
            Err(e) => {
                debug!("not sharing sound: {e:#}");
                return;
            }
        };
        info!("sound now plays on {}", peer.hello.name);
        *self.audio_out.lock().unwrap() = Some((id.to_string(), conn.stable_id(), speaker));
        std::thread::Builder::new()
            .name("audio-encode".into())
            .spawn(move || stream_audio(pcm, conn))
            .ok();
    }

    /// Stop sending sound to `id` (or to anyone). Removes the virtual speaker.
    fn stop_audio(&self, id: Option<&str>) {
        let mut out = self.audio_out.lock().unwrap();
        if out
            .as_ref()
            .is_some_and(|(p, _, _)| id.is_none_or(|id| id == p))
        {
            let speaker = out.take();
            drop(out);
            // Dropping the speaker restores the previous output; that shells out, so do it
            // off the async runtime.
            std::thread::spawn(move || drop(speaker));
        }
    }

    /// Typing silently doing nothing is baffling, so say why (at most once a minute).
    fn warn_if_keyboard_blocked(&self) {
        if !platform::keyboard_blocked() {
            return;
        }
        let mut last = self.keyboard_warned.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        *last = Some(Instant::now());
        warn!("keyboard blocked by Secure Input on this machine");
        platform::notify(
            "Keyboard paused",
            "A password field or locked screen on this Mac is blocking typing. The mouse still works.",
        );
    }

    // -----------------------------------------------------------------------------------------
    // Clipboard: carried across on each crossing, text for now.

    fn push_clipboard(&self, to: &str) {
        let (enabled, max) = {
            let c = self.config.lock().unwrap();
            (c.settings.clipboard, c.settings.clipboard_max_bytes)
        };
        if !enabled {
            return;
        }
        // Reading the clipboard can take a few milliseconds; keep it off the input path.
        let Some(tx) = self.peer(to).map(|p| p.tx) else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let last_hash = self.clipboard_hash.clone();
        rt.spawn_blocking(move || {
            let Some((mime, data)) = platform::clipboard_get() else {
                return;
            };
            if data.len() > max {
                debug!("clipboard too large to send ({} bytes)", data.len());
                return;
            }
            let hash = clipboard_hash(&data);
            let mut last = last_hash.lock().unwrap();
            if *last == Some(hash) {
                return;
            }
            *last = Some(hash);
            drop(last);
            debug!("sending clipboard ({} bytes)", data.len());
            let _ = tx.send(Message::Clipboard { mime, data });
        });
    }

    fn receive_clipboard(&self, id: &str, peer: &Peer, mime: String, data: Vec<u8>) {
        let (enabled, max) = {
            let c = self.config.lock().unwrap();
            (c.settings.clipboard, c.settings.clipboard_max_bytes)
        };
        // Only as part of a crossing: from the machine controlling us, or the one the cursor
        // just left. A paired machine can't rewrite the clipboard whenever it likes.
        let controlling_us = self
            .target
            .get()
            .is_some_and(|t| t.lock().unwrap().active.as_deref() == Some(id));
        let just_left = self
            .left_peer
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(p, t)| p == id && t.elapsed() < Duration::from_secs(5));
        if !enabled || data.len() > max || !(controlling_us || just_left) {
            debug!("ignoring clipboard from {}", peer.hello.name);
            return;
        }
        debug!("clipboard from {} ({} bytes)", peer.hello.name, data.len());
        *self.clipboard_hash.lock().unwrap() = Some(clipboard_hash(&data));
        tokio::task::spawn_blocking(move || platform::clipboard_set(&mime, &data));
    }

    // -----------------------------------------------------------------------------------------
    // Pairing (see mousetail_core::pairing)

    /// The other side asked to pair: show a code here.
    fn pair_show_code(&self, id: &str, peer: &Peer) {
        // Limits apply to this machine as a whole, not per requester: a new certificate is
        // free, so per-peer limits would do nothing against someone guessing codes.
        let allowed = self.pair_guard.lock().unwrap().allow();
        if let Err(reason) = allowed {
            warn!(
                "ignoring pairing request from {}: {reason}",
                peer.hello.name
            );
            self.send(id, Message::PairFailed(reason));
            return;
        }
        let code = pairing::new_code();
        info!("pairing requested by {}: code {code}", peer.hello.name);
        platform::notify(
            "MouseTail pairing",
            &format!("Enter {code} on {} to connect it", peer.hello.name),
        );
        self.pairing.lock().unwrap().insert(
            id.to_string(),
            Pairing::Shown {
                code,
                expires: Instant::now() + CODE_LIFETIME,
                key: None,
            },
        );
    }

    /// We typed the code shown on the other machine.
    pub async fn pair_with_code(&self, id: &str, code: &str) -> Result<(), String> {
        let (state, msg) = pairing::start(code);
        let (done_tx, done_rx) = oneshot::channel();
        self.pairing.lock().unwrap().insert(
            id.to_string(),
            Pairing::Typed {
                state: Some(state),
                key: None,
                done: Some(done_tx),
            },
        );
        self.send(id, Message::PairSpake(msg));
        match tokio::time::timeout(Duration::from_secs(15), done_rx).await {
            Ok(Ok(result)) => result,
            _ => {
                self.pairing.lock().unwrap().remove(id);
                Err("no answer from the other machine".into())
            }
        }
    }

    fn pair_spake(&self, id: &str, peer: &Peer, msg: Vec<u8>) {
        let mut pairing = self.pairing.lock().unwrap();
        match pairing.get_mut(id) {
            Some(Pairing::Shown {
                code, expires, key, ..
            }) => {
                if Instant::now() > *expires || key.is_some() {
                    // Expired, or already used for an attempt.
                    pairing.remove(id);
                    drop(pairing);
                    self.send(
                        id,
                        Message::PairFailed("that code has expired; ask for a new one".into()),
                    );
                    return;
                }
                // Count the attempt now: the tag we send back lets the other side check its
                // guess offline, so it may never report a failure. Success refunds it.
                self.pair_guard.lock().unwrap().failed();
                let (state, mine) = pairing::start(code);
                match pairing::finish(state, &msg) {
                    Ok(k) => {
                        let tag = pairing::tag(
                            &k,
                            Role::Responder,
                            &peer.fingerprint,
                            &self.identity.fingerprint,
                        );
                        *key = Some(k);
                        drop(pairing);
                        self.send(id, Message::PairSpake(mine));
                        self.send(id, Message::PairConfirm(tag));
                    }
                    Err(_) => {
                        pairing.remove(id);
                        drop(pairing);
                        self.send(id, Message::PairFailed("pairing exchange failed".into()));
                    }
                }
            }
            Some(Pairing::Typed { state, key, .. }) => {
                if let Some(s) = state.take() {
                    *key = pairing::finish(s, &msg).ok();
                }
            }
            None => {
                drop(pairing);
                self.send(
                    id,
                    Message::PairFailed("that code has expired; ask for a new one".into()),
                );
            }
        }
    }

    fn pair_confirm(&self, id: &str, peer: &Peer, tag: Vec<u8>) {
        // Decide under the lock, act after dropping it (sending takes the peers lock).
        let (ok, shown, reply) = {
            let pairing = self.pairing.lock().unwrap();
            match pairing.get(id) {
                // We showed the code: the typer confirms it knew it.
                Some(Pairing::Shown { key: Some(k), .. }) => {
                    let ok = pairing::verify(
                        k,
                        Role::Initiator,
                        &peer.fingerprint,
                        &self.identity.fingerprint,
                        &tag,
                    );
                    (ok, true, None)
                }
                // We typed the code: check the shower's tag, then send ours.
                Some(Pairing::Typed { key: Some(k), .. }) => {
                    let ok = pairing::verify(
                        k,
                        Role::Responder,
                        &self.identity.fingerprint,
                        &peer.fingerprint,
                        &tag,
                    );
                    let reply = ok.then(|| {
                        pairing::tag(
                            k,
                            Role::Initiator,
                            &self.identity.fingerprint,
                            &peer.fingerprint,
                        )
                    });
                    (ok, false, reply)
                }
                _ => return,
            }
        };
        if let Some(mine) = reply {
            self.send(id, Message::PairConfirm(mine));
        }
        if ok {
            self.pin(id, peer);
            if shown {
                self.pair_guard.lock().unwrap().succeeded();
                platform::notify("MouseTail", &format!("Paired with {}", peer.hello.name));
            }
            self.pair_finish(id, Ok(()));
        } else {
            if shown {
                warn!("{} entered the wrong pairing code", peer.hello.name);
            }
            // Either way, tell the other side so it retires its code now.
            self.send(id, Message::PairFailed("wrong code".into()));
            self.pair_finish(id, Err("wrong code".into()));
        }
    }

    fn pair_finish(&self, id: &str, result: Result<(), String>) {
        if let Some(Pairing::Typed {
            done: Some(done), ..
        }) = self.pairing.lock().unwrap().remove(id)
        {
            let _ = done.send(result);
        }
    }

    fn pin(&self, id: &str, peer: &Peer) {
        info!("paired with {} ({id})", peer.hello.name);
        self.update_config(|c| {
            // Pairing again (same computer) keeps where it sits on the desk.
            let (placement, placement_updated) = c
                .peer(id)
                .map(|p| (p.placement, p.placement_updated))
                .unwrap_or((None, 0));
            c.add_peer(PeerConfig {
                id: id.to_string(),
                name: peer.hello.name.clone(),
                fingerprint: peer.fingerprint.clone(),
                placement,
                placement_updated,
                displays: peer.hello.displays.clone(),
                wake_macs: peer.hello.wake_macs.clone(),
            })
        });
        if let Some(p) = self.peers.lock().unwrap().get_mut(id) {
            p.paired = true;
        }
        self.peer_up(id);
    }

    fn update_config(&self, f: impl FnOnce(&mut Config)) {
        let mut config = self.config.lock().unwrap();
        f(&mut config);
        if let Err(e) = config.save(&self.config_path) {
            warn!("couldn't save config: {e:#}");
        }
    }

    // -----------------------------------------------------------------------------------------
    // Local control (IPC)

    pub fn status(&self) -> Value {
        let config = self.config.lock().unwrap();
        let peers = self.peers.lock().unwrap();
        let discovered = self.discovered.lock().unwrap();
        let controller = self.controller.lock().unwrap();
        let controlled_by = self
            .target
            .get()
            .and_then(|t| t.lock().unwrap().active.clone());

        let mut ids: Vec<String> = config.peers.iter().map(|p| p.id.clone()).collect();
        ids.extend(peers.keys().cloned());
        ids.extend(discovered.keys().cloned());
        ids.sort();
        ids.dedup();
        let list: Vec<Value> = ids
            .iter()
            .map(|id| {
                let conn = peers.get(id);
                let name = conn
                    .map(|p| p.hello.name.clone())
                    .or_else(|| config.peer(id).map(|p| p.name.clone()))
                    .or_else(|| discovered.get(id).map(|d| d.found.name.clone()))
                    .unwrap_or_else(|| id.clone());
                json!({
                    "id": id,
                    "name": name,
                    "paired": config.peer(id).is_some(),
                    "connected": conn.is_some(),
                    "address": conn.map(|p| p.conn.remote_address().to_string()),
                    "rtt_ms": conn.map(|p| p.conn.rtt().as_secs_f64() * 1000.0),
                    "platform": conn.map(|p| p.hello.platform),
                    "placement": config.peer(id).and_then(|p| p.placement),
                    "version": discovered.get(id).and_then(|d| d.found.version.clone()),
                })
            })
            .collect();
        // A code we're showing right now, so every UI can display it, not just the
        // notification.
        let pairing_code = self
            .pairing
            .lock()
            .unwrap()
            .iter()
            .find_map(|(id, p)| match p {
                Pairing::Shown { code, expires, .. } if *expires > Instant::now() => Some(json!({
                    "peer": id,
                    "name": peers.get(id).map(|p| p.hello.name.clone()),
                    "code": code,
                })),
                _ => None,
            });
        json!({
            "id": self.id,
            "name": self.name,
            "port": self.port,
            "pairing_code": pairing_code,
            "can_control": self.capture.get().is_some(),
            "keyboard_blocked": platform::keyboard_blocked(),
            "capture_error": *self.capture_error.lock().unwrap(),
            "can_be_controlled": self.target.get().is_some(),
            "displays": *self.displays.lock().unwrap(),
            "controlling": controller.active_peer(),
            "controlled_by": controlled_by,
            "settings": config.settings,
            "peers": list,
            "version": update::VERSION,
            "update": self.updater.status(),
        })
    }

    /// Resolve a user's peer reference (name, id prefix) among connected peers.
    pub fn resolve_peer(&self, query: Option<&str>, want_unpaired: bool) -> Result<String, String> {
        let peers = self.peers.lock().unwrap();
        let matches: Vec<&String> = peers
            .iter()
            .filter(|(id, p)| match query {
                Some(q) => id.starts_with(q) || p.hello.name.eq_ignore_ascii_case(q),
                None => !want_unpaired || !p.paired,
            })
            .map(|(id, _)| id)
            .collect();
        match matches.as_slice() {
            [id] => Ok((*id).clone()),
            [] => Err(match query {
                Some(q) => format!("no connected machine matches {q:?}"),
                None => "no other MouseTail machine found on the network yet".into(),
            }),
            _ => Err("more than one machine matches; name one".into()),
        }
    }

    pub fn request_pairing(&self, id: &str) {
        self.send(id, Message::PairRequest);
    }

    pub fn unpair(&self, query: &str) -> Result<String, String> {
        let removed = {
            let mut config = self.config.lock().unwrap();
            let Some(peer) = config.find_peer(query).cloned() else {
                return Err(format!("not paired with {query:?}"));
            };
            config.peers.retain(|p| p.id != peer.id);
            if let Err(e) = config.save(&self.config_path) {
                warn!("couldn't save config: {e:#}");
            }
            peer
        };
        if let Some(p) = self.peers.lock().unwrap().get_mut(&removed.id) {
            p.paired = false;
            p.conn.close(0u32.into(), b"unpaired");
        }
        Ok(removed.name)
    }

    /// Put a peer beside one of this machine's displays.
    pub fn place(&self, query: &str, side: Side, display: Option<usize>) -> Result<Point, String> {
        let id = self.resolve_peer(Some(query), false)?;
        let offset = {
            let controller = self.controller.lock().unwrap();
            controller
                .offset_beside(&id, side, display)
                .ok_or("that machine isn't in the layout (is it paired and connected?)")?
        };
        self.update_config(|c| {
            if let Some(p) = c.peer_mut(&id) {
                p.placement = Some(offset);
                p.placement_updated = now_ms();
            }
        });
        self.peer_up(&id);
        Ok(offset)
    }

    /// Every machine's displays and where they sit, for the arrangement view.
    pub fn layout(&self) -> Value {
        let config = self.config.lock().unwrap();
        let peers = self.peers.lock().unwrap();
        let controller = self.controller.lock().unwrap();
        let mut machines = vec![json!({
            "id": self.id,
            "name": self.name,
            "this": true,
            "connected": true,
            "displays": *self.displays.lock().unwrap(),
            "offset": Point::default(),
        })];
        for p in &config.peers {
            let live = peers.get(&p.id);
            let placed = controller
                .layout()
                .machines
                .iter()
                .find(|m| m.id == p.id)
                .map(|m| m.offset);
            machines.push(json!({
                "id": p.id,
                "name": live.map(|l| l.hello.name.clone()).unwrap_or_else(|| p.name.clone()),
                "this": false,
                "connected": live.is_some(),
                "displays": live.map(|l| l.hello.displays.clone()).unwrap_or_else(|| p.displays.clone()),
                "offset": placed.or(p.placement),
            }));
        }
        // Include offline machines at their remembered spots, so the view still shows where
        // they'll connect.
        let mut all = controller.layout().clone();
        for p in &config.peers {
            if all.machine_index(&p.id).is_none()
                && let Some(offset) = p.placement
            {
                all.machines.push(mousetail_core::layout::Machine {
                    id: p.id.clone(),
                    displays: p.displays.clone(),
                    offset,
                });
            }
        }
        let crossings = all.crossing_edges();
        json!({
            "machines": machines,
            // Exactly where the cursor can pass between computers, straight from the logic
            // that moves it, so the arrangement view can't disagree with reality.
            "crossings": crossings,
        })
    }

    /// Drop a machine at `desired` (layout coordinates); it snaps to the nearest valid spot.
    pub fn place_at(&self, query: &str, desired: Point) -> Result<Point, String> {
        let id = self
            .config
            .lock()
            .unwrap()
            .find_peer(query)
            .map(|p| p.id.clone())
            .ok_or_else(|| format!("not paired with {query:?}"))?;
        // Lock order everywhere: config before controller.
        let remembered = self
            .config
            .lock()
            .unwrap()
            .peer(&id)
            .map(|p| p.displays.clone());
        let offset = {
            let mut controller = self.controller.lock().unwrap();
            if controller.layout().machine_index(&id).is_none() {
                // Offline: lay it out from its remembered displays.
                controller.set_peer(&id, remembered.clone().unwrap_or_default(), desired);
            }
            controller
                .snap(&id, desired)
                .ok_or("there's nowhere valid to put it")?
        };
        self.update_config(|c| {
            if let Some(p) = c.peer_mut(&id) {
                p.placement = Some(offset);
                p.placement_updated = now_ms();
            }
        });
        let live = self.peer(&id).map(|p| p.hello.displays);
        let actions = self.controller.lock().unwrap().set_peer(
            &id,
            live.or(remembered).unwrap_or_default(),
            offset,
        );
        self.apply_actions(actions);
        self.peer_up(&id);
        Ok(offset)
    }

    pub fn settings(&self) -> Value {
        json!(self.config.lock().unwrap().settings)
    }

    pub fn set_setting(&self, key: &str, value: &Value) -> Result<(), String> {
        let result = self.apply_setting(key, value);
        if result.is_ok() && key == "audio" {
            let peers: Vec<(String, Peer)> = self
                .peers
                .lock()
                .unwrap()
                .iter()
                .map(|(id, p)| (id.clone(), p.clone()))
                .collect();
            for (id, peer) in peers {
                self.request_audio(&id, &peer);
            }
            if value == &Value::Bool(false) {
                self.stop_audio(None);
            }
        }
        result
    }

    fn apply_setting(&self, key: &str, value: &Value) -> Result<(), String> {
        let mut result = Ok(());
        self.update_config(|c| match (key, value) {
            ("clipboard", Value::Bool(b)) => c.settings.clipboard = *b,
            ("audio", Value::Bool(b)) => c.settings.audio = *b,
            ("updates", Value::Bool(b)) => c.settings.updates = *b,
            _ => result = Err(format!("unknown setting {key:?}")),
        });
        result
    }

    /// Someone is using another computer through this one, or this one from another.
    pub fn in_use(&self) -> bool {
        self.controller.lock().unwrap().active_peer().is_some()
            || self
                .target
                .get()
                .is_some_and(|t| t.lock().unwrap().active.is_some())
    }

    pub fn release(&self) {
        let actions = self.controller.lock().unwrap().release();
        self.apply_actions(actions);
    }
}

impl Target {
    fn map(&mut self, code: u16, down: bool) -> Vec<(u16, bool)> {
        if self.remap_active {
            self.remap.map(code, down)
        } else {
            vec![(code, down)]
        }
    }

    fn leave(&mut self) {
        for (code, down) in self.remap.reset() {
            self.emulator.button_or_key(code, down);
        }
        self.emulator.release_all();
        self.active = None;
    }
}

trait ButtonOrKey {
    fn button_or_key(&self, code: u16, down: bool);
}

impl ButtonOrKey for platform::Emulator {
    fn button_or_key(&self, code: u16, down: bool) {
        if mousetail_core::keys::ev::is_button(code) {
            self.button(code, down);
        } else {
            self.key(code, down);
        }
    }
}

/// Encode whatever plays into the virtual speaker and send it, 10 ms per packet. Silence
/// isn't sent (the receiver notices the gap and re-buffers when sound resumes). Ends when the
/// speaker is removed or the connection closes.
fn stream_audio(pcm: std::sync::mpsc::Receiver<Vec<i16>>, conn: Connection) {
    const FRAME_SAMPLES: usize = audio::FRAME * audio::CHANNELS;
    const SILENCE_FRAMES: u32 = 50;
    let Ok(mut encoder) = audio::Encoder::new() else {
        warn!("couldn't start the audio encoder");
        return;
    };
    let stream = rand_u32();
    let mut seq = 0u32;
    let mut pending: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let mut quiet = 0u32;
    while let Ok(chunk) = pcm.recv() {
        pending.extend(chunk);
        while pending.len() >= FRAME_SAMPLES {
            let frame: Vec<i16> = pending.drain(..FRAME_SAMPLES).collect();
            quiet = if frame.iter().all(|s| s.unsigned_abs() < 8) {
                quiet + 1
            } else {
                0
            };
            if quiet <= SILENCE_FRAMES
                && let Ok(data) = encoder.encode(&frame)
            {
                let packet = Datagram::Audio(AudioPacket { stream, seq, data });
                if conn.send_datagram(proto::encode(&packet).into()).is_err()
                    && conn.close_reason().is_some()
                {
                    return;
                }
            }
            seq = seq.wrapping_add(1);
        }
    }
}

fn rand_u32() -> u32 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(Instant::now()) as u32
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn clipboard_hash(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
