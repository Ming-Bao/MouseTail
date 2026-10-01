//! The ripple where the cursor crosses (see `mousetail_core::ripple`): a transparent,
//! click-through window over each display, drawn with Metal and on screen only while a ripple
//! plays.
//!
//! AppKit wants the main thread (kept for the run loop, see `run_main_loop`), so everything
//! here hops onto it and lives in a thread-local there. Frames come from a run loop timer that
//! only runs while something is rippling.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Instant;

use core_foundation::base::TCFType;
use core_foundation::date::CFAbsoluteTimeGetCurrent;
use core_foundation::runloop::{
    CFRunLoop, CFRunLoopTimer, CFRunLoopTimerInvalidate, CFRunLoopTimerRef, kCFRunLoopCommonModes,
};
use dispatch2::DispatchQueue;
use mousetail_core::layout::{Point, Rect};
use mousetail_core::ripple::{self, Ripple};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSScreen, NSScreenSaverWindowLevel, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLLoadAction, MTLPixelFormat,
    MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLStoreAction,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

pub struct Ripples;

impl Ripples {
    pub fn start() -> anyhow::Result<Self> {
        anyhow::ensure!(MTLCreateSystemDefaultDevice().is_some(), "no Metal device");
        // Compile the shader now rather than on the first crossing, which it would hold up.
        DispatchQueue::main().exec_async(|| {
            STATE.with_borrow_mut(|state| match State::new() {
                Ok(s) => *state = Some(s),
                Err(e) => tracing::warn!("can't draw the crossing ripple: {e:#}"),
            });
        });
        Ok(Self)
    }

    /// Ripple at `at`, in global display coordinates (top-left origin, y down).
    pub fn show(&self, at: Point, strength: f32) {
        let started = Instant::now();
        DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().expect("on the main queue");
            STATE.with_borrow_mut(|state| {
                if let Some(state) = state {
                    state.show(mtm, at, strength, started);
                }
            });
        });
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Mirrors `ripple::height` and `ripple::shade`.
fn shader() -> String {
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

struct Ripple {{ float2 origin; float age; float strength; }};
struct Uniforms {{ float scale; int count; }};
struct Out {{ float4 position [[position]]; }};

vertex Out vert(uint id [[vertex_id]]) {{
    float2 p = float2((id << 1) & 2, id & 2); // one triangle covering the screen
    Out o;
    o.position = float4(p * 2.0 - 1.0, 0.0, 1.0);
    return o;
}}

static float height(float d, float age, float strength) {{
    float front = {speed:?} * age;
    float behind = front - d;
    float width = {wavelength:?} * (1.2 + 3.0 * age);
    float train = behind > 0 ? exp(-pow(behind / width, 2.0)) : exp(-pow(behind / ({wavelength:?} * 0.35), 2.0));
    float fade = pow(1.0 - clamp(age / {lifetime:?}, 0.0, 1.0), 2.0);
    float spread = rsqrt(1.0 + d / {spread:?});
    return strength * fade * spread * train * sin(6.2831853 * behind / {wavelength:?});
}}

fragment float4 frag(Out in [[stage_in]], constant Uniforms &u [[buffer(0)]], constant Ripple *ripples [[buffer(1)]]) {{
    float2 p = in.position.xy / u.scale; // points, top-left origin
    float h = 0;
    for (int i = 0; i < u.count; i++) {{
        h += height(length(p - ripples[i].origin), ripples[i].age, ripples[i].strength);
    }}
    float white = pow(clamp(h * {gain:?}, 0.0, 1.0), 1.5) * {highlight:?};
    float black = clamp(-h * {gain:?}, 0.0, 1.0) * {shadow:?};
    return float4(white, white, white, white + black * (1.0 - white)); // premultiplied
}}
"#,
        speed = ripple::SPEED,
        wavelength = ripple::WAVELENGTH,
        spread = ripple::SPREAD,
        lifetime = ripple::LIFETIME,
        gain = ripple::GAIN,
        highlight = ripple::HIGHLIGHT,
        shadow = ripple::SHADOW,
    )
}

#[repr(C)]
struct GpuRipple {
    origin: [f32; 2],
    age: f32,
    strength: f32,
}

#[repr(C)]
struct Uniforms {
    scale: f32,
    count: i32,
}

struct State {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    overlays: Vec<Overlay>,
    timer: Option<CFRunLoopTimer>,
}

/// One display's window.
struct Overlay {
    /// Where the display is, in global display coordinates.
    rect: Rect,
    scale: f64,
    window: Retained<NSWindow>,
    layer: Retained<CAMetalLayer>,
    ripples: Vec<Ripple>,
}

impl State {
    fn new() -> anyhow::Result<Self> {
        let device =
            MTLCreateSystemDefaultDevice().ok_or_else(|| anyhow::anyhow!("no Metal device"))?;
        let library = device
            .newLibraryWithSource_options_error(&NSString::from_str(&shader()), None)
            .map_err(|e| anyhow::anyhow!("compiling the shader: {}", e.localizedDescription()))?;
        let function = |name: &str| {
            library
                .newFunctionWithName(&NSString::from_str(name))
                .ok_or_else(|| anyhow::anyhow!("no {name} in the shader"))
        };
        let desc = MTLRenderPipelineDescriptor::new();
        let (vert, frag) = (function("vert")?, function("frag")?);
        desc.setVertexFunction(Some(&vert));
        desc.setFragmentFunction(Some(&frag));
        unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&desc)
            .map_err(|e| anyhow::anyhow!("building the pipeline: {}", e.localizedDescription()))?;
        let queue = device
            .newCommandQueue()
            .ok_or_else(|| anyhow::anyhow!("no Metal command queue"))?;
        Ok(Self {
            device,
            queue,
            pipeline,
            overlays: vec![],
            timer: None,
        })
    }

    fn show(&mut self, mtm: MainThreadMarker, at: Point, strength: f32, started: Instant) {
        self.follow_screens(mtm);
        // The display the point is on, looking just inside it either way: a right or bottom
        // edge isn't within its own display's rect.
        let nudges = [(0.0, 0.0), (-0.5, 0.0), (0.5, 0.0), (0.0, -0.5), (0.0, 0.5)];
        let Some(index) = nudges.iter().find_map(|(dx, dy)| {
            let p = Point::new(at.x + dx, at.y + dy);
            self.overlays.iter().position(|o| o.rect.contains(p))
        }) else {
            return;
        };
        let overlay = &mut self.overlays[index];
        let origin = at.minus(Point::new(overlay.rect.x, overlay.rect.y));
        ripple::push(
            &mut overlay.ripples,
            Ripple {
                origin,
                started,
                strength,
            },
        );
        overlay.window.orderFrontRegardless();
        if self.timer.is_none() {
            self.timer = Some(start_timer());
        }
    }

    /// Match the overlays to the displays, which may have changed since the last ripple.
    fn follow_screens(&mut self, mtm: MainThreadMarker) {
        let screens = NSScreen::screens(mtm);
        let Some(primary) = screens.firstObject() else {
            self.overlays.clear();
            return;
        };
        let primary_height = primary.frame().size.height;
        let wanted: Vec<(Rect, f64, NSRect)> = screens
            .iter()
            .map(|s| {
                let f = s.frame();
                // AppKit measures up from the bottom of the main display; we measure down from
                // its top.
                let rect = Rect::new(
                    f.origin.x,
                    primary_height - f.origin.y - f.size.height,
                    f.size.width,
                    f.size.height,
                );
                (rect, s.backingScaleFactor(), f)
            })
            .collect();
        let same = wanted.len() == self.overlays.len()
            && wanted
                .iter()
                .zip(&self.overlays)
                .all(|((r, s, _), o)| *r == o.rect && *s == o.scale);
        if same {
            // When a display goes away (a monitor asleep or unplugged), macOS moves its window
            // onto another display, and leaves it there when the display comes back where it
            // was. Put any it moved back.
            for ((_, _, frame), o) in wanted.iter().zip(&self.overlays) {
                if o.window.frame() != *frame {
                    o.window.setFrame_display(*frame, false);
                }
            }
            return;
        }
        for o in self.overlays.drain(..) {
            o.window.orderOut(None);
        }
        for (rect, scale, frame) in wanted {
            self.overlays
                .push(Overlay::new(mtm, &self.device, rect, scale, frame));
        }
    }

    /// Draw every display that's rippling; stop once none is.
    fn frame(&mut self) {
        let now = Instant::now();
        let mut any = false;
        for o in &mut self.overlays {
            if o.ripples.is_empty() {
                continue;
            }
            o.ripples.retain(|r| !r.done(now));
            if o.ripples.is_empty() {
                o.window.orderOut(None);
                continue;
            }
            any = true;
            o.draw(&self.queue, &self.pipeline, now);
        }
        if !any && let Some(timer) = self.timer.take() {
            unsafe { CFRunLoopTimerInvalidate(timer.as_concrete_TypeRef()) };
        }
    }
}

impl Overlay {
    fn new(
        mtm: MainThreadMarker,
        device: &ProtocolObject<dyn MTLDevice>,
        rect: Rect,
        scale: f64,
        frame: NSRect,
    ) -> Self {
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        window.setIgnoresMouseEvents(true);
        window.setLevel(NSScreenSaverWindowLevel);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        // Borderless windows can be nudged below the menu bar; put it back over the display.
        window.setFrame_display(frame, false);

        let layer = CAMetalLayer::new();
        layer.setDevice(Some(device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setOpaque(false);
        layer.setContentsScale(scale);
        layer.setDrawableSize(CGSize::new(rect.w * scale, rect.h * scale));
        let view = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(rect.w, rect.h)),
        );
        // Layer-hosting: our layer, drawn only by us.
        view.setLayer(Some(&layer));
        view.setWantsLayer(true);
        window.setContentView(Some(&view));
        Self {
            rect,
            scale,
            window,
            layer,
            ripples: vec![],
        }
    }

    fn draw(
        &self,
        queue: &ProtocolObject<dyn MTLCommandQueue>,
        pipeline: &ProtocolObject<dyn MTLRenderPipelineState>,
        now: Instant,
    ) {
        let Some(drawable) = self.layer.nextDrawable() else {
            return;
        };
        let pass = MTLRenderPassDescriptor::new();
        let target = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        target.setTexture(Some(&drawable.texture()));
        target.setLoadAction(MTLLoadAction::Clear);
        target.setClearColor(MTLClearColor {
            red: 0.0,
            green: 0.0,
            blue: 0.0,
            alpha: 0.0,
        });
        target.setStoreAction(MTLStoreAction::Store);
        let Some(commands) = queue.commandBuffer() else {
            return;
        };
        let Some(encoder) = commands.renderCommandEncoderWithDescriptor(&pass) else {
            return;
        };
        let mut ripples: Vec<GpuRipple> = self
            .ripples
            .iter()
            .map(|r| GpuRipple {
                origin: [r.origin.x as f32, r.origin.y as f32],
                age: r.age(now),
                strength: r.strength,
            })
            .collect();
        let mut uniforms = Uniforms {
            scale: self.scale as f32,
            count: ripples.len() as i32,
        };
        encoder.setRenderPipelineState(pipeline);
        unsafe {
            encoder.setFragmentBytes_length_atIndex(
                NonNull::from(&mut uniforms).cast::<c_void>(),
                size_of::<Uniforms>(),
                0,
            );
            encoder.setFragmentBytes_length_atIndex(
                NonNull::new(ripples.as_mut_ptr()).unwrap().cast::<c_void>(),
                size_of::<GpuRipple>() * ripples.len(),
                1,
            );
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        encoder.endEncoding();
        commands.presentDrawable(ProtocolObject::from_ref(&*drawable));
        commands.commit();
    }
}

/// A timer on the main run loop, firing every display frame until invalidated.
fn start_timer() -> CFRunLoopTimer {
    extern "C" fn tick(_: CFRunLoopTimerRef, _: *mut c_void) {
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.frame();
            }
        });
    }
    let timer = CFRunLoopTimer::new(
        unsafe { CFAbsoluteTimeGetCurrent() },
        1.0 / 120.0,
        0,
        0,
        tick,
        std::ptr::null_mut(),
    );
    CFRunLoop::get_main().add_timer(&timer, unsafe { kCFRunLoopCommonModes });
    timer
}
