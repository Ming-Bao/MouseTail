//! Linux as the main computer, on compositors with `wlr-layer-shell` (Hyprland, Sway, KDE,
//! river, niri…).
//!
//! Where an edge leads to another computer we lay a one-pixel invisible strip along it, on
//! the overlay layer. While the pointer rests on a strip, the compositor sends us its raw
//! relative motion even when the cursor can't move any further, which is exactly "pushing
//! against the edge". That feeds the same `Controller` the Mac uses; when it decides to
//! cross we lock the pointer to the strip, take keyboard focus and inhibit compositor
//! shortcuts, and forward everything until it brings the cursor home.

use std::collections::HashSet;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use anyhow::Context;
use mousetail_core::controller::{Action, Controller, Input};
use mousetail_core::keys::ev;
use mousetail_core::layout::{Point, Side};
use mousetail_core::proto::Scroll;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};
use wayland_client::protocol::wl_pointer::{self, ButtonState};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_region, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::ZwpLockedPointerV1,
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use crate::platform::Edge;

enum Cmd {
    Apply(Action),
    Edges(Vec<Edge>),
}

pub struct Capture {
    tx: mpsc::Sender<Cmd>,
    wake: OwnedFd,
}

impl Capture {
    pub fn supported() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    pub fn start(
        controller: Arc<Mutex<Controller>>,
        actions: UnboundedSender<Action>,
        _prompt: bool,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let mut fds = [0; 2];
        anyhow::ensure!(
            unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == 0,
            "pipe: {}",
            std::io::Error::last_os_error()
        );
        let (wake_rx, wake_tx) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::Builder::new()
            .name("capture".into())
            .spawn(move || match Grabber::connect(controller, actions) {
                Ok((grabber, queue)) => {
                    let _ = ready_tx.send(Ok(()));
                    if let Err(e) = grabber.run(queue, rx, wake_rx) {
                        warn!("input capture stopped: {e:#}");
                    }
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })?;
        ready_rx.recv().context("capture thread died")??;
        Ok(Self { tx, wake: wake_tx })
    }

    fn send(&self, cmd: Cmd) {
        if self.tx.send(cmd).is_ok() {
            unsafe { libc::write(self.wake.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        }
    }

    pub fn apply(&self, action: &Action) {
        self.send(Cmd::Apply(action.clone()));
    }

    /// Which edges lead somewhere (changes with the arrangement and the displays).
    pub fn set_edges(&self, edges: Vec<Edge>) {
        self.send(Cmd::Edges(edges));
    }
}

#[derive(Clone, Default)]
struct OutputRec {
    name: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

struct Zone {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    edge: Edge,
    /// Global position of the surface's top-left corner.
    origin: Point,
    buffer: Option<wl_buffer::WlBuffer>,
}

struct Grab {
    zone: usize,
    locked: ZwpLockedPointerV1,
    inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
}

#[derive(Default)]
struct PendingScroll {
    dx: f64,
    dy: f64,
    v120: (i32, i32),
    any: bool,
}

struct Grabber {
    qh: QueueHandle<Grabber>,
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    seat: wl_seat::WlSeat,
    layer_shell: ZwlrLayerShellV1,
    constraints: ZwpPointerConstraintsV1,
    relative: ZwpRelativePointerManagerV1,
    inhibit: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    outputs: Vec<(wl_output::WlOutput, OutputRec)>,
    pointer: Option<wl_pointer::WlPointer>,
    relative_pointer: Option<ZwpRelativePointerV1>,
    zones: Vec<Zone>,
    hovered: Option<usize>,
    enter_serial: u32,
    pos: Point,
    buttons: HashSet<u16>,
    grab: Option<Grab>,
    scroll: PendingScroll,
    controller: Arc<Mutex<Controller>>,
    actions: UnboundedSender<Action>,
}

/// Globals gathered during setup.
#[derive(Default)]
struct Globals {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    layer_shell: Option<ZwlrLayerShellV1>,
    constraints: Option<ZwpPointerConstraintsV1>,
    relative: Option<ZwpRelativePointerManagerV1>,
    inhibit: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    xdg_outputs: Option<ZxdgOutputManagerV1>,
    outputs: Vec<(wl_output::WlOutput, OutputRec)>,
}

impl Grabber {
    fn connect(
        controller: Arc<Mutex<Controller>>,
        actions: UnboundedSender<Action>,
    ) -> anyhow::Result<(Self, EventQueue<Grabber>)> {
        let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
        // Gather globals on a throwaway queue, then build the real state.
        let mut gq = conn.new_event_queue::<Globals>();
        let gqh = gq.handle();
        conn.display().get_registry(&gqh, ());
        let mut g = Globals::default();
        gq.roundtrip(&mut g)?;
        let need = |name: &str| {
            format!("this desktop doesn't support {name}, so it can't be the main computer yet")
        };
        let compositor = g.compositor.clone().context("no wl_compositor")?;
        let shm = g.shm.clone().context("no wl_shm")?;
        let seat = g.seat.clone().context("no wl_seat")?;
        let layer_shell = g
            .layer_shell
            .clone()
            .with_context(|| need("wlr-layer-shell"))?;
        let constraints = g
            .constraints
            .clone()
            .with_context(|| need("pointer constraints"))?;
        let relative = g
            .relative
            .clone()
            .with_context(|| need("relative pointer motion"))?;
        if let Some(m) = &g.xdg_outputs {
            for (i, (o, _)) in g.outputs.iter().enumerate() {
                m.get_xdg_output(o, &gqh, i);
            }
            gq.roundtrip(&mut g)?;
        }

        let queue = conn.new_event_queue::<Grabber>();
        let qh = queue.handle();
        // Re-bind the seat on our queue so its events (pointer, keyboard) come to us.
        let registry = conn.display().get_registry(&qh, ());
        let _ = registry;
        let grabber = Grabber {
            qh: qh.clone(),
            compositor,
            shm,
            seat: seat.clone(),
            layer_shell,
            constraints,
            relative,
            inhibit: g.inhibit.clone(),
            outputs: g.outputs.clone(),
            pointer: None,
            relative_pointer: None,
            zones: vec![],
            hovered: None,
            enter_serial: 0,
            pos: Point::default(),
            buttons: HashSet::new(),
            grab: None,
            scroll: PendingScroll::default(),
            controller,
            actions,
        };
        Ok((grabber, queue))
    }

    fn run(
        mut self,
        mut queue: EventQueue<Grabber>,
        rx: mpsc::Receiver<Cmd>,
        wake: OwnedFd,
    ) -> anyhow::Result<()> {
        queue.roundtrip(&mut self)?;
        loop {
            queue.flush()?;
            let guard = loop {
                queue.dispatch_pending(&mut self)?;
                if let Some(g) = queue.prepare_read() {
                    break g;
                }
            };
            let mut fds = [
                libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
            }
            if fds[0].revents & libc::POLLIN != 0 {
                guard.read()?;
            } else {
                drop(guard);
            }
            if fds[1].revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 64];
                while unsafe { libc::read(wake.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) }
                    > 0
                {}
            }
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    Cmd::Apply(a) => self.do_action(&a),
                    Cmd::Edges(e) => self.set_edges(e),
                }
            }
            queue.dispatch_pending(&mut self)?;
        }
    }

    // ---------------------------------------------------------------------- edge strips

    fn set_edges(&mut self, edges: Vec<Edge>) {
        let current: Vec<Edge> = self.zones.iter().map(|z| z.edge.clone()).collect();
        if current == edges {
            return;
        }
        if self.grab.is_some() {
            // Don't pull the rug out mid-grab; the node re-sends edges often enough.
            return;
        }
        for z in self.zones.drain(..) {
            z.layer.destroy();
            z.surface.destroy();
            if let Some(b) = z.buffer {
                b.destroy();
            }
        }
        self.hovered = None;
        for edge in edges {
            let Some((wl, out)) = self
                .outputs
                .iter()
                .find(|(_, o)| o.name == edge.display)
                .cloned()
            else {
                debug!("no output called {} for an edge strip", edge.display);
                continue;
            };
            let surface = self.compositor.create_surface(&self.qh, ());
            let index = self.zones.len();
            let layer = self.layer_shell.get_layer_surface(
                &surface,
                Some(&wl),
                Layer::Overlay,
                "mousetail-edge".into(),
                &self.qh,
                index,
            );
            let (anchor, size, origin) = match edge.side {
                Side::Left => (
                    Anchor::Left | Anchor::Top | Anchor::Bottom,
                    (1, 0),
                    (out.x, out.y),
                ),
                Side::Right => (
                    Anchor::Right | Anchor::Top | Anchor::Bottom,
                    (1, 0),
                    (out.x + out.w - 1, out.y),
                ),
                Side::Above => (
                    Anchor::Top | Anchor::Left | Anchor::Right,
                    (0, 1),
                    (out.x, out.y),
                ),
                Side::Below => (
                    Anchor::Bottom | Anchor::Left | Anchor::Right,
                    (0, 1),
                    (out.x, out.y + out.h - 1),
                ),
            };
            layer.set_anchor(anchor);
            layer.set_size(size.0, size.1);
            // -1: span the whole edge, even alongside bars that reserve space.
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            surface.commit();
            self.zones.push(Zone {
                surface,
                layer,
                edge,
                origin: Point::new(origin.0 as f64, origin.1 as f64),
                buffer: None,
            });
        }
    }

    /// A transparent buffer for a strip (layer surfaces need content to be shown).
    fn transparent_buffer(&self, w: i32, h: i32) -> Option<wl_buffer::WlBuffer> {
        let size = (w.max(1) * h.max(1) * 4) as usize;
        let fd = unsafe { libc::memfd_create(c"mousetail-edge".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } != 0 {
            return None;
        }
        // Not quite fully transparent: some compositors skip fully clear surfaces when
        // deciding what's under the pointer. Alpha 1/255 is invisible to the eye.
        let pixels: Vec<u8> = std::iter::repeat_n([0u8, 0, 0, 1], size / 4)
            .flatten()
            .collect();
        unsafe { libc::write(fd.as_raw_fd(), pixels.as_ptr().cast(), pixels.len()) };
        let pool = self.shm.create_pool(fd.as_fd(), size as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            w.max(1),
            h.max(1),
            w.max(1) * 4,
            wl_shm::Format::Argb8888,
            &self.qh,
            (),
        );
        pool.destroy();
        Some(buffer)
    }

    // ------------------------------------------------------------------ controller glue

    fn feed(&mut self, input: Input) {
        let outcome = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .handle(input);
        for action in outcome.actions {
            match action {
                Action::Grab | Action::Release { .. } => self.do_action(&action),
                other => {
                    let _ = self.actions.send(other);
                }
            }
        }
    }

    fn do_action(&mut self, action: &Action) {
        match action {
            Action::Grab => self.grab(),
            Action::Release { warp } => self.release(*warp),
            _ => {}
        }
    }

    fn grab(&mut self) {
        if self.grab.is_some() {
            return;
        }
        let (Some(zone), Some(pointer)) = (self.hovered, self.pointer.clone()) else {
            return;
        };
        let z = &self.zones[zone];
        let locked = self.constraints.lock_pointer(
            &z.surface,
            &pointer,
            None,
            Lifetime::Persistent,
            &self.qh,
            (),
        );
        pointer.set_cursor(self.enter_serial, None, 0, 0);
        z.layer
            .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        let inhibitor = self
            .inhibit
            .as_ref()
            .map(|m| m.inhibit_shortcuts(&z.surface, &self.seat, &self.qh, ()));
        z.surface.commit();
        self.grab = Some(Grab {
            zone,
            locked,
            inhibitor,
        });
    }

    fn release(&mut self, warp: Point) {
        let Some(g) = self.grab.take() else { return };
        let z = &self.zones[g.zone];
        // Ask the compositor to leave the cursor where the controller says it came home.
        g.locked
            .set_cursor_position_hint(warp.x - z.origin.x, warp.y - z.origin.y);
        z.surface.commit();
        g.locked.destroy();
        if let Some(i) = g.inhibitor {
            i.destroy();
        }
        z.layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        z.surface.commit();
        self.pos = warp;
        self.buttons.clear();
    }
}

// ------------------------------------------------------------------------------ globals

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        g: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => g.compositor = Some(registry.bind(name, version.min(4), qh, ())),
                "wl_shm" => g.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" if g.seat.is_none() => {
                    g.seat = Some(registry.bind(name, version.min(8), qh, ()))
                }
                "zwlr_layer_shell_v1" => {
                    g.layer_shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "zwp_pointer_constraints_v1" => {
                    g.constraints = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_relative_pointer_manager_v1" => {
                    g.relative = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                    g.inhibit = Some(registry.bind(name, 1, qh, ()))
                }
                "zxdg_output_manager_v1" => {
                    g.xdg_outputs = Some(registry.bind(name, version.min(3), qh, ()))
                }
                "wl_output" => {
                    let o = registry.bind(name, version.min(4), qh, ());
                    g.outputs.push((o, OutputRec::default()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZxdgOutputV1, usize> for Globals {
    fn event(
        g: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, o)) = g.outputs.get_mut(*index) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => (o.x, o.y) = (x, y),
            zxdg_output_v1::Event::LogicalSize { width, height } => (o.w, o.h) = (width, height),
            zxdg_output_v1::Event::Name { name } => o.name = name,
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Globals {
    fn event(
        g: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some((_, o)) = g.outputs.iter_mut().find(|(w, _)| w == output)
            && o.name.is_empty()
        {
            o.name = name;
        }
    }
}

delegate_noop!(Globals: ignore wl_compositor::WlCompositor);
delegate_noop!(Globals: ignore wl_shm::WlShm);
delegate_noop!(Globals: ignore wl_seat::WlSeat);
delegate_noop!(Globals: ignore ZwlrLayerShellV1);
delegate_noop!(Globals: ignore ZwpPointerConstraintsV1);
delegate_noop!(Globals: ignore ZwpRelativePointerManagerV1);
delegate_noop!(Globals: ignore ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(Globals: ignore ZxdgOutputManagerV1);

// ------------------------------------------------------------------------ live events

impl Dispatch<wl_registry::WlRegistry, ()> for Grabber {
    fn event(
        s: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // Our own seat binding, so pointer and keyboard events arrive on this queue.
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_seat"
            && s.pointer.is_none()
        {
            let seat: wl_seat::WlSeat = registry.bind(name, version.min(8), qh, ());
            tracing::trace!("bound seat for capture");
            s.seat = seat;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Grabber {
    fn event(
        s: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            tracing::trace!("seat capabilities {caps:?}");
            if caps.contains(wl_seat::Capability::Pointer) && s.pointer.is_none() {
                let pointer = seat.get_pointer(qh, ());
                s.relative_pointer = Some(s.relative.get_relative_pointer(&pointer, qh, ()));
                s.pointer = Some(pointer);
            }
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, usize> for Grabber {
    fn event(
        s: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                tracing::trace!("edge strip {index} configured {width}x{height}");
                layer.ack_configure(serial);
                if let Some(z) = s.zones.get(*index)
                    && z.buffer.is_none()
                {
                    let buffer = s.transparent_buffer(width as i32, height as i32);
                    let z = &mut s.zones[*index];
                    if let Some(b) = &buffer {
                        z.surface.attach(Some(b), 0, 0);
                        z.surface.damage_buffer(0, 0, width as i32, height as i32);
                    }
                    // Say explicitly that the whole strip takes pointer input.
                    let region = s.compositor.create_region(&s.qh, ());
                    region.add(0, 0, width.max(1) as i32, height.max(1) as i32);
                    z.surface.set_input_region(Some(&region));
                    region.destroy();
                    z.surface.commit();
                    z.buffer = buffer;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {}
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Grabber {
    fn event(
        s: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                tracing::trace!("pointer entered a surface at {surface_x},{surface_y}");
                s.enter_serial = serial;
                s.hovered = s.zones.iter().position(|z| z.surface == surface);
                if let Some(i) = s.hovered {
                    let o = s.zones[i].origin;
                    s.pos = Point::new(o.x + surface_x, o.y + surface_y);
                }
            }
            wl_pointer::Event::Leave { .. } => {
                if s.grab.is_none() {
                    s.hovered = None;
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                if let Some(i) = s.hovered
                    && s.grab.is_none()
                {
                    let o = s.zones[i].origin;
                    s.pos = Point::new(o.x + surface_x, o.y + surface_y);
                }
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let code = button as u16;
                let down = matches!(state, WEnum::Value(ButtonState::Pressed));
                if down {
                    s.buttons.insert(code);
                } else {
                    s.buttons.remove(&code);
                }
                if s.grab.is_some() {
                    s.feed(Input::Button { code, down });
                }
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                s.scroll.any = true;
                match axis {
                    WEnum::Value(wl_pointer::Axis::VerticalScroll) => s.scroll.dy += value,
                    WEnum::Value(wl_pointer::Axis::HorizontalScroll) => s.scroll.dx += value,
                    _ => {}
                }
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => match axis {
                WEnum::Value(wl_pointer::Axis::VerticalScroll) => s.scroll.v120.1 += value120,
                WEnum::Value(wl_pointer::Axis::HorizontalScroll) => s.scroll.v120.0 += value120,
                _ => {}
            },
            wl_pointer::Event::Frame => {
                let sc = std::mem::take(&mut s.scroll);
                if sc.any && s.grab.is_some() {
                    let notches = (sc.v120 != (0, 0)).then_some((sc.v120.0 / 120, sc.v120.1 / 120));
                    s.feed(Input::Scroll(Scroll {
                        dx: sc.dx,
                        dy: sc.dy,
                        notches,
                    }));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Grabber {
    fn event(
        s: &mut Self,
        _: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, .. } = &event {
            tracing::trace!("relative motion {dx},{dy} hovered={:?}", s.hovered);
        }
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, .. } = event
            && (s.hovered.is_some() || s.grab.is_some())
        {
            let dragging = s.buttons.iter().any(|b| ev::is_button(*b));
            s.feed(Input::Motion {
                at: s.pos,
                dx,
                dy,
                dragging,
            });
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Grabber {
    fn event(
        s: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key { key, state, .. } = event
            && s.grab.is_some()
        {
            let down = matches!(state, WEnum::Value(wl_keyboard::KeyState::Pressed));
            s.feed(Input::Key {
                code: key as u16,
                down,
            });
        }
    }
}

delegate_noop!(Grabber: ignore wl_surface::WlSurface);
delegate_noop!(Grabber: ignore wl_region::WlRegion);
delegate_noop!(Grabber: ignore wl_buffer::WlBuffer);
delegate_noop!(Grabber: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Grabber: ignore ZwpLockedPointerV1);
delegate_noop!(Grabber: ignore ZwpKeyboardShortcutsInhibitorV1);
