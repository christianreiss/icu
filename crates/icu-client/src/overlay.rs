//! Wayland layer-shell overlay (`Client/IMPLEMENTATION.md` §9, Spike 4b):
//! own thread, own Wayland connection, calloop with a 15 fps timer, layer
//! `Overlay`, empty input region, exclusive zone −1. Composes camera tiles
//! of the participants in premultiplied ARGB into a wl_shm buffer.
//!
//! Windows layered-window overlay is M3; on non-Wayland hosts `start` fails
//! and the UI shows "nicht verfügbar".

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::Duration,
};

use icu_proto::Session;
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
use tokio::sync::watch;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use crate::{
    config::{Corner, Expand, OverlaySettings, TileSize},
    engine::{AppState, VideoFrames},
};

const GAP: u32 = 8;
const BORDER: u32 = 3;
/// Brightness multiplier (out of 256) applied to dimmed, non-speaking tiles.
const DIM_MUL: u32 = 128;

/// Owns the overlay thread.
pub struct OverlayHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OverlayHandle {
    /// Connects to the compositor on a new thread. Fails when there is no
    /// Wayland display or no layer shell (GNOME).
    pub fn start(
        state: watch::Receiver<AppState>,
        frames: VideoFrames,
    ) -> Result<OverlayHandle, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("icu-overlay".into())
            .spawn(move || overlay_thread(state, frames, stop_t, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(OverlayHandle {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // No handle owns the thread now: tell it to end on its own
                // once `setup` returns, instead of running on unseen. Not
                // joined here — `setup` may be stuck for a while yet.
                stop.store(true, Ordering::Relaxed);
                Err("overlay thread did not start".into())
            }
        }
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Tile {
    rgba: Option<(usize, usize, Arc<Vec<u8>>)>,
    speaking: bool,
    dim: bool,
}

struct App {
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    surface: Option<LayerSurface>,
    configured: bool,
    width: u32,
    height: u32,
    /// The overlay settings as of the last tick.
    settings: OverlaySettings,
    tiles: Vec<Tile>,
    state: watch::Receiver<AppState>,
    frames: VideoFrames,
    qh: QueueHandle<App>,
}

fn overlay_thread(
    state: watch::Receiver<AppState>,
    frames: VideoFrames,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let (conn, mut event_loop, mut app) = match setup(state, frames) {
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
fn setup(
    state: watch::Receiver<AppState>,
    frames: VideoFrames,
) -> Result<(Connection, EventLoop<'static, App>, App), String> {
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
    let tick = Duration::from_millis(1000 / 15);
    event_loop
        .handle()
        .insert_source(Timer::from_duration(tick), move |_, _, app| {
            app.tick();
            TimeoutAction::ToDuration(tick)
        })
        .map_err(|e| format!("timer: {e}"))?;
    let settings = state.borrow().settings.overlay.clone();
    let app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        surface: None,
        configured: false,
        width: 0,
        height: 0,
        settings,
        tiles: Vec::new(),
        state,
        frames,
        qh: qh.clone(),
    };
    Ok((conn, event_loop, app))
}

/// What decides the layer surface's anchor, margin and size: a change here
/// needs the surface created anew.
fn layout(s: &OverlaySettings) -> (Corner, [u32; 2], TileSize, Expand) {
    (s.corner, s.offset, s.size, s.expand)
}

impl App {
    fn tile_dims(&self) -> (u32, u32) {
        match self.settings.size {
            TileSize::Small => (160, 120),
            TileSize::Medium => (320, 240),
            TileSize::Large => (480, 360),
        }
    }

    /// `Top`/`Bottom` are always laid out horizontally (and centered by the
    /// compositor); other positions follow the configured expand direction.
    fn effective_expand(&self) -> Expand {
        match self.settings.corner {
            Corner::Top | Corner::Bottom => Expand::Horizontal,
            _ => self.settings.expand,
        }
    }

    fn tick(&mut self) {
        // Only what a tile needs leaves the state: the whole `AppState`
        // (device list, user list, invite text) is not worth cloning at
        // 15 Hz.
        let (settings, mut peers) = {
            let state = self.state.borrow();
            let peers: Vec<(Session, String, bool)> = state
                .peers
                .iter()
                .filter(|p| p.info.flags.camera_on())
                .map(|p| (p.info.session, p.info.name.clone(), p.speaking))
                .collect();
            (state.settings.overlay.clone(), peers)
        };
        let layout_changed = layout(&settings) != layout(&self.settings);
        self.settings = settings;

        // Tiles: every peer with the camera on, in name order.
        let frames = self.frames.latest();
        peers.sort_by(|a, b| a.1.cmp(&b.1));
        self.tiles = peers
            .iter()
            .map(|(session, _, speaking)| Tile {
                rgba: frames
                    .iter()
                    .find(|(s, _)| s == session)
                    .map(|(_, f)| (f.width, f.height, f.rgba.clone())),
                speaking: *speaking,
                dim: self.settings.dim_non_speaking && !speaking,
            })
            .collect();

        let want = self.settings.enabled && !self.tiles.is_empty();
        if !want {
            if self.surface.take().is_some() {
                self.configured = false;
            }
            return;
        }
        let (tw, th) = self.tile_dims();
        let n = self.tiles.len() as u32;
        let (width, height) = match self.effective_expand() {
            Expand::Vertical => (tw + 2 * BORDER, n * (th + 2 * BORDER) + (n - 1) * GAP),
            Expand::Horizontal => (n * (tw + 2 * BORDER) + (n - 1) * GAP, th + 2 * BORDER),
        };
        let size_changed = width != self.width || height != self.height;
        self.width = width;
        self.height = height;
        if self.surface.is_none() || layout_changed {
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
            let anchor = match self.settings.corner {
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
            let (ox, oy) = (
                self.settings.offset[0] as i32,
                self.settings.offset[1] as i32,
            );
            layer.set_margin(oy, ox, oy, ox);
            layer.set_size(width, height);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_exclusive_zone(-1);
            if let Ok(empty) = Region::new(&self.compositor) {
                layer.set_input_region(Some(empty.wl_region()));
            }
            layer.commit();
            tracing::info!(width, height, tiles = n, "overlay layer surface created");
            self.surface = Some(layer);
            return;
        }
        if size_changed {
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
        let (w, h) = (self.width as i32, self.height as i32);
        if w <= 0 || h <= 0 {
            return;
        }
        let stride = w * 4;
        let (tw, th) = self.tile_dims();
        let horizontal = self.effective_expand() == Expand::Horizontal;
        let Ok((buffer, canvas)) = self
            .pool
            .create_buffer(w, h, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };
        canvas.fill(0);
        let mut x0 = 0u32;
        let mut y0 = 0u32;
        for tile in &self.tiles {
            let border = if tile.speaking && self.settings.speaking_frame {
                let [r, g, b] = self.settings.frame_color;
                [b, g, r, 0xff] // B G R A premultiplied
            } else {
                [0x30, 0x30, 0x30, 0xc0]
            };
            let outer_w = tw + 2 * BORDER;
            let outer_h = th + 2 * BORDER;
            let mul = if tile.dim { DIM_MUL } else { 256 };
            for y in 0..outer_h {
                for x in 0..outer_w {
                    let inside = x >= BORDER && x < BORDER + tw && y >= BORDER && y < BORDER + th;
                    let px = if inside {
                        match &tile.rgba {
                            Some((fw, fh, rgba)) => {
                                let sx = ((x - BORDER) as usize * fw) / tw as usize;
                                let sy = ((y - BORDER) as usize * fh) / th as usize;
                                let i = (sy * fw + sx) * 4;
                                [
                                    ((rgba[i + 2] as u32 * mul) >> 8) as u8,
                                    ((rgba[i + 1] as u32 * mul) >> 8) as u8,
                                    ((rgba[i] as u32 * mul) >> 8) as u8,
                                    0xff,
                                ]
                            }
                            None => [0x20, 0x20, 0x20, 0xc0],
                        }
                    } else {
                        border
                    };
                    let i = (((y0 + y) as i32 * stride) + ((x0 + x) as i32 * 4)) as usize;
                    canvas[i..i + 4].copy_from_slice(&px);
                }
            }
            if horizontal {
                x0 += outer_w + GAP;
            } else {
                y0 += outer_h + GAP;
            }
        }
        layer.wl_surface().damage_buffer(0, 0, w, h);
        if buffer.attach_to(layer.wl_surface()).is_ok() {
            layer.commit();
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
            self.width = c.new_size.0;
            self.height = c.new_size.1;
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
