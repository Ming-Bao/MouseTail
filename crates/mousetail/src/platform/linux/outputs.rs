//! Display layout on any Wayland compositor, via `xdg-output` (logical positions and sizes,
//! the same coordinate space the pointer lives in).

use mousetail_core::layout::Rect;
use mousetail_core::proto::DisplayInfo;
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};

#[derive(Default, Clone)]
struct Output {
    name: String,
    description: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    scale: i32,
}

#[derive(Default)]
struct State {
    outputs: Vec<(wl_output::WlOutput, Output)>,
    manager: Option<ZxdgOutputManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
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
                "wl_output" => {
                    let o = registry.bind(name, version.min(4), qh, ());
                    state.outputs.push((
                        o,
                        Output {
                            scale: 1,
                            ..Default::default()
                        },
                    ));
                }
                "zxdg_output_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(3), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, o)) = state.outputs.iter_mut().find(|(w, _)| w == output) else {
            return;
        };
        match event {
            wl_output::Event::Scale { factor } => o.scale = factor,
            wl_output::Event::Name { name } if o.name.is_empty() => o.name = name,
            wl_output::Event::Description { description } if o.description.is_empty() => {
                o.description = description
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, o)) = state.outputs.get_mut(*index) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => (o.x, o.y) = (x, y),
            zxdg_output_v1::Event::LogicalSize { width, height } => (o.w, o.h) = (width, height),
            zxdg_output_v1::Event::Name { name } => o.name = name,
            zxdg_output_v1::Event::Description { description } => o.description = description,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore ZxdgOutputManagerV1);

/// The compositor's outputs in logical coordinates, or `None` if it can't tell us.
pub fn list() -> Option<Vec<DisplayInfo>> {
    let conn = Connection::connect_to_env().ok()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).ok()?;
    let manager = state.manager.clone()?;
    for (i, (output, _)) in state.outputs.iter().enumerate() {
        manager.get_xdg_output(output, &qh, i);
    }
    queue.roundtrip(&mut state).ok()?;

    let mut displays: Vec<DisplayInfo> = state
        .outputs
        .iter()
        .map(|(_, o)| o)
        .filter(|o| o.w > 0 && o.h > 0)
        .map(|o| DisplayInfo {
            id: o.name.clone(),
            name: if o.description.is_empty() {
                o.name.clone()
            } else {
                o.description.clone()
            },
            rect: Rect::new(o.x as f64, o.y as f64, o.w as f64, o.h as f64),
            scale: o.scale.max(1) as f64,
            primary: false,
        })
        .collect();
    if displays.is_empty() {
        return None;
    }
    displays.sort_by(|a, b| {
        (a.rect.x, a.rect.y)
            .partial_cmp(&(b.rect.x, b.rect.y))
            .unwrap()
    });
    let primary = displays
        .iter()
        .position(|d| d.rect.x == 0.0 && d.rect.y == 0.0)
        .unwrap_or(0);
    displays[primary].primary = true;
    Some(displays)
}
