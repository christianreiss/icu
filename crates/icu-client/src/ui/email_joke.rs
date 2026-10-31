//! Disposable Zawinski joke. Set the constant below to false to remove all UI.
use std::{io::Read, path::Path, sync::mpsc};

use egui::{Color32, RichText};

pub const EMAIL_JOKE_ENABLED: bool = true;
const ID: &str = "icu-email-joke";
const LIMIT: u64 = 64 * 1024;
const REJECTED: &str =
    "Activation failed. This licence is valid for a product that does not exist.";

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
        let mut close = false;
        let alive = super::main_window::immediate_window(
            ctx,
            ID,
            "ICU PRO — Zawinski Compliance Edition",
            [640.0, 620.0],
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
        if let Some(rx) = &self.pending {
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
        let mut close = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            egui::Frame::new().fill(Color32::from_rgb(84, 10, 132)).inner_margin(18).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("SPECIAL OFFER • LITE EDITION DETECTED").strong().color(Color32::YELLOW));
                ui.label(RichText::new("ICU PRO").size(48.0).strong().color(Color32::WHITE));
                ui.label(RichText::new("Zawinski Compliance Edition").size(22.0).color(Color32::LIGHT_GREEN));
                ui.label(RichText::new("Because every voice client deserves an email client.").color(Color32::WHITE));
            });
            ui.heading("You are running ICU Lite.");
            ui.label("Email is an exclusive Pro experience. Unlock tomorrow's unnecessary features TODAY!");
            for (title, body) in [
                ("LITE — Your current experience", "Voice chat. Friends. Suspiciously little enterprise synergy."),
                ("PRO — Everything you never asked for", "✓ Unlimited subject lines\n✓ Enterprise-grade Reply All\n✓ AI-powered unread guilt\n✓ Cloud-native attachment anxiety\n✓ Premium spam, delivered locally"),
            ] {
                egui::Frame::group(ui.style()).show(ui, |ui| { ui.set_width(ui.available_width()); ui.heading(title); ui.label(body); });
            }
            ui.label(RichText::new("BEST VALUE!  •  400% MORE EMAIL!  •  ZERO EXTRA PRODUCTIVITY!").strong().color(Color32::from_rgb(220, 100, 0)));
            if ui.add(egui::Button::new(RichText::new("UPGRADE NOW — ONLY €99.99 / MINUTE").strong().color(Color32::BLACK)).fill(Color32::YELLOW).wrap()).clicked() {
                self.status = "The upgrade server requires ICU Pro. Please upgrade to connect.".into();
            }
            if ui.add_enabled(self.pending.is_none(), egui::Button::new("Load licence file…")).clicked() {
                let (tx, rx) = mpsc::channel();
                self.pending = Some(rx);
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let result = rfd::FileDialog::new().set_title("Activate ICU Pro — bring your own disappointment")
                        .pick_file().map(|path| reject_file(&path));
                    let _ = tx.send(result);
                    ctx.request_repaint();
                });
            }
            if self.pending.is_some() { ui.label("Awaiting your licence…"); }
            if !self.status.is_empty() { ui.label(RichText::new(&self.status).strong()); }
            ui.separator();
            close = ui.button("Close — remain gloriously Lite").clicked();
            ui.small("A Zawinski's Law joke. No purchase, no activation, no email. Your actual ICU features remain free of this nonsense.");
        });
        close
    }
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
                h.get_by_label("UPGRADE NOW — ONLY €99.99 / MINUTE").click();
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
