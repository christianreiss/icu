//! Disposable Zawinski joke. Set the constant below to false to remove all UI.
use std::{io::Read, path::Path, sync::mpsc};

use egui::emath::Rot2;
use egui::{Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

pub const EMAIL_JOKE_ENABLED: bool = true;
const ID: &str = "icu-email-joke";
const LIMIT: u64 = 64 * 1024;
const REJECTED: &str =
    "Activation failed. This licence is valid for a product that does not exist.";
const UPGRADE: &str = "The upgrade server requires ICU Pro. Please upgrade to connect.";
const MARQUEE_H: f32 = 28.0;
const TOP_LINE: &str = "BUY NOW  •  RATIO  •  400% MORE EMAIL  •  REPLY ALL INCLUDED  •  UNREAD GUILT  •  SOURCE: TRUST ME BRO  •  THIS CLIENT NOW HAS EMAIL  •  ";
const BOTTOM_LINE: &str = "BUY NOW  •  NO REFUNDS BECAUSE NO PRODUCT  •  W  •  TOUCH GRASS NOT INCLUDED  •  ZAWINSKI COMPLIANCE  •  ";
const PRICE_BANDS: [&str; 3] = [
    "BEST VALUE",
    "WORST VALUE",
    "400% MORE EMAIL  •  ZERO EXTRA PRODUCTIVITY",
];
const LEFT: [u8; 3] = [1, 2, 7];
const HERO_COLORS: [Color32; 4] = [
    Color32::from_rgb(84, 10, 132),
    Color32::from_rgb(190, 0, 110),
    Color32::from_rgb(12, 150, 48),
    Color32::from_rgb(220, 90, 0),
];
const TITLE_COLORS: [Color32; 3] = [
    Color32::YELLOW,
    Color32::WHITE,
    Color32::from_rgb(0, 255, 255),
];

#[derive(Default)]
pub struct EmailJoke {
    open: bool,
    status: String,
    pending: Option<mpsc::Receiver<Option<String>>>,
}

impl EmailJoke {
    pub fn open(&mut self, ctx: &egui::Context) {
        if EMAIL_JOKE_ENABLED {
            self.open = true;
            ctx.send_viewport_cmd_to(
                egui::ViewportId::from_hash_of(ID),
                egui::ViewportCommand::Focus,
            );
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !EMAIL_JOKE_ENABLED || !self.open {
            return;
        }
        super::repaint_after(ctx, std::time::Duration::from_millis(16));
        let mut close = false;
        let alive = super::main_window::immediate_window(
            ctx,
            ID,
            "ICU PRO — BUY NOW — Zawinski Compliance Edition",
            [720.0, 720.0],
            Some([360.0, 380.0]),
            |ui| close = self.ui(ui),
        );
        if close || !alive {
            self.open = false;
            self.status.clear();
            self.pending = None;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        self.poll_licence();
        let full = ui.available_rect_before_wrap();
        let top = Rect::from_min_size(full.min, Vec2::new(full.width(), MARQUEE_H));
        let bottom = Rect::from_min_size(
            Pos2::new(full.left(), full.bottom() - MARQUEE_H),
            Vec2::new(full.width(), MARQUEE_H),
        );
        paint_marquee(
            ui,
            top,
            TOP_LINE,
            180.0,
            false,
            Color32::from_rgb(180, 0, 120),
            Color32::WHITE,
        );
        paint_marquee(
            ui,
            bottom,
            BOTTOM_LINE,
            140.0,
            true,
            Color32::from_rgb(12, 8, 20),
            Color32::from_rgb(0, 255, 255),
        );
        let mid = Rect::from_min_max(
            Pos2::new(full.left(), top.bottom() + 4.0),
            Pos2::new(full.right(), bottom.top() - 4.0),
        );
        let mut close = false;
        if mid.height() > 40.0 {
            let mut mid_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(mid)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("icu-email-carnival")
                .show(&mut mid_ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    close = self.offers(ui);
                });
        }
        ui.advance_cursor_after_rect(full);
        close
    }

    fn poll_licence(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                if let Some(status) = result {
                    self.status = status;
                }
                self.pending = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.status = "Licence loading failed. Please try again.".into();
                self.pending = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn offers(&mut self, ui: &mut egui::Ui) -> bool {
        let t = ui.input(|i| i.time);
        let hero = egui::Frame::new()
            .fill(HERO_COLORS[phase(t, 3.0, HERO_COLORS.len())])
            .inner_margin(18)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let band =
                    Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), 140.0));
                paint_stars(ui.painter(), band, t as f32);
                hero_copy(ui, t);
            });
        paint_stickers(ui, hero.response.rect);

        ui.heading("You are running ICU Lite.");
        ui.label(
            "Email is an exclusive Pro experience. Unlock tomorrow's unnecessary features TODAY!",
        );
        for (title, body) in [
            (
                "LITE — Your current experience",
                "Voice chat. Friends. Suspiciously little enterprise synergy.",
            ),
            (
                "PRO — Everything you never asked for",
                "✓ Unlimited subject lines\n✓ Enterprise-grade Reply All\n✓ AI-powered unread guilt\n✓ Cloud-native attachment anxiety\n✓ Premium spam, delivered locally\n✓ Reply guys, preinstalled",
            ),
        ] {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.heading(title);
                ui.label(body);
            });
        }
        ui.label(
            RichText::new(PRICE_BANDS[phase(t, 2.5, PRICE_BANDS.len())])
                .strong()
                .color(Color32::from_rgb(220, 100, 0)),
        );
        let sec = 9 - (t.floor() as u64 % 10);
        ui.label(
            RichText::new(format!("OFFER EXPIRES IN 0:0{sec}"))
                .strong()
                .color(Color32::from_rgb(220, 20, 60)),
        );
        ui.label(
            RichText::new(format!("ONLY {} LEFT", LEFT[phase(t, 2.0, LEFT.len())]))
                .strong()
                .color(Color32::from_rgb(180, 0, 0)),
        );

        let pulse = 0.5 + 0.5 * (t * 7.0).sin() as f32;
        let green = (70.0 + 170.0 * pulse).round() as u8;
        if ui
            .add_sized(
                [ui.available_width(), 64.0],
                egui::Button::new(
                    RichText::new("BUY NOW")
                        .size(28.0)
                        .strong()
                        .color(Color32::BLACK),
                )
                .fill(Color32::from_rgb(255, green, 0)),
            )
            .clicked()
        {
            self.status = UPGRADE.into();
        }
        let price = if (t * 5.0).sin() >= 0.0 {
            Color32::from_rgb(255, 0, 120)
        } else {
            Color32::YELLOW
        };
        ui.label(RichText::new("ONLY €99.99 / MINUTE").strong().color(price));

        if ui
            .add_enabled(
                self.pending.is_none(),
                egui::Button::new("Load licence file…"),
            )
            .clicked()
        {
            let (tx, rx) = mpsc::channel();
            self.pending = Some(rx);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let result = rfd::FileDialog::new()
                    .set_title("Activate ICU Pro — bring your own disappointment")
                    .pick_file()
                    .map(|path| reject_file(&path));
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        }
        if self.pending.is_some() {
            ui.label("Awaiting your licence…");
        }
        if !self.status.is_empty() {
            ui.label(RichText::new(&self.status).strong());
        }
        ui.separator();
        ui.button("Close — remain gloriously Lite").clicked()
    }
}

fn phase(t: f64, hz: f64, len: usize) -> usize {
    let tick = (t * hz).floor();
    let tick = if tick <= 0.0 { 0 } else { tick as usize };
    tick % len
}

fn hero_copy(ui: &mut egui::Ui, t: f64) {
    let (blink, blink_color) = if (t * 8.0).sin() >= 0.0 {
        ("LITE DETECTED", Color32::YELLOW)
    } else {
        ("SKILL ISSUE", Color32::from_rgb(255, 50, 50))
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
    paint_text(ui, rect.left_top(), blink, 16.0, blink_color);
    ui.label(
        RichText::new("ICU PRO")
            .size(48.0)
            .strong()
            .color(TITLE_COLORS[phase(t, 5.0, TITLE_COLORS.len())]),
    );
    ui.label(
        RichText::new("Zawinski Compliance Edition")
            .size(22.0)
            .color(Color32::LIGHT_GREEN),
    );
    ui.label(
        RichText::new("Because every voice client deserves an email client.").color(Color32::WHITE),
    );
}

fn paint_text(ui: &egui::Ui, pos: Pos2, text: &str, size: f32, color: Color32) {
    let galley = ui.painter().fonts_mut(|fonts| {
        fonts.layout_no_wrap(
            text.to_owned(),
            FontId::new(size, egui::FontFamily::Proportional),
            color,
        )
    });
    ui.painter().galley(pos, galley, color);
}

fn paint_stars(painter: &egui::Painter, rect: Rect, t: f32) {
    const COLORS: [Color32; 4] = [
        Color32::YELLOW,
        Color32::WHITE,
        Color32::from_rgb(0, 255, 255),
        Color32::from_rgb(255, 80, 200),
    ];
    let painter = painter.with_clip_rect(rect);
    for i in 0..12 {
        let n = i as f32;
        let x = rect.left() + (n * 0.173 + t * (0.04 + n * 0.004)).fract() * rect.width();
        let y =
            rect.top() + (n * 0.271 + t * (0.03 + (i % 3) as f32 * 0.01)).fract() * rect.height();
        painter.circle_filled(
            Pos2::new(x, y),
            1.5 + (i % 3) as f32,
            COLORS[i % COLORS.len()],
        );
    }
}

fn paint_stickers(ui: &egui::Ui, rect: Rect) {
    sticker(
        ui,
        Pos2::new(rect.right() - 86.0, rect.top() + 34.0),
        "BUY NOW",
        -0.35,
        Color32::YELLOW,
        Color32::BLACK,
    );
    sticker(
        ui,
        Pos2::new(rect.right() - 168.0, rect.top() + 26.0),
        "WOW",
        0.28,
        Color32::from_rgb(220, 0, 140),
        Color32::WHITE,
    );
    sticker(
        ui,
        Pos2::new(rect.right() - 78.0, rect.top() + 86.0),
        "NEW",
        -0.12,
        Color32::from_rgb(150, 255, 0),
        Color32::BLACK,
    );
}

fn sticker(ui: &egui::Ui, center: Pos2, text: &str, angle: f32, fill: Color32, ink: Color32) {
    let galley = ui.painter().fonts_mut(|fonts| {
        fonts.layout_no_wrap(
            text.to_owned(),
            FontId::new(18.0, egui::FontFamily::Proportional),
            ink,
        )
    });
    let size = galley.size() + Vec2::new(28.0, 16.0);
    let rot = Rot2::from_angle(angle);
    let half = size / 2.0;
    let points = [
        Vec2::new(-half.x, -half.y),
        Vec2::new(half.x, -half.y),
        Vec2::new(half.x, half.y),
        Vec2::new(-half.x, half.y),
    ]
    .map(|corner| center + rot * corner);
    ui.painter().add(Shape::convex_polygon(
        points.to_vec(),
        fill,
        Stroke::new(2.0, Color32::BLACK),
    ));
    // `with_angle_and_anchor` keeps the anchor where the unrotated text
    // already placed it. The galley origin has to put that anchor on `center`
    // first; passing `center` itself shifts the letters by half a line.
    let anchor = egui::Align2::CENTER_CENTER
        .pos_in_rect(&galley.rect)
        .to_vec2();
    ui.painter().add(
        egui::epaint::TextShape::new(center - anchor, galley, ink)
            .with_angle_and_anchor(angle, egui::Align2::CENTER_CENTER),
    );
}

fn paint_marquee(
    ui: &egui::Ui,
    rect: Rect,
    text: &str,
    speed: f32,
    reverse: bool,
    bg: Color32,
    fg: Color32,
) {
    let t = ui.input(|i| i.time) as f32;
    let font = FontId::new(18.0, egui::FontFamily::Proportional);
    let unit = ui
        .painter()
        .fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.repeat(2), font.clone(), fg)
                .size()
                .x
        })
        .max(2.0)
        / 2.0;
    let copies = ((rect.width() / unit).ceil() as usize).saturating_add(2);
    let galley = ui
        .painter()
        .fonts_mut(|fonts| fonts.layout_no_wrap(text.repeat(copies), font, fg));
    let shift = (t * speed) % unit;
    let x = if reverse {
        rect.left() - unit + shift
    } else {
        rect.left() - shift
    };
    let y = rect.center().y - galley.size().y * 0.5;
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, bg);
    painter.galley(Pos2::new(x, y), galley, fg);
}

fn reject_file(path: &Path) -> String {
    let result = (|| -> std::io::Result<bool> {
        if !std::fs::metadata(path)?.is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        let file = std::fs::File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        Ok(bytes.len() as u64 > LIMIT)
    })();
    match result {
        Ok(false) => REJECTED.into(),
        Ok(true) => "Activation failed. Licence exceeds 64 KiB. Even our bloat has limits.".into(),
        Err(_) => "Activation failed. Could not read the licence file.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_renders_in_both_themes_and_narrow_sizes() {
        use egui_kittest::{kittest::Queryable, Harness};
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for width in [360.0, 640.0] {
                let mut view = EmailJoke::default();
                let mut h = Harness::builder()
                    .with_size(egui::vec2(width, 900.0))
                    .with_theme(theme)
                    .build_ui(move |ui| {
                        view.ui(ui);
                    });
                h.run();
                h.get_by_label("BUY NOW").click();
                h.run();
                h.get_by_label("The upgrade server requires ICU Pro. Please upgrade to connect.");
                h.get_by_label("Close — remain gloriously Lite");
            }
        }
    }

    #[test]
    fn every_licence_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("licence");
        for bytes in [
            vec![],
            b"VALID ICU PRO LICENCE".to_vec(),
            vec![0xff; LIMIT as usize],
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(reject_file(&path), REJECTED);
        }
        std::fs::write(&path, vec![0; LIMIT as usize + 1]).unwrap();
        assert!(reject_file(&path).contains("exceeds 64 KiB"));
        assert!(reject_file(dir.path()).contains("Could not read"));
        assert!(reject_file(&dir.path().join("missing")).contains("Could not read"));
    }
}
