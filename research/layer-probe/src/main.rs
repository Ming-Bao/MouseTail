//! Spike: does an overlay layer-shell strip at the right screen edge get pointer events?
//! Prints every pointer event for 12 seconds.

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
fn main() {
    probe::run();
}

#[cfg(target_os = "linux")]
mod probe {
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
    use std::time::{Duration, Instant};

    use wayland_client::protocol::*;
    use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
        Layer, ZwlrLayerShellV1,
    };
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
        Anchor, Event, ZwlrLayerSurfaceV1,
    };

    #[derive(Default)]
    struct S {
        comp: Option<wl_compositor::WlCompositor>,
        shm: Option<wl_shm::WlShm>,
        seat: Option<wl_seat::WlSeat>,
        ls: Option<ZwlrLayerShellV1>,
        surface: Option<wl_surface::WlSurface>,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for S {
        fn event(
            s: &mut Self,
            r: &wl_registry::WlRegistry,
            e: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = e
            {
                match interface.as_str() {
                    "wl_compositor" => s.comp = Some(r.bind(name, version.min(4), qh, ())),
                    "wl_shm" => s.shm = Some(r.bind(name, 1, qh, ())),
                    "wl_seat" => s.seat = Some(r.bind(name, version.min(7), qh, ())),
                    "zwlr_layer_shell_v1" => s.ls = Some(r.bind(name, version.min(4), qh, ())),
                    _ => {}
                }
            }
        }
    }
    impl Dispatch<wl_seat::WlSeat, ()> for S {
        fn event(
            _: &mut Self,
            seat: &wl_seat::WlSeat,
            e: wl_seat::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_seat::Event::Capabilities { capabilities } = e {
                println!("caps {capabilities:?}");
                seat.get_pointer(qh, ());
            }
        }
    }
    impl Dispatch<wl_pointer::WlPointer, ()> for S {
        fn event(
            _: &mut Self,
            _: &wl_pointer::WlPointer,
            e: wl_pointer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            println!("pointer {e:?}");
        }
    }
    impl Dispatch<ZwlrLayerSurfaceV1, ()> for S {
        fn event(
            s: &mut Self,
            l: &ZwlrLayerSurfaceV1,
            e: Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let Event::Configure {
                serial,
                width,
                height,
            } = e
            {
                println!("configure {width}x{height}");
                l.ack_configure(serial);
                let size = (width * height * 4) as usize;
                let fd = unsafe { OwnedFd::from_raw_fd(libc::memfd_create(c"probe".as_ptr(), 0)) };
                unsafe { libc::ftruncate(fd.as_raw_fd(), size as i64) };
                let px: Vec<u8> = std::iter::repeat_n([0u8, 0, 255, 128], size / 4)
                    .flatten()
                    .collect();
                unsafe { libc::write(fd.as_raw_fd(), px.as_ptr().cast(), px.len()) };
                let pool = s
                    .shm
                    .as_ref()
                    .unwrap()
                    .create_pool(fd.as_fd(), size as i32, qh, ());
                let buf = pool.create_buffer(
                    0,
                    width as i32,
                    height as i32,
                    width as i32 * 4,
                    wl_shm::Format::Argb8888,
                    qh,
                    (),
                );
                let surf = s.surface.as_ref().unwrap();
                surf.attach(Some(&buf), 0, 0);
                surf.damage_buffer(0, 0, width as i32, height as i32);
                surf.commit();
            }
        }
    }
    delegate_noop!(S: ignore wl_compositor::WlCompositor);
    delegate_noop!(S: ignore wl_shm::WlShm);
    delegate_noop!(S: ignore wl_shm_pool::WlShmPool);
    delegate_noop!(S: ignore wl_buffer::WlBuffer);
    delegate_noop!(S: ignore wl_surface::WlSurface);
    delegate_noop!(S: ignore ZwlrLayerShellV1);

    pub fn run() {
        let conn = Connection::connect_to_env().unwrap();
        let mut q = conn.new_event_queue();
        let qh = q.handle();
        conn.display().get_registry(&qh, ());
        let mut s = S::default();
        q.roundtrip(&mut s).unwrap();
        let surf = s.comp.as_ref().unwrap().create_surface(&qh, ());
        let layer = s.ls.as_ref().unwrap().get_layer_surface(
            &surf,
            None,
            Layer::Overlay,
            "probe".into(),
            &qh,
            (),
        );
        layer.set_anchor(Anchor::Right | Anchor::Top | Anchor::Bottom);
        layer.set_size(60, 0);
        layer.set_exclusive_zone(-1);
        surf.commit();
        s.surface = Some(surf);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(12) {
            q.blocking_dispatch(&mut s).unwrap();
        }
    }
}
