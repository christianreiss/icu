//! Speaking overlay (`docs/reference/client.md` §9): a click-through window
//! above everything that shows, for everyone whose camera is on, a camera
//! tile with the name, and for everyone without one a smaller name plate.
//! Both remain visible and highlight whoever is speaking.
//!
//! What is shown and how it is drawn (GPU 30 fps, CPU fallback 15 fps) lives
//! here; the window is the platform's: a wlr layer-shell surface on Linux
//! (`wayland`), a layered topmost window on Windows (`windows`). Without
//! layer shell (GNOME) `start` fails, the Overlay tile is left out and the
//! settings say why.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use icu_proto::Session;
use tokio::sync::{mpsc, watch};

use crate::{
    config::{Corner, Expand, OverlaySettings, TileSize},
    engine::{AppState, Command, Peer, VideoFrames},
};

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
use wayland as platform;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use self::windows as platform;

mod gpu;

/// Base spacing between adjacent tiles, in layout pixels — run through
/// [`scaled`] like every other overlay dimension so it tracks DPI and tile
/// size instead of sitting fixed while everything around it grows/shrinks.
const GAP: u32 = 3;
const BORDER: u32 = 3;
/// Gap between a tile's border ring and its outside name-label band: flush,
/// so the paper tag reads as attached to the tile rather than floating near
/// it.
const LABEL_GAP: u32 = 0;
/// Vertical padding inside the label band, above/below the glyphs' ascent
/// span, that sets the band's radial thickness.
const LABEL_PAD_V: u32 = 4;
/// The max arc a name may curve across, starting at 6 o'clock and sweeping
/// counterclockwise, before it's cut with „…" — past this the letters would
/// start reading upside down.
const MAX_LABEL_ANGLE: f32 = 124.0 * std::f32::consts::PI / 180.0;
/// Brightness multiplier (out of 256) applied to dimmed, non-speaking tiles.
const DIM_MUL: u32 = 128;
/// The name label's paper-tag backdrop: a warm, opaque cream, in
/// premultiplied BGRA — deliberately not a UI-theme color, since it must
/// read the same over any tile picture or background.
const PAPER_FILL: [u8; 4] = [0xd0, 0xe8, 0xf2, 0xff];
/// The name label's ink: dark and slightly warm (R > G > B), chosen to
/// stay visually distinct from the neutral grays used elsewhere on a tile
/// (border, plate fill) so tests can tell "ink" apart from "some dark
/// pixel".
const LABEL_INK: [u8; 3] = [0x1a, 0x22, 0x2b];
/// Frame interval: 15 fps.
const TICK: Duration = Duration::from_millis(1000 / 15);

/// A tile's resting scale — its normal, not-speaking size.
const REST_SCALE: f32 = 1.0;
/// How much larger a tile grows while its person is speaking.
const SPEAK_SCALE: f32 = 1.2;
/// Fraction of the remaining gap to the target scale closed per tick;
/// the same both growing and shrinking, so both directions ease equally
/// (at 15 fps, ~5-8 ticks / 300-500 ms to settle).
const SCALE_EASE: f32 = 0.35;

/// Eases `current` toward `target` by `factor` of the remaining distance —
/// used once per tick per tile so its size animates smoothly both ways
/// instead of snapping.
fn ease(current: f32, target: f32, factor: f32) -> f32 {
    current + (target - current) * factor
}

/// Linear attack/release envelope, independent of rendering cadence.
fn fade(value: f32, speaking: bool, dt: f32) -> f32 {
    if speaking {
        (value + dt / 0.15).min(1.0)
    } else {
        (value - dt / 0.7).max(0.0)
    }
}

/// The diameter a tile's slot reserves: always the *speaking* size,
/// so the reserved slot never needs to resize as a tile's drawn size
/// animates between resting and speaking. Rounds up, not to nearest — the
/// reserved slot must never end up a fraction of a pixel smaller than the
/// exact (unrounded) size a tile can actually draw at `SPEAK_SCALE`.
fn speak_diameter(base: u32) -> u32 {
    (base as f32 * SPEAK_SCALE).ceil() as u32
}

/// Owns the overlay thread.
pub struct OverlayHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OverlayHandle {
    /// Opens the overlay on a new thread. Fails when the platform has no
    /// way to show one (no Wayland display, no layer shell).
    pub fn start(
        state: watch::Receiver<AppState>,
        frames: VideoFrames,
        cmd: mpsc::UnboundedSender<Command>,
    ) -> Result<OverlayHandle, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("icu-overlay".into())
            .spawn(move || platform::run(Scene::new(state, frames, cmd), stop_t, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(OverlayHandle {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // No handle owns the thread now: tell it to end on its own
                // once its setup returns, instead of running on unseen. Not
                // joined here — the setup may be stuck for a while yet.
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
    /// No camera: a smaller name plate, also visible while silent.
    Plate,
}

struct Tile {
    face: Face,
    name: String,
    speaking: bool,
    dim: bool,
    /// Current eased size multiplier ([`REST_SCALE`]..=[`SPEAK_SCALE`]).
    scale: f32,
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

    /// One character's own bitmap, cached separately from whole-string
    /// labels: the curved outside label places each glyph on its own
    /// rotation, so it needs them individually rather than kerned into a
    /// single strip.
    fn glyph(&mut self, c: char, px: u32) -> &Label {
        let mut buf = [0u8; 4];
        let key = (c.encode_utf8(&mut buf).to_string(), px, u32::MAX);
        if !self.cache.contains_key(&key) {
            if self.cache.len() > 256 {
                self.cache.clear();
            }
            let label = self.render(&key.0, px, u32::MAX);
            self.cache.insert(key.clone(), label);
        }
        &self.cache[&key]
    }

    /// Total advance (with kerning) of `text` at `px` pixels, unclamped.
    fn advance(&self, text: &str, px: u32) -> f32 {
        let scaled = self.font.as_scaled(PxScale::from(px as f32));
        let mut w = 0.0f32;
        let mut prev = None;
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if let Some(p) = prev {
                w += scaled.kern(p, id);
            }
            w += scaled.h_advance(id);
            prev = Some(id);
        }
        w
    }

    /// `text`, cut with „…" so its advance at `px` pixels fits `max_width`.
    fn cut(&self, text: &str, px: u32, max_width: f32) -> String {
        if self.advance(text, px) <= max_width {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let candidate = format!("{}…", chars.iter().collect::<String>().trim_end());
            if self.advance(&candidate, px) <= max_width {
                return candidate;
            }
        }
        String::new()
    }

    fn render(&self, text: &str, px: u32, max_width: u32) -> Label {
        let scaled = self.font.as_scaled(PxScale::from(px as f32));
        let shown = self.cut(text, px, max_width as f32);
        let width = (self.advance(&shown, px).ceil() as u32).clamp(1, max_width.max(1));
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

/// Blends `ink` (B, G, R) into one premultiplied BGRA pixel's color
/// channels by coverage `a` (0-255), with source-over alpha. This also
/// supports cached transparent glyph textures in the GPU renderer.
fn blend_ink(dst: &mut [u8], ink: [u8; 3], a: u32) {
    dst[3] = (a + u32::from(dst[3]) * (255 - a) / 255) as u8;
    for (c, ink_c) in dst.iter_mut().zip(ink) {
        *c = ((u32::from(ink_c) * a + u32::from(*c) * (255 - a)) / 255) as u8;
    }
}

/// Blends a `label` in `ink` into `canvas` (premultiplied BGRA, `stride`
/// bytes per row) at `x`, `y`, clipped to the canvas.
fn blend_label(canvas: &mut [u8], stride: usize, x: u32, y: u32, label: &Label, ink: [u8; 3]) {
    for ly in 0..label.height {
        for lx in 0..label.width {
            let a = u32::from(label.alpha[(ly * label.width + lx) as usize]);
            if a == 0 {
                continue;
            }
            let i = (y + ly) as usize * stride + (x + lx) as usize * 4;
            if let Some(px) = canvas.get_mut(i..i + 4) {
                blend_ink(px, ink, a);
            }
        }
    }
}

/// Blits one already-rasterized glyph (see [`Labels::glyph`]) rotated by
/// `theta` around a tile's center `(cx, cy)`: its top row (ascent, small
/// row indices) sits at radius `r_inner`, closest to the circle, and it
/// extends further outward as the row index grows toward its baseline and
/// descent — the normal top-then-bottom reading order, just bent around
/// the circle instead of running straight. `theta = 0` is straight down
/// (6 o'clock); it increases counterclockwise, so a name's first glyph
/// sits at `theta = 0` and later ones climb the circle's right side.
#[allow(clippy::too_many_arguments)]
fn blit_rotated_glyph(
    canvas: &mut [u8],
    stride: usize,
    canvas_size: (u32, u32),
    center: (f32, f32),
    r_inner: f32,
    theta: f32,
    glyph: &Label,
    ink: [u8; 3],
) {
    let (canvas_w, canvas_h) = canvas_size;
    let (cx, cy) = center;
    if glyph.width == 0 || glyph.height == 0 {
        return;
    }
    let (sin_t, cos_t) = theta.sin_cos();
    let (ox, oy) = (sin_t, cos_t); // outward at theta
    let (tx, ty) = (cos_t, -sin_t); // tangent, left→right as theta increases
    let (gw, gh) = (glyph.width as f32, glyph.height as f32);
    // World = center + (lx - gw/2)*tangent + (r_inner + ly)*outward.
    let point = |lx: f32, ly: f32| {
        let lx_c = lx - gw / 2.0;
        let rr = r_inner + ly;
        (cx + lx_c * tx + rr * ox, cy + lx_c * ty + rr * oy)
    };
    // World-space bounding box of the rotated glyph, from its four corners.
    let corners = [
        point(0.0, 0.0),
        point(gw, 0.0),
        point(0.0, gh),
        point(gw, gh),
    ];
    let min_x = corners.iter().fold(f32::MAX, |a, &(x, _)| a.min(x));
    let max_x = corners.iter().fold(f32::MIN, |a, &(x, _)| a.max(x));
    let min_y = corners.iter().fold(f32::MAX, |a, &(_, y)| a.min(y));
    let max_y = corners.iter().fold(f32::MIN, |a, &(_, y)| a.max(y));
    let x0 = min_x.floor().max(0.0) as u32;
    let x1 = (max_x.ceil().max(0.0) as u32).min(canvas_w);
    let y0 = min_y.floor().max(0.0) as u32;
    let y1 = (max_y.ceil().max(0.0) as u32).min(canvas_h);
    for wy in y0..y1 {
        for wx in x0..x1 {
            let dx = wx as f32 + 0.5 - cx;
            let dy = wy as f32 + 0.5 - cy;
            // Inverse of the forward map above: project onto the
            // (tangent, outward) basis, which is orthonormal, so a dot
            // product suffices.
            let lx_c = dx * tx + dy * ty;
            let rr = dx * ox + dy * oy;
            let lx = lx_c + gw / 2.0;
            let ly = rr - r_inner;
            if lx < 0.0 || ly < 0.0 || lx >= gw || ly >= gh {
                continue;
            }
            let a = u32::from(glyph.alpha[(ly as u32 * glyph.width + lx as u32) as usize]);
            if a == 0 {
                continue;
            }
            let i = wy as usize * stride + wx as usize * 4;
            if let Some(px) = canvas.get_mut(i..i + 4) {
                blend_ink(px, ink, a);
            }
        }
    }
}

/// What the overlay shows, taken from the state once per frame, and the
/// size it needs.
struct Scene {
    /// The overlay settings as of the last update.
    settings: OverlaySettings,
    tiles: Vec<Tile>,
    /// Name rendering; `None` only if the embedded font failed to parse.
    labels: Option<Labels>,
    state: watch::Receiver<AppState>,
    frames: VideoFrames,
    cmd: mpsc::UnboundedSender<Command>,
    visible_frames: Vec<(Session, u64)>,
    reported: HashMap<Session, (u64, Instant)>,
    width: u32,
    height: u32,
    /// Pixels per layout pixel. The Windows window is sized in physical
    /// pixels and sets this from the monitor's DPI each frame; a Wayland
    /// compositor scales the surface itself (1).
    scale: f32,
    /// Each tile's current eased size multiplier, by session —
    /// carried across ticks so it can ease smoothly rather than snap;
    /// rebuilt fresh every [`Scene::update`], which also prunes anyone no
    /// longer shown in the overlay.
    anim: HashMap<Session, f32>,
    gpu: Option<gpu::Renderer>,
    glow: HashMap<Session, f32>,
    intensities: Vec<f32>,
    last_update: Instant,
    membership: Vec<(Session, String, bool)>,
    presented_frames: Option<Vec<(Session, u64)>>,
}

/// What [`Scene::update`] found.
struct Update {
    /// Something to show: overlay on and at least one tile.
    shown: bool,
    /// Position or tile size changed: the window is placed anew. (The
    /// Windows overlay places and sizes itself every frame.)
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    layout_changed: bool,
    /// The needed size changed.
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    size_changed: bool,
}

impl Scene {
    fn new(
        state: watch::Receiver<AppState>,
        frames: VideoFrames,
        cmd: mpsc::UnboundedSender<Command>,
    ) -> Scene {
        let settings = state.borrow().settings.overlay.clone();
        Scene {
            settings,
            tiles: Vec::new(),
            labels: Labels::new(),
            state,
            frames,
            cmd,
            visible_frames: Vec::new(),
            reported: HashMap::new(),
            width: 0,
            height: 0,
            scale: 1.0,
            anim: HashMap::new(),
            gpu: match gpu::Renderer::new() {
                Ok(renderer) => Some(renderer),
                Err(error) => {
                    tracing::info!(%error, "overlay uses software renderer");
                    None
                }
            },
            glow: HashMap::new(),
            intensities: Vec::new(),
            last_update: Instant::now(),
            membership: Vec::new(),
            presented_frames: None,
        }
    }

    /// Diameter of a camera tile's circle.
    fn camera_diameter(&self) -> u32 {
        let d = match self.settings.size {
            TileSize::Small => 120,
            TileSize::Medium => 240,
            TileSize::Large => 360,
        };
        scaled(d, self.scale)
    }

    /// Diameter of a voice-only plate's circle — smaller than a camera
    /// tile, closer to the small avatars elsewhere in the UI.
    fn plate_diameter(&self) -> u32 {
        let d = match self.settings.size {
            TileSize::Small => 72,
            TileSize::Medium => 144,
            TileSize::Large => 216,
        };
        scaled(d, self.scale)
    }

    /// The outside name label's font size, per tile size.
    fn label_px(&self) -> u32 {
        let px = match self.settings.size {
            TileSize::Small => 12,
            TileSize::Medium => 15,
            TileSize::Large => 18,
        };
        scaled(px, self.scale)
    }

    /// A tile's reserved slot: its circle plus its outside label ring, at
    /// the size it will ever need. Every tile reserves its *speaking*
    /// size — larger than it draws at rest — so the slot never has to
    /// resize as its drawn size eases between resting and speaking.
    fn tile_geom(&self, tile: &Tile) -> CircleGeom {
        let diameter = match tile.face {
            Face::Camera(_) => speak_diameter(self.camera_diameter()),
            Face::Plate => speak_diameter(self.plate_diameter()),
        };
        let mut geom = circle_geometry(diameter, self.label_px());
        if self.gpu.is_some() {
            let pad = scaled(gpu::HALO, self.scale);
            geom.half_w += pad;
            geom.outer_w += pad * 2;
            geom.outer_h += pad * 2;
        }
        geom
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

    /// Takes the tiles and settings from the state and works out the size.
    fn interval(&self) -> Duration {
        if self.gpu.is_some() {
            gpu::GPU_TICK
        } else {
            TICK
        }
    }

    fn update(&mut self) -> Update {
        let now = Instant::now();
        let dt = now.duration_since(self.last_update).as_secs_f32();
        self.last_update = now;
        // Only what a tile needs leaves the state: the whole `AppState`
        // (device list, user list, invite text) is not worth cloning at
        // 15 Hz.
        let (settings, peers) = {
            let state = self.state.borrow();
            let peers: Vec<(Session, String, bool, bool)> =
                overlay_peers(&state.peers, state.settings.overlay.voice_plates)
                    .into_iter()
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
        let settings_changed = self.settings != settings;
        self.settings = settings;

        // Camera tiles first, then the smaller plates; each group in name order.
        let frames = self.frames.latest();
        let seqs = self.frames.seqs();
        self.visible_frames = peers
            .iter()
            .filter(|(session, _, _, camera)| *camera && frames.iter().any(|(s, _)| s == session))
            .filter_map(|(session, _, _, _)| seqs.iter().find(|(s, _)| s == session).copied())
            .collect();
        let membership: Vec<_> = peers
            .iter()
            .map(|(s, n, _, c)| (*s, n.clone(), *c))
            .collect();
        if settings_changed || membership != self.membership {
            if let Some(renderer) = self.gpu.as_mut() {
                renderer.invalidate();
            }
            self.membership = membership;
        }
        // A fresh map each tick prunes members who left or are hidden.
        let mut anim = HashMap::with_capacity(self.anim.len());
        let mut glow = HashMap::new();
        self.intensities.clear();
        self.tiles = peers
            .into_iter()
            .map(|(session, name, speaking, camera)| {
                let scale = {
                    let target = if speaking { SPEAK_SCALE } else { REST_SCALE };
                    let prev = self.anim.get(&session).copied().unwrap_or(REST_SCALE);
                    let factor = if self.gpu.is_some() {
                        1.0 - (1.0 - SCALE_EASE).powf(dt / TICK.as_secs_f32())
                    } else {
                        SCALE_EASE
                    };
                    let next = ease(prev, target, factor);
                    anim.insert(session, next);
                    next
                };
                let intensity = fade(
                    self.glow.get(&session).copied().unwrap_or(0.0),
                    speaking && self.settings.speaking_frame,
                    dt,
                );
                glow.insert(session, intensity);
                self.intensities.push(if self.settings.speaking_frame {
                    intensity
                } else {
                    0.0
                });
                Tile {
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
                    dim: self.settings.dim_non_speaking && !speaking,
                    scale,
                }
            })
            .collect();
        self.anim = anim;
        self.glow = glow;

        let shown = self.settings.enabled && !self.tiles.is_empty();
        if !shown {
            return Update {
                shown,
                layout_changed,
                size_changed: false,
            };
        }
        let n = self.tiles.len() as u32;
        let geoms: Vec<CircleGeom> = self.tiles.iter().map(|t| self.tile_geom(t)).collect();
        // Each tile reserves a fixed slot sized for the worst-case label
        // arc, not its actual name's length, so a row/column stays aligned
        // regardless of who has a long name (same principle as before:
        // only the text itself gets truncated with „…").
        let gap = scaled(GAP, self.scale);
        let (width, height) = match self.effective_expand() {
            Expand::Vertical => (
                geoms.iter().map(|g| g.outer_w).max().unwrap_or(0),
                geoms.iter().map(|g| g.outer_h).sum::<u32>() + n.saturating_sub(1) * gap,
            ),
            Expand::Horizontal => (
                geoms.iter().map(|g| g.outer_w).sum::<u32>() + n.saturating_sub(1) * gap,
                geoms.iter().map(|g| g.outer_h).max().unwrap_or(0),
            ),
        };
        let size_changed = width != self.width || height != self.height;
        self.width = width;
        self.height = height;
        Update {
            shown,
            layout_changed,
            size_changed,
        }
    }

    /// Draws the tiles into `canvas`, premultiplied BGRA of
    /// `self.width × self.height`.
    fn draw(&mut self, canvas: &mut [u8]) -> bool {
        let geometry = Geometry {
            width: self.width,
            height: self.height,
            camera_diameter: self.camera_diameter(),
            plate_diameter: self.plate_diameter(),
            name_px: self.label_px(),
            horizontal: self.effective_expand() == Expand::Horizontal,
            bottom: self.anchored_bottom(),
            speaking_frame: self.settings.speaking_frame,
            frame_color: self.settings.frame_color,
            gap: scaled(GAP, self.scale),
        };

        if let Some(mut renderer) = self.gpu.take() {
            let result = renderer.draw(
                &mut gpu::RenderScene {
                    geometry,
                    scale: self.scale,
                    tiles: &self.tiles,
                    intensities: &self.intensities,
                    labels: &mut self.labels,
                    visible_frames: &self.visible_frames,
                },
                canvas,
            );
            match result {
                Ok(frames) => {
                    self.gpu = Some(renderer);
                    self.presented_frames = frames;
                    return self.presented_frames.is_some();
                }
                Err(error) => {
                    tracing::warn!(%error, "overlay falling back to software");
                    self.presented_frames = None;
                    // Recompute native buffer dimensions on the next tick.
                    return false;
                }
            }
        }
        self.presented_frames = Some(self.visible_frames.clone());
        compose(canvas, &geometry, &self.tiles, self.labels.as_mut());
        true
    }

    /// Called only after the platform accepts a frame for presentation.
    fn report_displayed(&mut self) {
        let now = Instant::now();
        self.reported
            .retain(|session, _| self.visible_frames.iter().any(|(s, _)| s == session));
        for (session, seq) in self.presented_frames.as_deref().unwrap_or(&[]) {
            if new_display_report(self.reported.get(session).copied(), *seq, now) {
                self.reported.insert(*session, (*seq, now));
                let _ = self.cmd.send(Command::VideoDisplayed(*session));
            }
        }
    }
}

/// Visible members in camera-first, then name order. Speaking never changes
/// membership, so a voice-only conversation keeps a stable layout too.
fn overlay_peers(peers: &[Peer], voice_plates: bool) -> Vec<&Peer> {
    let mut shown: Vec<_> = peers
        .iter()
        .filter(|p| p.info.flags.camera_on() || voice_plates)
        .collect();
    shown.sort_by(|a, b| {
        b.info
            .flags
            .camera_on()
            .cmp(&a.info.flags.camera_on())
            .then_with(|| a.info.name.cmp(&b.info.name))
    });
    shown
}

fn new_display_report(last: Option<(u64, Instant)>, seq: u64, now: Instant) -> bool {
    last.is_none_or(|(last_seq, at)| {
        last_seq != seq && now.duration_since(at) >= Duration::from_secs(1)
    })
}

/// `px` layout pixels at `scale` physical pixels each.
fn scaled(px: u32, scale: f32) -> u32 {
    (px as f32 * scale).round() as u32
}

/// Physical pixels per layout pixel at `dpi` (Windows: 96 = 100 %); an
/// unknown DPI (0) counts as 100 %.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn dpi_scale(dpi: u32) -> f32 {
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

/// What decides the window's position and tile size: a change here needs
/// the window placed anew.
fn layout(s: &OverlaySettings) -> (Corner, [u32; 2], TileSize, Expand) {
    (s.corner, s.offset, s.size, s.expand)
}

/// A circle's border ring and outside label band, in continuous pixels —
/// everything [`compose`] needs to actually paint a tile at whatever size
/// it's currently drawn at (see [`Tile::scale`]).
#[derive(Clone, Copy)]
struct Radii {
    r: f32,
    r_outer: f32,
    r_label_inner: f32,
    r_label_outer: f32,
}

/// The border ring around a circle of `diameter`, and the label band
/// around that sized for `name_px` text.
fn radii(diameter: f32, name_px: u32) -> Radii {
    let r = diameter / 2.0;
    let r_outer = r + BORDER as f32;
    let thickness = (name_px + 2 * LABEL_PAD_V) as f32;
    let r_label_inner = r_outer + LABEL_GAP as f32;
    let r_label_outer = r_label_inner + thickness;
    Radii {
        r,
        r_outer,
        r_label_inner,
        r_label_outer,
    }
}

/// A tile's reserved slot: its round shape and outside label ring at the
/// largest size it will ever draw, derived purely from its circle's
/// diameter and the label font size, so the sizing pass ([`Scene::update`])
/// and the draw pass ([`compose`]) can't drift apart. A tile may draw
/// smaller than this (see [`Tile::scale`]), but never larger, so the slot
/// never needs to change size.
#[derive(Clone, Copy)]
struct CircleGeom {
    radii: Radii,
    /// Half the outer bounding box's width.
    half_w: u32,
    outer_w: u32,
    outer_h: u32,
}

fn circle_geometry(diameter: u32, name_px: u32) -> CircleGeom {
    let radii = radii(diameter as f32, name_px);
    // The label band can bulge sideways past the circle itself at wide
    // angles, all to the right since the label starts at 6 o'clock and
    // only ever sweeps counterclockwise from there; the tile's reserved
    // box (kept symmetric, so a tile never has to move) must be wide
    // enough for that on its wider side, sized for the widest angle a name
    // is ever allowed (truncation keeps shorter names from needing it, but
    // the slot stays constant). Two shapes can reach furthest right: the
    // flat band itself (widest at 90° if the sweep passes it, else at its
    // end angle) and its rounded end cap (a circle of radius `half_thick`,
    // half the band's thickness, centered on the band's midline at the end
    // angle) — reserve whichever of the two reaches further.
    let half_thick = (radii.r_label_outer - radii.r_label_inner) / 2.0;
    let mid_r = radii.r_label_inner + half_thick;
    let flat_bulge = radii.r_label_outer * MAX_LABEL_ANGLE.min(std::f32::consts::FRAC_PI_2).sin();
    let cap_bulge = mid_r * MAX_LABEL_ANGLE.sin() + half_thick;
    let half_w = radii.r_outer.max(flat_bulge).max(cap_bulge).ceil() as u32;
    CircleGeom {
        radii,
        half_w,
        outer_w: half_w * 2,
        outer_h: (radii.r_outer + radii.r_label_outer).ceil() as u32,
    }
}

/// Samples `rgba` (announced `fw × fh`) as a centered square crop into a
/// circle of radius `r`, so a 4:3 frame doesn't stretch: the short side is
/// kept whole and the long one cropped symmetrically — the same idea
/// `ui/widgets.rs::avatar` uses for the small round avatars. `dx`, `dy` are
/// the destination pixel's offset from the circle's center, in `[-r, r]`.
/// Bounds-checked: a frame shorter than its announced size (a partial
/// decode) must not take the client down.
fn sample_camera_circle(
    fw: usize,
    fh: usize,
    rgba: &[u8],
    dx: f32,
    dy: f32,
    r: f32,
    mul: u32,
) -> [u8; 4] {
    let side = fw.min(fh).max(1);
    let u = ((dx + r) / (2.0 * r)).clamp(0.0, 0.999_9);
    let v = ((dy + r) / (2.0 * r)).clamp(0.0, 0.999_9);
    let sx = (fw - side) / 2 + (u * side as f32) as usize;
    let sy = (fh - side) / 2 + (v * side as f32) as usize;
    let i = (sy * fw + sx) * 4;
    match rgba.get(i..i + 3) {
        Some(&[r8, g8, b8]) => [
            ((u32::from(b8) * mul) >> 8) as u8,
            ((u32::from(g8) * mul) >> 8) as u8,
            ((u32::from(r8) * mul) >> 8) as u8,
            0xff,
        ],
        _ => [0x20, 0x20, 0x20, 0xff],
    }
}

/// The name actually shown for a tile — cut with „…" so it fits `max_angle`
/// of arc starting at 6 o'clock and sweeping counterclockwise — and the
/// angle it actually needs there. Empty (angle `0.0`) for an empty name or
/// one that cuts down to nothing, so [`compose`] knows to paint no paper
/// backdrop at all rather than a bare, textless tag.
fn label_extent(
    labels: &mut Labels,
    name: &str,
    px: u32,
    r_inner: f32,
    max_angle: f32,
) -> (String, f32) {
    let max_arc = max_angle * r_inner;
    let shown = labels.cut(name, px, max_arc);
    if shown.is_empty() {
        return (shown, 0.0);
    }
    let angle = (labels.advance(&shown, px) / r_inner).min(max_angle);
    (shown, angle)
}

/// Draws `shown` (already cut to fit, see [`label_extent`]) curved from
/// straight down (`(cx, cy)`, 6 o'clock) counterclockwise, its ascent
/// following the circle at radius `r_inner`, in `ink` — the tile's outside
/// label, replacing the old strip drawn over the picture. Kerning is
/// dropped (each glyph is its own rigid rotation), and layout uses exact
/// arc length (`theta = s / r_inner`), not an approximation.
#[allow(clippy::too_many_arguments)]
fn draw_curved_label(
    canvas: &mut [u8],
    stride: usize,
    canvas_w: u32,
    canvas_h: u32,
    cx: f32,
    cy: f32,
    r_inner: f32,
    shown: &str,
    px: u32,
    ink: [u8; 3],
    labels: &mut Labels,
) {
    let mut caret = 0.0f32;
    for c in shown.chars() {
        let glyph_w = labels.advance(&c.to_string(), px);
        let theta = (caret + glyph_w / 2.0) / r_inner;
        caret += glyph_w;
        let glyph = labels.glyph(c, px);
        blit_rotated_glyph(
            canvas,
            stride,
            (canvas_w, canvas_h),
            (cx, cy),
            r_inner,
            theta,
            glyph,
            ink,
        );
    }
}

/// Where and how big the tiles are; everything [`compose`] needs besides
/// the tiles themselves.
struct Geometry {
    width: u32,
    height: u32,
    camera_diameter: u32,
    plate_diameter: u32,
    name_px: u32,
    horizontal: bool,
    /// The surface hangs from its bottom edge: a row lines up there.
    bottom: bool,
    speaking_frame: bool,
    frame_color: [u8; 3],
    /// Spacing between adjacent tiles, already scaled — see [`GAP`].
    gap: u32,
}

/// Draws the tiles into `canvas` (premultiplied BGRA, `width × height`):
/// a round, opaque border and fill — the picture, or for a plate an
/// initials circle — and the name curved along the outside of the circle.
/// Every pixel access is bounds-checked — a frame that does not match its
/// announced size must not take the client down.
fn compose(canvas: &mut [u8], g: &Geometry, tiles: &[Tile], mut labels: Option<&mut Labels>) {
    canvas.fill(0);
    let stride = g.width as usize * 4;
    let mut x0 = 0u32;
    let mut y0 = 0u32;
    for tile in tiles {
        let diameter = match tile.face {
            Face::Camera(_) => g.camera_diameter,
            Face::Plate => g.plate_diameter,
        };
        // The reserved slot (fixed: sized for every tile's *speaking*
        // size, see `Scene::tile_geom`) decides position and the loop
        // bounds below; `draw` (animated via `tile.scale`) decides what's
        // actually painted, from the same fixed center — so a tile grows
        // and shrinks concentrically without moving or resizing its slot.
        let reserve_diameter = speak_diameter(diameter);
        let geom = circle_geometry(reserve_diameter, g.name_px);
        let draw = radii(diameter as f32 * tile.scale, g.name_px);
        let outer_w = geom.outer_w;
        let outer_h = geom.outer_h;
        // In a row the tiles line up at the edge the surface hangs from; in
        // a column each tile centers within the column's width — camera
        // and plate circles are different sizes now, unlike the old
        // uniform-width rectangles.
        let ty = if g.horizontal && g.bottom {
            g.height.saturating_sub(outer_h)
        } else {
            y0
        };
        let tx = if g.horizontal {
            x0
        } else {
            (g.width.saturating_sub(outer_w)) / 2
        };
        let cx = tx as f32 + geom.half_w as f32;
        let cy = ty as f32 + geom.radii.r_outer;
        let [fr, fg, fb] = g.frame_color;
        let border = if tile.speaking && g.speaking_frame {
            [fb, fg, fr, 0xff] // B G R A premultiplied
        } else {
            [0x30, 0x30, 0x30, 0xff]
        };
        let mul = if tile.dim { DIM_MUL } else { 256 };
        let dim = |c: [u8; 4]| {
            [
                ((u32::from(c[0]) * mul) >> 8) as u8,
                ((u32::from(c[1]) * mul) >> 8) as u8,
                ((u32::from(c[2]) * mul) >> 8) as u8,
                c[3],
            ]
        };
        let paper = dim(PAPER_FILL);
        let ink = {
            let [b, g, r] = LABEL_INK;
            let [b, g, r, _] = dim([b, g, r, 0xff]);
            [b, g, r]
        };

        // The name actually shown, and the arc it sweeps from 6 o'clock —
        // computed once so Pass 1's backdrop and Pass 3's glyphs can never
        // disagree about how much room the name needs. Neither draws
        // anything (backdrop included) for an empty or unrenderable name.
        let (shown, sweep) = match labels.as_deref_mut() {
            Some(labels) => label_extent(
                labels,
                &tile.name,
                g.name_px,
                draw.r_label_inner,
                MAX_LABEL_ANGLE,
            ),
            None => (String::new(), 0.0),
        };
        // The label band's midline radius and half-thickness, and the
        // rounded caps at its two ends (see `circle_geometry`): full
        // circles of that half-thickness, centered on the midline at
        // `theta = 0` (6 o'clock, where the name starts) and `theta =
        // sweep` (where it ends) — offsets from the tile's own center,
        // same convention as `dx`/`dy` below.
        let mid_r = (draw.r_label_inner + draw.r_label_outer) / 2.0;
        let half_thick = (draw.r_label_outer - draw.r_label_inner) / 2.0;
        let (sin_e, cos_e) = sweep.sin_cos();
        let cap_start = (0.0f32, mid_r);
        let cap_end = (mid_r * sin_e, mid_r * cos_e);

        // Pass 1: the circle (fill + border ring) and the paper label
        // backdrop behind it, flush against the border ring and shaped
        // like a rounded tag — flat along the band, rounded where it
        // starts (6 o'clock) and ends; everything else stays transparent,
        // which is what makes the tile round instead of a square with a
        // circle painted on it.
        for y in 0..outer_h {
            for x in 0..outer_w {
                // `cx`/`cy` are absolute canvas coordinates (they already
                // include `tx`/`ty`); `x`/`y` here are tile-local, so they
                // need the same offset before comparing against them.
                let dx = (tx + x) as f32 + 0.5 - cx;
                let dy = (ty + y) as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let px = if dist <= draw.r {
                    Some(match &tile.face {
                        Face::Camera(Some((fw, fh, rgba))) => {
                            sample_camera_circle(*fw, *fh, rgba, dx, dy, draw.r, mul)
                        }
                        Face::Camera(None) => dim([0x20, 0x20, 0x20, 0xff]),
                        Face::Plate => dim([0x1c, 0x1c, 0x1c, 0xff]),
                    })
                } else if dist <= draw.r_outer {
                    Some(border)
                } else if sweep > 0.0 && dist > draw.r_label_inner && dist <= draw.r_label_outer {
                    let theta = dx.atan2(dy);
                    let in_band = if theta >= 0.0 && theta <= sweep {
                        true
                    } else {
                        let cap = if theta < 0.0 { cap_start } else { cap_end };
                        let (cdx, cdy) = (dx - cap.0, dy - cap.1);
                        cdx * cdx + cdy * cdy <= half_thick * half_thick
                    };
                    in_band.then_some(paper)
                } else {
                    None
                };
                if let Some(px) = px {
                    let i = (ty + y) as usize * stride + (tx + x) as usize * 4;
                    if let Some(dst) = canvas.get_mut(i..i + 4) {
                        dst.copy_from_slice(&px);
                    }
                }
            }
        }

        // Pass 2: a plate's initials, centered and straight inside its
        // circle — the same no-picture fallback the small avatars
        // elsewhere use (`ui/widgets.rs::avatar`).
        if matches!(tile.face, Face::Plate) {
            if let Some(labels) = labels.as_deref_mut() {
                let initials_px = (diameter as f32 * 0.37).round().max(1.0) as u32;
                let text = crate::ui::widgets::initials(&tile.name);
                let label = labels.get(&text, initials_px, diameter);
                let lx = (cx - label.width as f32 / 2.0).round().max(0.0) as u32;
                let ly = (cy - label.height as f32 / 2.0).round().max(0.0) as u32;
                let [b, g, r, _] = dim([255; 4]);
                blend_label(canvas, stride, lx, ly, label, [b, g, r]);
            }
        }

        // Pass 3: the name, curved along the outside of the circle, on top
        // of the paper backdrop Pass 1 just drew for it.
        if sweep > 0.0 {
            if let Some(labels) = labels.as_deref_mut() {
                draw_curved_label(
                    canvas,
                    stride,
                    g.width,
                    g.height,
                    cx,
                    cy,
                    draw.r_label_inner,
                    &shown,
                    g.name_px,
                    ink,
                    labels,
                );
            }
        }

        if g.horizontal {
            x0 += outer_w + g.gap;
        } else {
            y0 += outer_h + g.gap;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_video_members_stay_visible_until_hidden_or_gone() {
        use icu_proto::{UserFlags, UserInfo};
        let peer = |session, name: &str, flags| {
            Peer::new(
                UserInfo {
                    session,
                    user_id: u32::from(session),
                    name: name.into(),
                    flags: UserFlags(flags),
                },
                100.0,
            )
        };
        let mut peers = vec![
            peer(1, "Zoe", 0),
            peer(2, "Anna", UserFlags::SPEAKING),
            peer(3, "Carla", UserFlags::CAMERA_ON),
            peer(4, "Ben", UserFlags::CAMERA_ON),
        ];
        let sessions = |peers: &[Peer], plates| {
            overlay_peers(peers, plates)
                .into_iter()
                .map(|p| p.info.session)
                .collect::<Vec<_>>()
        };
        assert_eq!(sessions(&peers, true), [4, 3, 2, 1]);
        assert_eq!(sessions(&peers, false), [4, 3]);
        for p in &mut peers {
            p.speaking = !p.speaking;
        }
        assert_eq!(sessions(&peers, true), [4, 3, 2, 1]);
        peers[0].info.flags = UserFlags(UserFlags::CAMERA_ON);
        assert_eq!(sessions(&peers, true), [4, 3, 1, 2]);
        assert_eq!(sessions(&peers, false), [4, 3, 1]);
        for p in &mut peers {
            p.info.flags = UserFlags(0);
            p.speaking = false;
        }
        assert_eq!(sessions(&peers, true), [2, 4, 3, 1]);
        assert!(sessions(&peers, false).is_empty());
        peers.remove(1);
        assert_eq!(sessions(&peers, true), [4, 3, 1]);
        peers.clear();
        assert!(sessions(&peers, true).is_empty());
    }

    #[test]
    fn frozen_overlay_frame_does_not_renew_a_viewer_report() {
        let now = Instant::now();
        let old = now - Duration::from_secs(4);
        assert!(!new_display_report(Some((7, old)), 7, now));
        assert!(new_display_report(Some((7, old)), 8, now));
        assert!(!new_display_report(Some((7, now)), 8, now));
    }

    const CAMERA_D: u32 = 120;
    const PLATE_D: u32 = 72;
    const NAME_PX: u32 = 12;

    fn geometry(tiles: &[Tile], horizontal: bool) -> Geometry {
        // Mirrors `compose`'s own reservation math: every tile's slot
        // is sized for its *speaking* diameter, never its resting one.
        let geoms: Vec<CircleGeom> = tiles
            .iter()
            .map(|t| {
                let d = match t.face {
                    Face::Camera(_) => speak_diameter(CAMERA_D),
                    Face::Plate => speak_diameter(PLATE_D),
                };
                circle_geometry(d, NAME_PX)
            })
            .collect();
        let n = tiles.len() as u32;
        let (width, height) = if horizontal {
            (
                geoms.iter().map(|g| g.outer_w).sum::<u32>() + n.saturating_sub(1) * GAP,
                geoms.iter().map(|g| g.outer_h).max().unwrap(),
            )
        } else {
            (
                geoms.iter().map(|g| g.outer_w).max().unwrap(),
                geoms.iter().map(|g| g.outer_h).sum::<u32>() + n.saturating_sub(1) * GAP,
            )
        };
        Geometry {
            width,
            height,
            camera_diameter: CAMERA_D,
            plate_diameter: PLATE_D,
            name_px: NAME_PX,
            horizontal,
            bottom: true,
            speaking_frame: true,
            frame_color: [0x2e, 0x9b, 0xff],
            gap: GAP,
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
                scale: REST_SCALE,
            },
            Tile {
                face: Face::Plate,
                name: "Eva-Maria Schulte-Wiedenbrück von Hohenfels".into(),
                speaking: true,
                dim: false,
                scale: SPEAK_SCALE,
            },
            Tile {
                face: Face::Plate,
                name: "Frank".into(),
                speaking: false,
                dim: true,
                scale: REST_SCALE,
            },
        ]
    }

    /// At 150 % and 200 % display scaling the tiles, plates and names are
    /// as large on screen as at 100 %, not ⅔ or ½ of it.
    #[test]
    fn sizes_follow_the_display_scaling() {
        assert_eq!(dpi_scale(96), 1.0);
        assert_eq!(dpi_scale(144), 1.5);
        assert_eq!(dpi_scale(192), 2.0);
        assert_eq!(dpi_scale(0), 1.0);
        let at = |scale| [120, 72, 12, 20].map(|px| scaled(px, scale));
        assert_eq!(at(1.0), [120, 72, 12, 20]);
        assert_eq!(at(1.5), [180, 108, 18, 30]);
        assert_eq!(at(2.0), [240, 144, 24, 40]);
        assert_eq!(scaled(15, 1.25), 19);
    }

    /// Camera tile and plate both carry their name, a long one cut with
    /// „…" curving outside the tile; a picture shorter than announced does
    /// not panic; both layouts stay inside the buffer; tiles are round
    /// (transparent corners) with an opaque fill (no more see-through
    /// background).
    #[test]
    fn names_are_drawn_and_nothing_panics() {
        let mut labels = Labels::new().expect("embedded font");
        for horizontal in [false, true] {
            let tiles = tiles();
            let g = geometry(&tiles, horizontal);
            let mut canvas = vec![0u8; (g.width * g.height * 4) as usize];
            compose(&mut canvas, &g, &tiles, Some(&mut labels));
            let alpha_at =
                |x: u32, y: u32| canvas[(y as usize * g.width as usize + x as usize) * 4 + 3];
            for (x, y) in [
                (0, 0),
                (g.width - 1, 0),
                (0, g.height - 1),
                (g.width - 1, g.height - 1),
            ] {
                assert_eq!(
                    alpha_at(x, y),
                    0,
                    "canvas corner ({x},{y}) should be transparent — tiles are round, not square"
                );
            }
            let opaque = canvas
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] == 0xff)
                .count();
            assert!(
                opaque > 100,
                "expected a solidly opaque tile fill, found {opaque} fully-opaque pixels"
            );
            // White text pixels exist (only the plate's initials, drawn on
            // its dark fill, are near-white — the curved names are ink).
            let white = canvas
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[0] > 230 && p[1] > 230 && p[2] > 230)
                .count();
            assert!(white > 40, "no plate initials drawn ({white} white pixels)");
            // The paper backdrop is drawn (flush against each tile, sized
            // to its own name) and the curved name is ink on top of it —
            // distinct from the neutral grays used elsewhere on a tile.
            let paper = canvas
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3] == PAPER_FILL[..3])
                .count();
            assert!(
                paper > 40,
                "no paper label backdrop drawn ({paper} matching pixels)"
            );
            let ink = canvas
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[2] > p[1] && p[1] > p[0] && p[2] < 80)
                .count();
            assert!(ink > 20, "no ink name text drawn ({ink} matching pixels)");
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

    /// A name's curved label starts at 6 o'clock (straight down from the
    /// tile's center) and sweeps counterclockwise from there, climbing the
    /// circle's right side — never to the left of center.
    #[test]
    fn label_starts_at_six_and_sweeps_counterclockwise() {
        let mut labels = Labels::new().expect("embedded font");
        let tiles = vec![Tile {
            face: Face::Camera(None),
            name: "Anna".into(),
            speaking: false,
            dim: false,
            scale: REST_SCALE,
        }];
        let g = geometry(&tiles, true);
        let mut canvas = vec![0u8; (g.width * g.height * 4) as usize];
        compose(&mut canvas, &g, &tiles, Some(&mut labels));
        let geom = circle_geometry(speak_diameter(CAMERA_D), NAME_PX);
        let cx = geom.half_w as f32; // the lone tile sits at x0 == 0
        let is_ink = |p: &[u8]| p[2] > p[1] && p[1] > p[0] && p[2] < 80;
        let (mut ink_left, mut ink_right) = (0u32, 0u32);
        for y in 0..g.height {
            for x in 0..g.width {
                let p = &canvas[(y as usize * g.width as usize + x as usize) * 4..][..4];
                if is_ink(p) {
                    if (x as f32) < cx {
                        ink_left += 1;
                    } else {
                        ink_right += 1;
                    }
                }
            }
        }
        assert!(
            ink_left <= 2,
            "name ink drawn left of 6 o'clock ({ink_left} px) — must sweep counterclockwise only"
        );
        assert!(
            ink_right > 20,
            "no name ink drawn right of center ({ink_right} px)"
        );
    }

    /// `ease` closes the gap to its target every step, in both directions,
    /// without ever overshooting, and is a no-op once already there —
    /// the property a speaking-size animation that must never snap or
    /// bounce depends on.
    #[test]
    fn ease_smooths_toward_target_without_overshoot() {
        let mut v = REST_SCALE;
        let mut prev = v;
        for _ in 0..30 {
            v = ease(v, SPEAK_SCALE, SCALE_EASE);
            assert!(v >= prev, "grew backward: {prev} -> {v}");
            assert!(v <= SPEAK_SCALE + 1e-6, "overshot the target: {v}");
            prev = v;
        }
        assert!((v - SPEAK_SCALE).abs() < 0.01, "never converged: {v}");

        let mut v = SPEAK_SCALE;
        let mut prev = v;
        for _ in 0..30 {
            v = ease(v, REST_SCALE, SCALE_EASE);
            assert!(v <= prev, "shrank backward: {prev} -> {v}");
            assert!(v >= REST_SCALE - 1e-6, "overshot the target: {v}");
            prev = v;
        }
        assert!((v - REST_SCALE).abs() < 0.01, "never converged: {v}");

        assert_eq!(ease(SPEAK_SCALE, SPEAK_SCALE, SCALE_EASE), SPEAK_SCALE);
    }

    /// A speaking-sized tile draws visibly larger than a resting one, but
    /// both reserve the exact same slot — the animation must never resize
    /// or clip against the overlay window.
    #[test]
    fn scale_changes_the_drawn_circle_but_not_its_slot() {
        let mut labels = Labels::new().expect("embedded font");
        for camera in [false, true] {
            let tile = |scale: f32| {
                vec![Tile {
                    face: if camera {
                        Face::Camera(None)
                    } else {
                        Face::Plate
                    },
                    name: "Anna".into(),
                    speaking: scale > REST_SCALE,
                    dim: false,
                    scale,
                }]
            };
            let (rest, speak) = (tile(REST_SCALE), tile(SPEAK_SCALE));
            let (g_rest, g_speak) = (geometry(&rest, false), geometry(&speak, false));
            assert_eq!(
                (g_rest.width, g_rest.height),
                (g_speak.width, g_speak.height),
                "the reserved slot must not depend on the tile's current scale"
            );

            let max_opaque_row_span = |canvas: &[u8], width: u32, height: u32| {
                (0..height)
                    .map(|y| {
                        (0..width)
                            .filter(|&x| {
                                canvas[(y as usize * width as usize + x as usize) * 4 + 3] == 0xff
                            })
                            .count()
                    })
                    .max()
                    .unwrap_or(0)
            };
            let mut canvas_rest = vec![0u8; (g_rest.width * g_rest.height * 4) as usize];
            compose(&mut canvas_rest, &g_rest, &rest, Some(&mut labels));
            let mut canvas_speak = vec![0u8; (g_speak.width * g_speak.height * 4) as usize];
            compose(&mut canvas_speak, &g_speak, &speak, Some(&mut labels));

            let span_rest = max_opaque_row_span(&canvas_rest, g_rest.width, g_rest.height);
            let span_speak = max_opaque_row_span(&canvas_speak, g_speak.width, g_speak.height);
            assert!(
                span_speak > span_rest,
                "speaking tile should draw wider: rest {span_rest}, speaking {span_speak}"
            );
        }
    }

    #[test]
    fn silent_plate_dims_its_fill_initials_and_name() {
        let mut labels = Labels::new().unwrap();
        let mut tiles = vec![Tile {
            face: Face::Plate,
            name: "Anna".into(),
            speaking: false,
            dim: false,
            scale: REST_SCALE,
        }];
        let g = geometry(&tiles, false);
        let mut bright = vec![0; (g.width * g.height * 4) as usize];
        let mut dimmed = bright.clone();
        compose(&mut bright, &g, &tiles, Some(&mut labels));
        tiles[0].dim = true;
        compose(&mut dimmed, &g, &tiles, Some(&mut labels));
        for color in [
            [28, 28, 28],
            [255, 255, 255],
            PAPER_FILL[..3].try_into().unwrap(),
        ] {
            let mut count = 0;
            for (a, b) in bright
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dimmed.as_chunks::<4>().0.iter())
            {
                if a[..3] == color {
                    assert_eq!(b[..3], color.map(|c| c / 2));
                    assert_eq!(b[3], a[3]);
                    count += 1;
                }
            }
            assert!(count > 10, "missing fill, initials or paper: {color:?}");
        }
    }

    /// However a tile is currently scaled, it never draws past the
    /// slot reserved for its speaking size — the invariant that keeps the
    /// animation from ever resizing the overlay window or clipping.
    #[test]
    fn speaking_size_never_exceeds_its_reserved_slot() {
        for base in [72u32, 120, 144, 216, 240, 360] {
            for dpi in [1.0, 1.25, 1.5, 2.0] {
                let base = scaled(base, dpi);
                let name_px = scaled(NAME_PX, dpi);
                let reserved = circle_geometry(speak_diameter(base), name_px);
                for &scale in &[REST_SCALE, 1.05, SPEAK_SCALE] {
                    let drawn = radii(base as f32 * scale, name_px);
                    assert!(
                        drawn.r_outer <= reserved.radii.r_outer + 1e-3,
                        "base {base} scale {scale}: drawn r_outer {} > reserved {}",
                        drawn.r_outer,
                        reserved.radii.r_outer
                    );
                    assert!(
                        drawn.r_label_outer <= reserved.radii.r_label_outer + 1e-3,
                        "base {base} scale {scale}: drawn r_label_outer {} > reserved {}",
                        drawn.r_label_outer,
                        reserved.radii.r_label_outer
                    );
                }
            }
        }
        assert_eq!(speak_diameter(100), (100.0 * SPEAK_SCALE).ceil() as u32);
    }
}
