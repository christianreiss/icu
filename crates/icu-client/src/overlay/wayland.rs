//! Wayland layer-shell overlay (Spike 4b): own thread, own Wayland
//! connection, calloop with a 30/15 fps GPU/software timer, layer `Overlay`, empty input
//! region, exclusive zone −1, the scene drawn into a wl_shm buffer.
//!
//! With `overlay.shy` the tiles that are shown take pointer input instead
//! (`Scene::input_rects`): only so does the compositor tell where the
//! pointer is. The whole row hides as soon as the pointer is on a tile and
//! gives its input up, so clicks reach what is below again.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        calloop_wayland_source::WaylandSource,
        protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
            Shape, WpCursorShapeDeviceV1,
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{
            cursor_shape::CursorShapeManager, PointerEvent, PointerEventKind, PointerHandler,
        },
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, Proxy, QueueHandle,
};

use super::{Scene, Slot, TICK};
use crate::config::Corner;

struct App {
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    surface: Option<LayerSurface>,
    configured: bool,
    scene: Scene,
    qh: QueueHandle<App>,
    seats: SeatState,
    /// The seat's pointer: where it is over the tiles, for `overlay.shy`.
    pointer: Option<wl_pointer::WlPointer>,
    /// A surface the pointer enters owns its image: the tiles show the
    /// normal arrow, where the compositor offers cursor shapes.
    cursor_shapes: Option<CursorShapeManager>,
    cursor_shape: Option<WpCursorShapeDeviceV1>,
    /// The input region the surface has, as tile slots (empty: none).
    input: Vec<Slot>,
}

/// Runs the overlay until `stop`; reports on `ready` whether it came up.
pub(super) fn run(
    scene: Scene,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let (conn, mut event_loop, mut app) = match setup(scene) {
        Ok(x) => x,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = event_loop.dispatch(Some(Duration::from_millis(100)), &mut app) {
            tracing::warn!(%e, "overlay loop");
            break;
        }
    }
    app.surface = None;
    let _ = conn.flush();
}

/// Connects to the compositor, binds the globals and builds the event loop
/// with its 15 fps timer. Each step names itself in the error the window
/// shows as "nicht verfügbar".
fn setup(scene: Scene) -> Result<(Connection, EventLoop<'static, App>, App), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland: {e}"))?;
    let (globals, event_queue) =
        registry_queue_init::<App>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = event_queue.handle();
    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|e| format!("compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|e| format!("kein zwlr_layer_shell_v1 (GNOME?): {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("shm: {e}"))?;
    let pool = SlotPool::new(320 * 240 * 4 * 4, &shm).map_err(|e| format!("pool: {e}"))?;
    let event_loop: EventLoop<App> = EventLoop::try_new().map_err(|e| format!("calloop: {e}"))?;
    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|e| format!("wayland source: {e}"))?;
    event_loop
        .handle()
        .insert_source(Timer::from_duration(TICK), move |_, _, app| {
            let started = std::time::Instant::now();
            app.tick();
            TimeoutAction::ToDuration(app.scene.interval().saturating_sub(started.elapsed()))
        })
        .map_err(|e| format!("timer: {e}"))?;
    let app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        surface: None,
        configured: false,
        scene,
        qh: qh.clone(),
        seats: SeatState::new(&globals, &qh),
        pointer: None,
        cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
        cursor_shape: None,
        input: Vec::new(),
    };
    Ok((conn, event_loop, app))
}

impl App {
    fn tick(&mut self) {
        let update = self.scene.update();
        if !update.shown {
            if self.surface.take().is_some() {
                self.configured = false;
            }
            return;
        }
        let (width, height) = (self.scene.width, self.scene.height);
        if self.surface.is_none() || update.layout_changed {
            self.surface = None;
            self.configured = false;
            let surface = self.compositor.create_surface(&self.qh);
            let layer = self.layer_shell.create_layer_surface(
                &self.qh,
                surface,
                Layer::Overlay,
                Some("icu-overlay"),
                None,
            );
            let settings = &self.scene.settings;
            let anchor = match settings.corner {
                Corner::TopLeft => Anchor::TOP | Anchor::LEFT,
                Corner::TopRight => Anchor::TOP | Anchor::RIGHT,
                Corner::BottomLeft => Anchor::BOTTOM | Anchor::LEFT,
                Corner::BottomRight => Anchor::BOTTOM | Anchor::RIGHT,
                // No LEFT/RIGHT: wlr-layer-shell centers the surface on the
                // horizontal axis when neither of its edges is anchored.
                Corner::Top => Anchor::TOP,
                Corner::Bottom => Anchor::BOTTOM,
            };
            layer.set_anchor(anchor);
            let (ox, oy) = (settings.offset[0] as i32, settings.offset[1] as i32);
            layer.set_margin(oy, ox, oy, ox);
            layer.set_size(width, height);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_exclusive_zone(-1);
            if let Ok(empty) = Region::new(&self.compositor) {
                layer.set_input_region(Some(empty.wl_region()));
            }
            self.input.clear();
            self.scene.pointer_at(None);
            layer.commit();
            tracing::info!(
                width,
                height,
                tiles = self.scene.tiles.len(),
                "overlay layer surface created"
            );
            self.surface = Some(layer);
            return;
        }
        // The new size goes out with the next picture, in one commit: on
        // its own the compositor would place the old picture by the new
        // size (centered on an edge it moves sideways at once) and the
        // rest of the change would follow only with the picture.
        if update.size_changed {
            if let Some(layer) = &self.surface {
                layer.set_size(width, height);
            }
        }
        if self.configured {
            let input_changed = self.set_input();
            if !self.draw() && input_changed {
                if let Some(layer) = &self.surface {
                    layer.commit();
                }
            }
        }
    }

    /// Stages the input region the scene wants, when it changed, for the
    /// next commit; true if it did.
    fn set_input(&mut self) -> bool {
        let rects = self.scene.input_rects();
        if rects == self.input {
            return false;
        }
        let Some(layer) = &self.surface else {
            return false;
        };
        let Ok(region) = Region::new(&self.compositor) else {
            return false;
        };
        for &[x, y, w, h] in &rects {
            region.add(x as i32, y as i32, w as i32, h as i32);
        }
        layer.set_input_region(Some(region.wl_region()));
        self.input = rects;
        true
    }

    /// Draws and commits a picture; false if there was none to show yet.
    fn draw(&mut self) -> bool {
        let Some(layer) = &self.surface else {
            return false;
        };
        let (w, h) = (self.scene.width as i32, self.scene.height as i32);
        if w <= 0 || h <= 0 {
            return false;
        }
        let Ok((buffer, canvas)) = self
            .pool
            .create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)
        else {
            return false;
        };
        if !self.scene.draw(canvas) {
            return false;
        }
        layer.wl_surface().damage_buffer(0, 0, w, h);
        if buffer.attach_to(layer.wl_surface()).is_err() {
            return false;
        }
        layer.commit();
        self.scene.report_displayed();
        true
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.surface = None;
        self.configured = false;
        self.input.clear();
        self.scene.pointer_at(None);
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        c: LayerSurfaceConfigure,
        _: u32,
    ) {
        // The size is the one asked for with `set_size` — both are always
        // set, so the compositor has nothing to choose. While the size
        // sliders move, one for a size already gone by can still arrive:
        // the scene's own size stays, so no picture of a stale size ever
        // goes out between two current ones.
        tracing::debug!(size = ?c.new_size, "overlay configured");
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Pointer || self.pointer.is_some() {
            return;
        }
        if let Ok(pointer) = self.seats.get_pointer(qh, &seat) {
            self.cursor_shape = self
                .cursor_shapes
                .as_ref()
                .map(|shapes| shapes.get_shape_device(&pointer, qh));
            self.pointer = Some(pointer);
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Pointer {
            return;
        }
        if let Some(shape) = self.cursor_shape.take() {
            shape.destroy();
        }
        if let Some(pointer) = self.pointer.take() {
            if pointer.version() >= 3 {
                pointer.release();
            }
        }
        self.scene.pointer_at(None);
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let Some(layer) = &self.surface else { return };
        for event in events {
            if &event.surface != layer.wl_surface() {
                continue;
            }
            // Surface-local coordinates are canvas pixels: the compositor
            // scales the surface, the scene draws at scale 1.
            let at = (event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(shape) = &self.cursor_shape {
                        shape.set_shape(serial, Shape::Default);
                    }
                    self.scene.pointer_at(Some(at));
                }
                PointerEventKind::Leave { .. } => self.scene.pointer_at(None),
                _ => self.scene.pointer_at(Some(at)),
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
