//! Wayland layer-shell overlay (Spike 4b): own thread, own Wayland
//! connection, calloop with a 15 fps timer, layer `Overlay`, empty input
//! region, exclusive zone −1, the scene drawn into a wl_shm buffer.

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
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
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
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use super::{Scene, TICK};
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
            app.tick();
            TimeoutAction::ToDuration(TICK)
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
        if update.size_changed {
            if let Some(layer) = &self.surface {
                layer.set_size(width, height);
                layer.commit();
            }
        }
        if self.configured {
            self.draw();
        }
    }

    fn draw(&mut self) {
        let Some(layer) = &self.surface else { return };
        let (w, h) = (self.scene.width as i32, self.scene.height as i32);
        if w <= 0 || h <= 0 {
            return;
        }
        let Ok((buffer, canvas)) = self
            .pool
            .create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        self.scene.draw(canvas);
        layer.wl_surface().damage_buffer(0, 0, w, h);
        if buffer.attach_to(layer.wl_surface()).is_ok() {
            layer.commit();
            self.scene.report_displayed();
        }
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
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        c: LayerSurfaceConfigure,
        _: u32,
    ) {
        if c.new_size.0 > 0 && c.new_size.1 > 0 {
            self.scene.width = c.new_size.0;
            self.scene.height = c.new_size.1;
        }
        tracing::debug!(size = ?c.new_size, "overlay configured");
        self.configured = true;
        self.draw();
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
    registry_handlers![OutputState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
