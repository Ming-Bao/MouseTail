//! The ripple where the cursor crosses (see `mousetail_core::ripple`), on compositors with
//! `wlr-layer-shell`. While a ripple plays, a click-through surface covers that monitor on
//! the overlay layer and we draw into it on the CPU, repainting only the square the waves
//! have reached. Frames follow the compositor's frame callbacks; nothing runs in between.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use mousetail_core::layout::{Point, Rect};
use mousetail_core::ripple::{self, Ripple};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_output, wl_region, wl_registry, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

pub struct Ripples {
    tx: mpsc::Sender<(Point, f32, Instant)>,
    wake: OwnedFd,
}

impl Ripples {
    pub fn start() -> anyhow::Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
        let (painter, queue) = Painter::connect(&conn)?;
        let (tx, rx) = mpsc::channel();
        let mut fds = [0; 2];
        anyhow::ensure!(
            unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == 0,
            "pipe: {}",
            std::io::Error::last_os_error()
        );
        let (wake_rx, wake_tx) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        thread::Builder::new()
            .name("ripple".into())
            .spawn(move || {
                let mut next = Some((painter, queue));
                loop {
                    if let Some((painter, queue)) = next.take() {
                        match painter.run(queue, &rx, &wake_rx) {
                            Ok(()) => return, // `Ripples` is gone
                            Err(e) => tracing::warn!("crossing ripple stopped: {e:#}"),
                        }
                    }
                    // Only for show, so never worth giving up on: start again (the compositor
                    // may have restarted).
                    thread::sleep(Duration::from_secs(2));
                    next = Connection::connect_to_env()
                        .map_err(anyhow::Error::from)
                        .and_then(|conn| Painter::connect(&conn))
                        .inspect_err(|e| tracing::debug!("crossing ripple can't restart: {e:#}"))
                        .ok();
                    // Ripples asked for meanwhile are stale by now.
                    loop {
                        match rx.try_recv() {
                            Ok(_) => {}
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => return,
                        }
                    }
                }
            })?;
        Ok(Self { tx, wake: wake_tx })
    }

    /// Ripple at `at`, in logical (compositor) coordinates.
    pub fn show(&self, at: Point, strength: f32) {
        if self.tx.send((at, strength, Instant::now())).is_ok() {
            unsafe { libc::write(self.wake.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        }
    }
}

/// A monitor: its registry name, and where it is once xdg-output says.
struct Output {
    global: u32,
    wl: wl_output::WlOutput,
    rect: Option<Rect>,
    scale: i32,
}

/// A monitor's surface while it's rippling.
struct Overlay {
    output: u32,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    ripples: Vec<Ripple>,
    scale: i32,
    /// Set once the compositor has sized the surface.
    canvas: Option<Canvas>,
    /// Waiting for the compositor to ask for the next frame.
    frame_pending: bool,
    /// What the last committed frame painted, in buffer pixels, so the next can damage it.
    shown: Option<Area>,
}

/// Two shared-memory buffers, drawn into alternately (the compositor may still be reading the
/// other).
struct Canvas {
    width: usize,
    height: usize,
    map: *mut u32,
    len: usize,
    slots: [Slot; 2],
}

struct Slot {
    buffer: wl_buffer::WlBuffer,
    busy: bool,
    /// What's painted in it, to clear before painting again.
    painted: Option<Area>,
}

/// A rectangle of buffer pixels: x0..x1, y0..y1.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Area {
    fn union(self, o: Area) -> Area {
        Area {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }
}

// The thread only ever touches the mapping it made.
unsafe impl Send for Canvas {}

impl Drop for Canvas {
    fn drop(&mut self) {
        for s in &self.slots {
            s.buffer.destroy();
        }
        unsafe { libc::munmap(self.map.cast(), self.len * 4) };
    }
}

struct Painter {
    qh: QueueHandle<Painter>,
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    layer_shell: ZwlrLayerShellV1,
    xdg_outputs: ZxdgOutputManagerV1,
    outputs: Vec<Output>,
    overlays: Vec<Overlay>,
    /// Premultiplied ARGB for each step of summed wave height (see `shade_table`).
    shades: Vec<u32>,
}

/// Globals gathered during setup.
#[derive(Default)]
struct Globals {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    xdg_outputs: Option<ZxdgOutputManagerV1>,
}

impl Painter {
    fn connect(conn: &Connection) -> anyhow::Result<(Self, EventQueue<Painter>)> {
        let mut gq = conn.new_event_queue::<Globals>();
        conn.display().get_registry(&gq.handle(), ());
        let mut g = Globals::default();
        gq.roundtrip(&mut g)?;
        let queue = conn.new_event_queue::<Painter>();
        let qh = queue.handle();
        let painter = Painter {
            compositor: g.compositor.context("no wl_compositor")?,
            shm: g.shm.context("no wl_shm")?,
            layer_shell: g.layer_shell.context("no wlr-layer-shell")?,
            xdg_outputs: g.xdg_outputs.context("no xdg-output")?,
            outputs: vec![],
            overlays: vec![],
            shades: shade_table(),
            qh: qh.clone(),
        };
        // Monitors, now and as they come and go, on our own queue.
        conn.display().get_registry(&qh, ());
        Ok((painter, queue))
    }

    fn run(
        mut self,
        mut queue: EventQueue<Painter>,
        rx: &mpsc::Receiver<(Point, f32, Instant)>,
        wake: &OwnedFd,
    ) -> anyhow::Result<()> {
        // Monitors, then where each one is (asked for as each is announced).
        queue.roundtrip(&mut self)?;
        queue.roundtrip(&mut self)?;
        loop {
            flush(&queue)?;
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
            // While rippling, look in now and then even without frame callbacks (a monitor
            // that's asleep sends none), so finished ripples are cleared away.
            let timeout = if self.overlays.is_empty() { -1 } else { 100 };
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
            }
            if fds[0].revents & libc::POLLIN != 0 {
                match guard.read() {
                    Ok(_) => {}
                    // Nothing there after all.
                    Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                }
            } else {
                drop(guard);
            }
            if fds[1].revents & libc::POLLHUP != 0 {
                return Ok(()); // `Ripples` is gone
            }
            if fds[1].revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 64];
                while unsafe { libc::read(wake.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) }
                    > 0
                {}
            }
            while let Ok((at, strength, started)) = rx.try_recv() {
                self.add(at, strength, started);
            }
            queue.dispatch_pending(&mut self)?;
            self.tidy();
        }
    }

    fn add(&mut self, at: Point, strength: f32, started: Instant) {
        // The monitor it's on, looking just inside it either way: a right or bottom edge
        // isn't within its own monitor's rect.
        let nudges = [(0.0, 0.0), (-0.5, 0.0), (0.5, 0.0), (0.0, -0.5), (0.0, 0.5)];
        let Some((global, rect)) = nudges.iter().find_map(|(dx, dy)| {
            let p = Point::new(at.x + dx, at.y + dy);
            self.outputs
                .iter()
                .find_map(|o| o.rect.filter(|r| r.contains(p)).map(|r| (o.global, r)))
        }) else {
            tracing::debug!("no monitor at {at:?} for the crossing ripple");
            return;
        };
        let ripple = Ripple {
            origin: at.minus(Point::new(rect.x, rect.y)),
            started,
            strength,
        };
        if let Some(o) = self.overlays.iter_mut().find(|o| o.output == global) {
            ripple::push(&mut o.ripples, ripple);
            return;
        }
        let Some(output) = self.outputs.iter().find(|o| o.global == global) else {
            return;
        };
        let surface = self.compositor.create_surface(&self.qh, ());
        // Clicks go straight through.
        let region = self.compositor.create_region(&self.qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        surface.set_buffer_scale(output.scale);
        let layer = self.layer_shell.get_layer_surface(
            &surface,
            Some(&output.wl),
            Layer::Overlay,
            "mousetail-ripple".into(),
            &self.qh,
            global,
        );
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        surface.commit();
        self.overlays.push(Overlay {
            output: global,
            surface,
            layer,
            ripples: vec![ripple],
            scale: output.scale,
            canvas: None,
            frame_pending: false,
            shown: None,
        });
    }

    /// Take down overlays whose ripples have all finished.
    fn tidy(&mut self) {
        let now = Instant::now();
        self.overlays.retain_mut(|o| {
            o.ripples.retain(|r| !r.done(now));
            if o.ripples.is_empty() {
                o.canvas = None;
                o.layer.destroy();
                o.surface.destroy();
                return false;
            }
            true
        });
    }

    fn configured(&mut self, output: u32, width: u32, height: u32) {
        let Some(o) = self.overlays.iter_mut().find(|o| o.output == output) else {
            return;
        };
        let scale = o.scale.max(1) as usize;
        let (w, h) = (width as usize * scale, height as usize * scale);
        if w == 0
            || h == 0
            || o.canvas
                .as_ref()
                .is_some_and(|c| (c.width, c.height) == (w, h))
        {
            return;
        }
        o.canvas = None;
        o.shown = None;
        match Canvas::new(&self.shm, &self.qh, output, w, h) {
            Ok(c) => o.canvas = Some(c),
            Err(e) => {
                tracing::debug!("no canvas for the crossing ripple: {e:#}");
                return;
            }
        }
        self.draw(output);
    }

    /// Paint and commit the next frame for `output`'s overlay, if a buffer is free.
    fn draw(&mut self, output: u32) {
        let now = Instant::now();
        let Some(o) = self.overlays.iter_mut().find(|o| o.output == output) else {
            return;
        };
        o.ripples.retain(|r| !r.done(now));
        let scale = o.scale.max(1) as f64;
        let Some(canvas) = &mut o.canvas else { return };
        let Some(index) = canvas.slots.iter().position(|s| !s.busy) else {
            return;
        };
        let reach = o
            .ripples
            .iter()
            .filter_map(|r| {
                let radius = (ripple::SPEED * r.age(now) + 2.0 * ripple::WAVELENGTH)
                    .min(ripple::REACH) as f64
                    * scale;
                let (cx, cy) = (r.origin.x * scale, r.origin.y * scale);
                let clip = |v: f64, max: usize| v.clamp(0.0, max as f64) as usize;
                let a = Area {
                    x0: clip(cx - radius, canvas.width),
                    y0: clip(cy - radius, canvas.height),
                    x1: clip(cx + radius, canvas.width),
                    y1: clip(cy + radius, canvas.height),
                };
                (a.x1 > a.x0 && a.y1 > a.y0).then_some(a)
            })
            .reduce(Area::union);
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(
                canvas.map.add(index * canvas.width * canvas.height),
                canvas.width * canvas.height,
            )
        };
        let slot = &mut canvas.slots[index];
        if let Some(old) = slot.painted.take() {
            fill(pixels, canvas.width, old, 0);
        }
        if let Some(area) = reach {
            paint(
                pixels,
                canvas.width,
                area,
                scale,
                &o.ripples,
                now,
                &self.shades,
            );
        }
        slot.painted = reach;
        slot.busy = true;
        o.surface.attach(Some(&slot.buffer), 0, 0);
        if let Some(d) = [o.shown, reach].into_iter().flatten().reduce(Area::union) {
            o.surface.damage_buffer(
                d.x0 as i32,
                d.y0 as i32,
                (d.x1 - d.x0) as i32,
                (d.y1 - d.y0) as i32,
            );
        }
        o.shown = reach;
        if !o.ripples.is_empty() {
            o.surface.frame(&self.qh, output);
            o.frame_pending = true;
        }
        o.surface.commit();
    }
}

/// Write out pending requests, waiting briefly if the socket is momentarily full.
fn flush(queue: &EventQueue<Painter>) -> anyhow::Result<()> {
    for _ in 0..500 {
        match queue.flush() {
            Ok(()) => return Ok(()),
            Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("compositor stopped reading")
}

impl Canvas {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<Painter>,
        output: u32,
        width: usize,
        height: usize,
    ) -> anyhow::Result<Self> {
        let len = width * height * 2;
        let bytes = len * 4;
        let fd = unsafe { libc::memfd_create(c"mousetail-ripple".as_ptr(), libc::MFD_CLOEXEC) };
        anyhow::ensure!(fd >= 0, "memfd: {}", std::io::Error::last_os_error());
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        anyhow::ensure!(
            unsafe { libc::ftruncate(fd.as_raw_fd(), bytes as libc::off_t) } == 0,
            "sizing the canvas: {}",
            std::io::Error::last_os_error()
        );
        // A fresh memfd reads as zeros: fully transparent.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        anyhow::ensure!(
            map != libc::MAP_FAILED,
            "mapping the canvas: {}",
            std::io::Error::last_os_error()
        );
        let pool = shm.create_pool(fd.as_fd(), bytes as i32, qh, ());
        let slot = |i: usize| Slot {
            buffer: pool.create_buffer(
                (i * width * height * 4) as i32,
                width as i32,
                height as i32,
                (width * 4) as i32,
                wl_shm::Format::Argb8888,
                qh,
                (output, i),
            ),
            busy: false,
            painted: None,
        };
        let slots = [slot(0), slot(1)];
        pool.destroy();
        Ok(Self {
            width,
            height,
            map: map.cast(),
            len,
            slots,
        })
    }
}

/// Steps of summed height per unit, in the shade table.
const SHADE_STEPS: f32 = 256.0;

/// Premultiplied ARGB for each summed height from -1/GAIN to 1/GAIN (beyond which shading
/// saturates), so painting a pixel is a lookup rather than a `powf`.
fn shade_table() -> Vec<u32> {
    let steps = SHADE_STEPS as i32;
    (-steps..=steps)
        .map(|i| {
            let (white, alpha) = ripple::shade(i as f32 / SHADE_STEPS / ripple::GAIN);
            let (w, a) = (
                (white * 255.0).round() as u32,
                (alpha * 255.0).round() as u32,
            );
            a << 24 | w << 16 | w << 8 | w
        })
        .collect()
}

/// Heights along each ripple's radius every half point, looked up per pixel.
const PER_POINT: f32 = 2.0;

/// One ripple, ready to paint.
struct Wave {
    /// Where it started, in points.
    x: f32,
    y: f32,
    /// Within this far (points) it's flat.
    calm: f32,
    /// Its height every `1 / PER_POINT` points out from the middle.
    profile: Vec<f32>,
}

/// Paint `area` (buffer pixels) with the ripples' summed waves, its rows shared out across
/// the processor's threads.
fn paint(
    pixels: &mut [u32],
    stride: usize,
    area: Area,
    scale: f64,
    ripples: &[Ripple],
    now: Instant,
    shades: &[u32],
) {
    let steps = (ripple::REACH * PER_POINT) as usize + 2;
    let waves: Vec<Wave> = ripples
        .iter()
        .map(|r| {
            let age = r.age(now);
            Wave {
                x: r.origin.x as f32,
                y: r.origin.y as f32,
                calm: ripple::calm_within(age),
                profile: (0..steps)
                    .map(|i| ripple::height(i as f32 / PER_POINT, age, r.strength))
                    .collect(),
            }
        })
        .collect();
    let threads = thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let band = (area.y1 - area.y0).div_ceil(threads).max(1);
    let rows = &mut pixels[area.y0 * stride..area.y1 * stride];
    thread::scope(|s| {
        for (i, chunk) in rows.chunks_mut(band * stride).enumerate() {
            let waves = &waves;
            let top = area.y0 + i * band;
            s.spawn(move || {
                for (j, row) in chunk.chunks_mut(stride).enumerate() {
                    paint_row(
                        &mut row[area.x0..area.x1],
                        area.x0,
                        top + j,
                        scale as f32,
                        waves,
                        shades,
                    );
                }
            });
        }
    });
}

/// Paint one row of pixels, `x0` being the first one's column.
fn paint_row(row: &mut [u32], x0: usize, y: usize, scale: f32, waves: &[Wave], shades: &[u32]) {
    let py = (y as f32 + 0.5) / scale;
    // Where this row crosses every ripple's calm middle it stays clear: skip those pixels.
    let mut calm = (f32::NEG_INFINITY, f32::INFINITY);
    for w in waves {
        let dy = (py - w.y).abs();
        let half = if dy < w.calm {
            (w.calm * w.calm - dy * dy).sqrt()
        } else {
            f32::NEG_INFINITY
        };
        calm = (calm.0.max(w.x - half), calm.1.min(w.x + half));
    }
    // In this row's pixels, a pixel short each side to be sure.
    let skip = if calm.0 < calm.1 {
        let column = |x: f32| (x - x0 as f32).max(0.0) as usize;
        column((calm.0 * scale).ceil() + 1.0)..column((calm.1 * scale).floor() - 1.0)
    } else {
        0..0
    };
    let middle = SHADE_STEPS as i32;
    for (i, px) in row.iter_mut().enumerate() {
        if skip.contains(&i) {
            *px = 0;
            continue;
        }
        let pxx = ((x0 + i) as f32 + 0.5) / scale;
        let mut h = 0.0;
        for w in waves {
            let d = ((pxx - w.x).powi(2) + (py - w.y).powi(2)).sqrt();
            h += w
                .profile
                .get((d * PER_POINT) as usize)
                .copied()
                .unwrap_or(0.0);
        }
        let step = ((h * ripple::GAIN * SHADE_STEPS) as i32).clamp(-middle, middle);
        *px = shades[(step + middle) as usize];
    }
}

fn fill(pixels: &mut [u32], stride: usize, area: Area, value: u32) {
    for y in area.y0..area.y1 {
        pixels[y * stride + area.x0..y * stride + area.x1].fill(value);
    }
}

// ------------------------------------------------------------------------- Wayland events

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
                "zwlr_layer_shell_v1" => {
                    g.layer_shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "zxdg_output_manager_v1" => {
                    g.xdg_outputs = Some(registry.bind(name, version.min(3), qh, ()))
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(Globals: ignore wl_compositor::WlCompositor);
delegate_noop!(Globals: ignore wl_shm::WlShm);
delegate_noop!(Globals: ignore ZwlrLayerShellV1);
delegate_noop!(Globals: ignore ZxdgOutputManagerV1);

impl Dispatch<wl_registry::WlRegistry, ()> for Painter {
    fn event(
        p: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" => {
                let wl: wl_output::WlOutput = registry.bind(name, version.min(2), qh, name);
                p.xdg_outputs.get_xdg_output(&wl, qh, name);
                p.outputs.push(Output {
                    global: name,
                    wl,
                    rect: None,
                    scale: 1,
                });
            }
            wl_registry::Event::GlobalRemove { name } => {
                p.outputs.retain(|o| o.global != name);
                p.overlays.retain_mut(|o| {
                    if o.output != name {
                        return true;
                    }
                    o.canvas = None;
                    o.layer.destroy();
                    o.surface.destroy();
                    false
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for Painter {
    fn event(
        p: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Scale { factor } = event
            && let Some(o) = p.outputs.iter_mut().find(|o| o.global == *global)
        {
            o.scale = factor.max(1);
        }
    }
}

impl Dispatch<ZxdgOutputV1, u32> for Painter {
    fn event(
        p: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = p.outputs.iter_mut().find(|o| o.global == *global) else {
            return;
        };
        let mut rect = o.rect.unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                (rect.x, rect.y) = (x as f64, y as f64)
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                (rect.w, rect.h) = (width as f64, height as f64)
            }
            _ => return,
        }
        o.rect = Some(rect);
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, u32> for Painter {
    fn event(
        p: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        output: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                p.configured(*output, width, height);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                if let Some(o) = p.overlays.iter_mut().find(|o| o.output == *output) {
                    o.ripples.clear();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for Painter {
    fn event(
        p: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        output: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            if let Some(o) = p.overlays.iter_mut().find(|o| o.output == *output) {
                o.frame_pending = false;
            }
            p.draw(*output);
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, (u32, usize)> for Painter {
    fn event(
        p: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        (output, index): &(u32, usize),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event
            && let Some(o) = p.overlays.iter_mut().find(|o| o.output == *output)
            && let Some(c) = &mut o.canvas
        {
            c.slots[*index].busy = false;
            // Both were busy when the compositor asked for a frame: draw it now.
            if !o.frame_pending && !o.ripples.is_empty() {
                p.draw(*output);
            }
        }
    }
}

delegate_noop!(Painter: ignore wl_compositor::WlCompositor);
delegate_noop!(Painter: ignore wl_shm::WlShm);
delegate_noop!(Painter: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Painter: ignore wl_surface::WlSurface);
delegate_noop!(Painter: ignore wl_region::WlRegion);
delegate_noop!(Painter: ignore ZwlrLayerShellV1);
delegate_noop!(Painter: ignore ZxdgOutputManagerV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shade_table_is_clear_in_the_middle() {
        let t = shade_table();
        assert_eq!(t.len(), 2 * SHADE_STEPS as usize + 1);
        assert_eq!(t[SHADE_STEPS as usize], 0);
        // Crest: white at full highlight; trough: dark.
        assert_eq!(
            t[t.len() - 1] >> 24,
            (ripple::HIGHLIGHT * 255.0).round() as u32
        );
        assert_eq!(t[0] & 0xFF_FFFF, 0);
    }

    #[test]
    fn paints_only_its_area() {
        let (w, h) = (64, 64);
        let mut pixels = vec![0u32; w * h];
        let now = Instant::now();
        let started = now - std::time::Duration::from_millis(50);
        let ripples = [Ripple {
            origin: Point::new(0.0, 32.0),
            started,
            strength: 1.0,
        }];
        let area = Area {
            x0: 0,
            y0: 0,
            x1: 32,
            y1: 64,
        };
        paint(&mut pixels, w, area, 1.0, &ripples, now, &shade_table());
        assert!(
            pixels
                .iter()
                .enumerate()
                .any(|(i, p)| i % w < 32 && *p != 0)
        );
        assert!(
            pixels
                .iter()
                .enumerate()
                .all(|(i, p)| i % w < 32 || *p == 0)
        );
    }
}
