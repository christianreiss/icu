//! Speaking overlay (`Client/IMPLEMENTATION.md` §9): a click-through window
//! above everything that shows, for everyone whose camera is on, a camera
//! tile with the name, and for everyone without one a name plate while they
//! talk — so the overlay answers „wer spricht?" in a voice-only round too.
//!
//! What is shown and how it is drawn (premultiplied BGRA, 15 fps) lives
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
    time::Duration,
};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use icu_proto::Session;
use tokio::sync::watch;

use crate::{
    config::{Corner, Expand, OverlaySettings, TileSize},
    engine::{AppState, VideoFrames},
};

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
use wayland as platform;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use self::windows as platform;

const GAP: u32 = 8;
const BORDER: u32 = 3;
/// Gap between a tile's border ring and its outside name-label band.
const LABEL_GAP: u32 = 4;
/// Vertical padding inside the label band, above/below the glyphs' ascent
/// span, that sets the band's radial thickness.
const LABEL_PAD_V: u32 = 4;
/// Half the total arc a name may curve across before it's cut with „…" —
/// past this the letters would start reading upside down near the ends.
const MAX_LABEL_HALF_ANGLE: f32 = 62.0 * std::f32::consts::PI / 180.0;
/// Extra angle the opaque label band extends past the text itself, so
/// antialiased glyph edges never poke out past an opaque backing.
const LABEL_BAND_PAD_ANGLE: f32 = 6.0 * std::f32::consts::PI / 180.0;
/// Brightness multiplier (out of 256) applied to dimmed, non-speaking tiles.
const DIM_MUL: u32 = 128;
/// Frame interval: 15 fps.
const TICK: Duration = Duration::from_millis(1000 / 15);

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
    ) -> Result<OverlayHandle, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("icu-overlay".into())
            .spawn(move || platform::run(Scene::new(state, frames), stop_t, ready_tx))
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

/// Blends white into one premultiplied BGRA pixel by coverage `a` (0-255) —
/// shared by the straight `blend_label` strip blit and the curved outside
/// label's rotated glyph blit.
fn blend_white(dst: &mut [u8], a: u32) {
    for c in dst.iter_mut() {
        *c = ((255 * a + u32::from(*c) * (255 - a)) / 255) as u8;
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
            if let Some(px) = canvas.get_mut(i..i + 4) {
                blend_white(px, a);
            }
        }
    }
}

/// Blits one already-rasterized glyph (see [`Labels::glyph`]) rotated by
/// `theta` around a tile's center `(cx, cy)`: its top row (ascent, small
/// row indices) sits at radius `r_inner`, closest to the circle, and it
/// extends further outward as the row index grows toward its baseline and
/// descent — the normal top-then-bottom reading order, just bent around
/// the circle instead of running straight. `theta = 0` is straight down;
/// it increases clockwise, matching left→right reading.
fn blit_rotated_glyph(
    canvas: &mut [u8],
    stride: usize,
    canvas_size: (u32, u32),
    center: (f32, f32),
    r_inner: f32,
    theta: f32,
    glyph: &Label,
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
                blend_white(px, a);
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
    width: u32,
    height: u32,
    /// Pixels per layout pixel. The Windows window is sized in physical
    /// pixels and sets this from the monitor's DPI each frame; a Wayland
    /// compositor scales the surface itself (1).
    scale: f32,
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
    fn new(state: watch::Receiver<AppState>, frames: VideoFrames) -> Scene {
        let settings = state.borrow().settings.overlay.clone();
        Scene {
            settings,
            tiles: Vec::new(),
            labels: Labels::new(),
            state,
            frames,
            width: 0,
            height: 0,
            scale: 1.0,
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

    /// A tile's full round shape: its circle plus its outside label ring.
    fn tile_geom(&self, tile: &Tile) -> CircleGeom {
        let diameter = match tile.face {
            Face::Camera(_) => self.camera_diameter(),
            Face::Plate => self.plate_diameter(),
        };
        circle_geometry(diameter, self.label_px())
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
    fn update(&mut self) -> Update {
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
        let (width, height) = match self.effective_expand() {
            Expand::Vertical => (
                geoms.iter().map(|g| g.outer_w).max().unwrap_or(0),
                geoms.iter().map(|g| g.outer_h).sum::<u32>() + n.saturating_sub(1) * GAP,
            ),
            Expand::Horizontal => (
                geoms.iter().map(|g| g.outer_w).sum::<u32>() + n.saturating_sub(1) * GAP,
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
    fn draw(&mut self, canvas: &mut [u8]) {
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
        };
        compose(canvas, &geometry, &self.tiles, self.labels.as_mut());
    }
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

/// A tile's round shape and outside label ring, derived purely from its
/// circle's diameter and the label font size, so the sizing pass
/// ([`Scene::update`]) and the draw pass ([`compose`]) can't drift apart.
#[derive(Clone, Copy)]
struct CircleGeom {
    r: f32,
    r_outer: f32,
    r_label_inner: f32,
    r_label_outer: f32,
    /// Half the outer bounding box's width.
    half_w: u32,
    outer_w: u32,
    outer_h: u32,
}

/// The border ring around a circle of `diameter`, and the label band
/// around that sized for `name_px` text.
fn circle_geometry(diameter: u32, name_px: u32) -> CircleGeom {
    let r = diameter as f32 / 2.0;
    let r_outer = r + BORDER as f32;
    let thickness = (name_px + 2 * LABEL_PAD_V) as f32;
    let r_label_inner = r_outer + LABEL_GAP as f32;
    let r_label_outer = r_label_inner + thickness;
    // The label band can bulge sideways past the circle itself at wide
    // angles; the tile's reserved box must be wide enough for that, sized
    // for the widest angle a name is ever allowed (truncation keeps
    // shorter names from needing it, but the slot stays constant).
    let band_half_angle = MAX_LABEL_HALF_ANGLE + LABEL_BAND_PAD_ANGLE;
    let bulge = r_label_outer * band_half_angle.sin();
    let half_w = r_outer.max(bulge).ceil() as u32;
    CircleGeom {
        r,
        r_outer,
        r_label_inner,
        r_label_outer,
        half_w,
        outer_w: half_w * 2,
        outer_h: (r_outer + r_label_outer).ceil() as u32,
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

/// Draws `name`, curved along the arc up to `max_half_angle` either side of
/// straight down from `(cx, cy)`, its ascent following the circle at
/// radius `r_inner`, cut with „…" to fit — the tile's outside label,
/// replacing the old strip drawn over the picture. Kerning is dropped
/// (each glyph is its own rigid rotation), and layout uses exact arc
/// length (`theta = s / r_inner`), not an approximation.
#[allow(clippy::too_many_arguments)]
fn draw_curved_label(
    canvas: &mut [u8],
    stride: usize,
    canvas_w: u32,
    canvas_h: u32,
    cx: f32,
    cy: f32,
    r_inner: f32,
    max_half_angle: f32,
    name: &str,
    px: u32,
    labels: &mut Labels,
) {
    let max_arc = 2.0 * max_half_angle * r_inner;
    let shown = labels.cut(name, px, max_arc);
    let total_w = labels.advance(&shown, px);
    if total_w <= 0.0 {
        return;
    }
    let mut caret = -total_w / 2.0;
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
    let band_half_angle = MAX_LABEL_HALF_ANGLE + LABEL_BAND_PAD_ANGLE;
    for tile in tiles {
        let diameter = match tile.face {
            Face::Camera(_) => g.camera_diameter,
            Face::Plate => g.plate_diameter,
        };
        let geom = circle_geometry(diameter, g.name_px);
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
        let cy = ty as f32 + geom.r_outer;
        let [fr, fg, fb] = g.frame_color;
        let border = if tile.speaking && g.speaking_frame {
            [fb, fg, fr, 0xff] // B G R A premultiplied
        } else {
            [0x30, 0x30, 0x30, 0xff]
        };
        let mul = if tile.dim { DIM_MUL } else { 256 };

        // Pass 1: the circle (fill + border ring) and the opaque label
        // band behind it; everything else stays transparent — that's what
        // makes the tile round instead of a square with a circle painted
        // on it.
        for y in 0..outer_h {
            for x in 0..outer_w {
                // `cx`/`cy` are absolute canvas coordinates (they already
                // include `tx`/`ty`); `x`/`y` here are tile-local, so they
                // need the same offset before comparing against them.
                let dx = (tx + x) as f32 + 0.5 - cx;
                let dy = (ty + y) as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let px = if dist <= geom.r {
                    Some(match &tile.face {
                        Face::Camera(Some((fw, fh, rgba))) => {
                            sample_camera_circle(*fw, *fh, rgba, dx, dy, geom.r, mul)
                        }
                        Face::Camera(None) => [0x20, 0x20, 0x20, 0xff],
                        Face::Plate => [0x1c, 0x1c, 0x1c, 0xff],
                    })
                } else if dist <= geom.r_outer {
                    Some(border)
                } else if dist > geom.r_label_inner
                    && dist <= geom.r_label_outer
                    && dx.atan2(dy).abs() <= band_half_angle
                {
                    Some([0x1c, 0x1c, 0x1c, 0xff])
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
                blend_label(canvas, stride, lx, ly, label);
            }
        }

        // Pass 3: the name, curved along the outside of the circle.
        if let Some(labels) = labels.as_deref_mut() {
            draw_curved_label(
                canvas,
                stride,
                g.width,
                g.height,
                cx,
                cy,
                geom.r_label_inner,
                MAX_LABEL_HALF_ANGLE,
                &tile.name,
                g.name_px,
                labels,
            );
        }

        if g.horizontal {
            x0 += outer_w + GAP;
        } else {
            y0 += outer_h + GAP;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAMERA_D: u32 = 120;
    const PLATE_D: u32 = 72;
    const NAME_PX: u32 = 12;

    fn geometry(tiles: &[Tile], horizontal: bool) -> Geometry {
        let geoms: Vec<CircleGeom> = tiles
            .iter()
            .map(|t| {
                let d = match t.face {
                    Face::Camera(_) => CAMERA_D,
                    Face::Plate => PLATE_D,
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
