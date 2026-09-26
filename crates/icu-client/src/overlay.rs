//! Wayland layer-shell overlay (`Client/IMPLEMENTATION.md` §9, Spike 4b):
//! own thread, own Wayland connection, calloop with a 15 fps timer, layer
//! `Overlay`, empty input region, exclusive zone −1. Composes, in
//! premultiplied ARGB into a wl_shm buffer, a camera tile with the name for
//! everyone whose camera is on, and a name plate for everyone without one
//! while they talk — so the overlay answers „wer spricht?" in a voice-only
//! round too.
//!
//! Windows layered-window overlay is M3; without layer shell (GNOME) `start`
//! fails, the Overlay tile is left out and the settings say why.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::Duration,
};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

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

/// What a tile shows.
enum Face {
    /// Camera on: the latest picture, once one has arrived.
    Camera(Option<(usize, usize, Arc<Vec<u8>>)>),
    /// No camera, talking right now: a name plate.
    Plate,
}

struct Tile {
    face: Face,
    name: String,
    speaking: bool,
    dim: bool,
}

/// A name rasterized once as an alpha mask, width × height.
struct Label {
    width: u32,
    height: u32,
    alpha: Vec<u8>,
}

/// Rasterizes names with the app's own semibold Inter, cached per name and
/// size: at 15 fps a name is drawn far more often than it changes.
struct Labels {
    font: FontRef<'static>,
    cache: HashMap<(String, u32, u32), Label>,
}

impl Labels {
    fn new() -> Option<Labels> {
        let font = FontRef::try_from_slice(crate::ui::fonts::INTER_SEMIBOLD).ok()?;
        Some(Labels {
            font,
            cache: HashMap::new(),
        })
    }

    /// `text` at `px` pixels, cut with „…" to at most `max_width`.
    fn get(&mut self, text: &str, px: u32, max_width: u32) -> &Label {
        let key = (text.to_string(), px, max_width);
        if !self.cache.contains_key(&key) {
            // A name list changes rarely; a runaway cache would only mean
            // somebody renamed thousands of times.
            if self.cache.len() > 256 {
                self.cache.clear();
            }
            let label = self.render(text, px, max_width);
            self.cache.insert(key.clone(), label);
        }
        &self.cache[&key]
    }

    fn render(&self, text: &str, px: u32, max_width: u32) -> Label {
        let scaled = self.font.as_scaled(PxScale::from(px as f32));
        let width_of = |t: &str| {
            let mut w = 0.0f32;
            let mut prev = None;
            for c in t.chars() {
                let id = scaled.glyph_id(c);
                if let Some(p) = prev {
                    w += scaled.kern(p, id);
                }
                w += scaled.h_advance(id);
                prev = Some(id);
            }
            w
        };
        let mut shown = text.to_string();
        if width_of(&shown) > max_width as f32 {
            let mut chars: Vec<char> = text.chars().collect();
            while !chars.is_empty() {
                chars.pop();
                let candidate = format!("{}…", chars.iter().collect::<String>().trim_end());
                if width_of(&candidate) <= max_width as f32 {
                    shown = candidate;
                    break;
                }
            }
        }
        let width = (width_of(&shown).ceil() as u32).clamp(1, max_width.max(1));
        let height = ((scaled.ascent() - scaled.descent()).ceil() as u32).max(1);
        let mut alpha = vec![0u8; (width * height) as usize];
        let mut caret = 0.0f32;
        let mut prev = None;
        for c in shown.chars() {
            let id = scaled.glyph_id(c);
            if let Some(p) = prev {
                caret += scaled.kern(p, id);
            }
            let glyph = id.with_scale_and_position(
                PxScale::from(px as f32),
                ab_glyph::point(caret, scaled.ascent()),
            );
            caret += scaled.h_advance(id);
            prev = Some(id);
            let Some(outline) = self.font.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outline.px_bounds();
            outline.draw(|x, y, coverage| {
                let px = bounds.min.x as i32 + x as i32;
                let py = bounds.min.y as i32 + y as i32;
                if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                    let i = (py as u32 * width + px as u32) as usize;
                    alpha[i] = alpha[i].max((coverage * 255.0).round() as u8);
                }
            });
        }
        Label {
            width,
            height,
            alpha,
        }
    }
}

/// Blends a white `label` into `canvas` (premultiplied BGRA, `stride`
/// bytes per row) at `x`, `y`, clipped to the canvas.
fn blend_label(canvas: &mut [u8], stride: usize, x: u32, y: u32, label: &Label) {
    for ly in 0..label.height {
        for lx in 0..label.width {
            let a = u32::from(label.alpha[(ly * label.width + lx) as usize]);
            if a == 0 {
                continue;
            }
            let i = (y + ly) as usize * stride + (x + lx) as usize * 4;
            let Some(px) = canvas.get_mut(i..i + 4) else {
                continue;
            };
            for c in px.iter_mut() {
                *c = ((255 * a + u32::from(*c) * (255 - a)) / 255) as u8;
            }
        }
    }
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
    /// Name rendering; `None` only if the embedded font failed to parse.
    labels: Option<Labels>,
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
        labels: Labels::new(),
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

    /// Height of a name plate, and the name's pixel size, per tile size.
    fn plate_dims(&self) -> (u32, u32) {
        match self.settings.size {
            TileSize::Small => (26, 12),
            TileSize::Medium => (34, 15),
            TileSize::Large => (42, 18),
        }
    }

    /// Inner height of a tile.
    fn tile_height(&self, tile: &Tile) -> u32 {
        match tile.face {
            Face::Camera(_) => self.tile_dims().1,
            Face::Plate => self.plate_dims().0,
        }
    }

    /// Whether the surface hangs from its bottom edge: the tiles of a
    /// horizontal row line up there.
    fn anchored_bottom(&self) -> bool {
        matches!(
            self.settings.corner,
            Corner::BottomLeft | Corner::BottomRight | Corner::Bottom
        )
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
            let plates = state.settings.overlay.voice_plates;
            let peers: Vec<(Session, String, bool, bool)> = state
                .peers
                .iter()
                .filter(|p| p.info.flags.camera_on() || (plates && p.speaking))
                .map(|p| {
                    (
                        p.info.session,
                        p.info.name.clone(),
                        p.speaking,
                        p.info.flags.camera_on(),
                    )
                })
                .collect();
            (state.settings.overlay.clone(), peers)
        };
        let layout_changed = layout(&settings) != layout(&self.settings);
        self.settings = settings;

        // Camera tiles first, then the plates of whoever talks without a
        // camera; each group in name order.
        let frames = self.frames.latest();
        peers.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.1.cmp(&b.1)));
        self.tiles = peers
            .into_iter()
            .map(|(session, name, speaking, camera)| Tile {
                face: if camera {
                    Face::Camera(
                        frames
                            .iter()
                            .find(|(s, _)| *s == session)
                            .map(|(_, f)| (f.width, f.height, f.rgba.clone())),
                    )
                } else {
                    Face::Plate
                },
                name,
                speaking,
                dim: camera && self.settings.dim_non_speaking && !speaking,
            })
            .collect();

        let want = self.settings.enabled && !self.tiles.is_empty();
        if !want {
            if self.surface.take().is_some() {
                self.configured = false;
            }
            return;
        }
        let (tw, _) = self.tile_dims();
        let n = self.tiles.len() as u32;
        let heights: Vec<u32> = self.tiles.iter().map(|t| self.tile_height(t)).collect();
        let (width, height) = match self.effective_expand() {
            Expand::Vertical => (
                tw + 2 * BORDER,
                heights.iter().map(|h| h + 2 * BORDER).sum::<u32>() + (n - 1) * GAP,
            ),
            Expand::Horizontal => (
                n * (tw + 2 * BORDER) + (n - 1) * GAP,
                heights.iter().max().copied().unwrap_or(0) + 2 * BORDER,
            ),
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
        let (tile_w, camera_h) = self.tile_dims();
        let (plate_h, name_px) = self.plate_dims();
        let geometry = Geometry {
            width: self.width,
            height: self.height,
            tile_w,
            camera_h,
            plate_h,
            name_px,
            horizontal: self.effective_expand() == Expand::Horizontal,
            bottom: self.anchored_bottom(),
            speaking_frame: self.settings.speaking_frame,
            frame_color: self.settings.frame_color,
        };
        let Ok((buffer, canvas)) = self
            .pool
            .create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        compose(canvas, &geometry, &self.tiles, self.labels.as_mut());
        layer.wl_surface().damage_buffer(0, 0, w, h);
        if buffer.attach_to(layer.wl_surface()).is_ok() {
            layer.commit();
        }
    }
}

/// Where and how big the tiles are; everything [`compose`] needs besides
/// the tiles themselves.
struct Geometry {
    width: u32,
    height: u32,
    tile_w: u32,
    camera_h: u32,
    plate_h: u32,
    name_px: u32,
    horizontal: bool,
    /// The surface hangs from its bottom edge: a row lines up there.
    bottom: bool,
    speaking_frame: bool,
    frame_color: [u8; 3],
}

/// Draws the tiles into `canvas` (premultiplied BGRA, `width × height`):
/// a border, the picture or a plate, and the name. Every pixel access is
/// bounds-checked — a frame that does not match its announced size must
/// not take the client down.
fn compose(canvas: &mut [u8], g: &Geometry, tiles: &[Tile], mut labels: Option<&mut Labels>) {
    canvas.fill(0);
    let stride = g.width as usize * 4;
    let tw = g.tile_w;
    let mut x0 = 0u32;
    let mut y0 = 0u32;
    for tile in tiles {
        let th = match tile.face {
            Face::Camera(_) => g.camera_h,
            Face::Plate => g.plate_h,
        };
        let outer_w = tw + 2 * BORDER;
        let outer_h = th + 2 * BORDER;
        // In a row the tiles line up at the edge the surface hangs from.
        let ty = if g.horizontal && g.bottom {
            g.height.saturating_sub(outer_h)
        } else {
            y0
        };
        let [fr, fg, fb] = g.frame_color;
        let border = if tile.speaking && g.speaking_frame {
            [fb, fg, fr, 0xff] // B G R A premultiplied
        } else {
            [0x30, 0x30, 0x30, 0xc0]
        };
        let mul = if tile.dim { DIM_MUL } else { 256 };
        for y in 0..outer_h {
            for x in 0..outer_w {
                let inside = x >= BORDER && x < BORDER + tw && y >= BORDER && y < BORDER + th;
                let px = if !inside {
                    border
                } else {
                    match &tile.face {
                        Face::Camera(Some((fw, fh, rgba))) => {
                            let sx = ((x - BORDER) as usize * fw) / tw as usize;
                            let sy = ((y - BORDER) as usize * fh) / th as usize;
                            let i = (sy * fw + sx) * 4;
                            match rgba.get(i..i + 3) {
                                Some(&[r, g, b]) => [
                                    ((u32::from(b) * mul) >> 8) as u8,
                                    ((u32::from(g) * mul) >> 8) as u8,
                                    ((u32::from(r) * mul) >> 8) as u8,
                                    0xff,
                                ],
                                _ => [0x20, 0x20, 0x20, 0xc0],
                            }
                        }
                        Face::Camera(None) => [0x20, 0x20, 0x20, 0xc0],
                        Face::Plate => [0x1c, 0x1c, 0x1c, 0xd8],
                    }
                };
                let i = (ty + y) as usize * stride + (x0 + x) as usize * 4;
                if let Some(dst) = canvas.get_mut(i..i + 4) {
                    dst.copy_from_slice(&px);
                }
            }
        }
        if let Some(labels) = labels.as_deref_mut() {
            const PAD: u32 = 6;
            let max = tw.saturating_sub(2 * PAD + 12);
            let label = labels.get(&tile.name, g.name_px, max);
            let (lx, ly) = match tile.face {
                // A darkened strip along the bottom of the picture keeps the
                // name readable on any background.
                Face::Camera(_) => {
                    let strip = label.height + 6;
                    let top = ty + BORDER + th.saturating_sub(strip);
                    for y in top..ty + BORDER + th {
                        for x in x0 + BORDER..x0 + BORDER + tw {
                            let i = y as usize * stride + x as usize * 4;
                            if let Some(dst) = canvas.get_mut(i..i + 4) {
                                for c in &mut dst[..3] {
                                    *c = ((u32::from(*c) * 110) >> 8) as u8;
                                }
                                dst[3] = dst[3].max(0xd0);
                            }
                        }
                    }
                    (x0 + BORDER + PAD, top + 3)
                }
                // A plate: a dot in the frame color, then the name.
                Face::Plate => {
                    let cy = ty + BORDER + th / 2;
                    let cx = x0 + BORDER + PAD + 4;
                    for dy in 0..9u32 {
                        for dx in 0..9u32 {
                            let (ex, ey) = (dx as i32 - 4, dy as i32 - 4);
                            if ex * ex + ey * ey <= 16 {
                                let i =
                                    (cy + dy - 4) as usize * stride + (cx + dx - 4) as usize * 4;
                                if let Some(dst) = canvas.get_mut(i..i + 4) {
                                    dst.copy_from_slice(&[fb, fg, fr, 0xff]);
                                }
                            }
                        }
                    }
                    (
                        x0 + BORDER + PAD + 14,
                        ty + BORDER + th.saturating_sub(label.height) / 2,
                    )
                }
            };
            blend_label(canvas, stride, lx, ly, label);
        }
        if g.horizontal {
            x0 += outer_w + GAP;
        } else {
            y0 += outer_h + GAP;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(tiles: &[Tile], horizontal: bool) -> Geometry {
        let heights = tiles.iter().map(|t| match t.face {
            Face::Camera(_) => 120 + 2 * BORDER,
            Face::Plate => 26 + 2 * BORDER,
        });
        let n = tiles.len() as u32;
        let (width, height) = if horizontal {
            (
                n * (160 + 2 * BORDER) + (n - 1) * GAP,
                heights.max().unwrap(),
            )
        } else {
            (160 + 2 * BORDER, heights.sum::<u32>() + (n - 1) * GAP)
        };
        Geometry {
            width,
            height,
            tile_w: 160,
            camera_h: 120,
            plate_h: 26,
            name_px: 12,
            horizontal,
            bottom: true,
            speaking_frame: true,
            frame_color: [0x2e, 0x9b, 0xff],
        }
    }

    fn tiles() -> Vec<Tile> {
        vec![
            Tile {
                // Deliberately shorter than 64×48 claims: must not panic.
                face: Face::Camera(Some((64, 48, Arc::new(vec![200; 64 * 20 * 4])))),
                name: "Anna".into(),
                speaking: false,
                dim: true,
            },
            Tile {
                face: Face::Plate,
                name: "Eva-Maria Schulte-Wiedenbrück von Hohenfels".into(),
                speaking: true,
                dim: false,
            },
        ]
    }

    /// Camera tile and plate both carry their name, a long one cut with
    /// „…" inside the tile; a picture shorter than announced does not
    /// panic; both layouts stay inside the buffer.
    #[test]
    fn names_are_drawn_and_nothing_panics() {
        let mut labels = Labels::new().expect("embedded font");
        for horizontal in [false, true] {
            let tiles = tiles();
            let g = geometry(&tiles, horizontal);
            let mut canvas = vec![0u8; (g.width * g.height * 4) as usize];
            compose(&mut canvas, &g, &tiles, Some(&mut labels));
            // White text pixels exist (only the labels are near-white).
            let white = canvas
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[0] > 230 && p[1] > 230 && p[2] > 230)
                .count();
            assert!(white > 40, "no name text drawn ({white} white pixels)");
            if let Ok(dir) = std::env::var("ICU_UI_SHOTS") {
                let rgba: Vec<u8> = canvas
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[2], p[1], p[0], p[3]])
                    .collect();
                let name = if horizontal { "row" } else { "column" };
                image::save_buffer(
                    format!("{dir}/overlay-{name}.png"),
                    &rgba,
                    g.width,
                    g.height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
        let long = labels.get("Eva-Maria Schulte-Wiedenbrück von Hohenfels", 12, 136);
        assert!(
            long.width <= 136,
            "label wider than its room: {}",
            long.width
        );
    }
}
