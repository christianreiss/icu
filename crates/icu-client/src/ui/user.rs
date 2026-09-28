//! The per-user window: one person's connection, encryption and voice
//! controls at a glance, opened by clicking them in the list
//! (`super::main_window::participant_row`). Replaces the old inline
//! accordion under the row — a window can afford a visual read on ping and
//! a plain sentence on encryption instead of a label/value grid squeezed
//! under a name.
//!
//! Encryption is a property of this client's one QUIC/TLS/mTLS session to
//! the server, not of a link to this particular person (audio is
//! server-relayed — see `connection.rs`'s own note on this). So this window
//! states that plainly and links to the full Verbindung window instead of
//! inventing a per-user cipher.

use egui::{RichText, Ui};
use egui_phosphor::regular as icon;

use super::{
    connection::{hairline, opt, plain_row, Row, Tone, NONE},
    main_window::{self, moderation_menu, Participant},
    theme, widgets,
};
use crate::{dsp::gate::Mode, engine::{AppState, Command}};

/// State of the per-user window: just who it's showing. Everything else is
/// read fresh from [`AppState`] each frame, the same way
/// [`super::connection::ConnectionView`] does.
pub struct UserView {
    user_id: icu_proto::UserId,
}

/// What [`UserView::ui`] asks of the app around it.
#[derive(Debug, Default)]
pub struct UserOutput {
    /// „Alle Verbindungsdetails" was clicked: open the Verbindung window.
    pub open_connection: bool,
    /// This person is no longer in the room: close the window.
    pub left: bool,
    /// How much taller the cards are than the room they got; the window
    /// grows by it (`super::grow_to_fit`), same as every other window.
    pub overflow: f32,
}

impl UserView {
    /// A fresh view targeting `user_id`.
    pub fn new(user_id: icu_proto::UserId) -> UserView {
        UserView { user_id }
    }

    /// Who this window is showing.
    pub fn user_id(&self) -> icu_proto::UserId {
        self.user_id
    }

    /// Draws the window body. `video` looks up a live picture by session
    /// (`main_window::MainEnv::video`); `send` posts a command.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        video: &dyn Fn(icu_proto::Session) -> Option<egui::load::SizedTexture>,
        send: &impl Fn(Command),
    ) -> UserOutput {
        let mut out = UserOutput::default();
        // The local user's own camera preview isn't threaded through here —
        // see `main_window::App::user_window` — so it falls back to
        // initials, same as a peer whose camera is off.
        let Some(p) = main_window::participant_for(state, self.user_id, video, None) else {
            out.left = true;
            return out;
        };
        let tok = theme::tokens(ui.visuals());

        header(ui, &p, &tok);
        ui.add_space(8.0);
        // A `ScrollArea`, like every other window: a glance window is sized
        // for the common case, but a long name or an admin's extra card
        // must never clip silently.
        let page = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                connection_card(ui, &p);
                ui.add_space(4.0);
                if encryption_card(ui) {
                    out.open_connection = true;
                }
                if let Some(v) = p.volume {
                    ui.add_space(4.0);
                    volume_card(ui, &p, v, send);
                }
                if let Some(m) = p.moderation {
                    ui.add_space(4.0);
                    widgets::group_card(ui, "Moderation", |ui| {
                        moderation_menu(ui, p.user_id, m, send);
                    });
                }
            });
        out.overflow = page.content_size.y - page.inner_rect.height();
        super::grow_to_fit(ui.ctx(), out.overflow);
        out
    }
}

/// Avatar, name, role and a live one-word status — the identity strip every
/// other card sits under.
fn header(ui: &mut Ui, p: &Participant<'_>, tok: &theme::Tokens) {
    ui.horizontal(|ui| {
        widgets::avatar(ui, p.name, p.user_id, p.speaking, 56.0, p.video);
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(theme::strong_text(p.name).size(17.0));
                widgets::badge(
                    ui,
                    if p.is_admin { "Admin" } else { "Mitglied" },
                    if p.is_admin {
                        widgets::BadgeKind::Accent
                    } else {
                        widgets::BadgeKind::Neutral
                    },
                );
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (color, word) = status_word(p, tok);
                widgets::status_pill(ui, color, word);
                // `UserFlags::VIEWING` while somebody's camera is on:
                // independent of speaking/muted/deafened, so it sits beside
                // the pill instead of replacing it.
                if p.viewing {
                    ui.label(RichText::new("schaut Video").size(11.5).color(tok.dim_text));
                }
            });
        });
    });
}

/// The one-word status a `status_pill` shows: what this person is doing
/// right now, priority order speaking > deafened > muted > idle.
fn status_word(p: &Participant<'_>, tok: &theme::Tokens) -> (egui::Color32, &'static str) {
    if p.speaking {
        (tok.connected, "Spricht")
    } else if p.deafened {
        (tok.error, "Hört nicht zu")
    } else if p.muted {
        (tok.error, "Stummgeschaltet")
    } else {
        (tok.muted_text, "Verbunden")
    }
}

/// Signal bars sized up for one-glance reading, the ping in words and in
/// milliseconds, then whatever is specific to this person: their mode and
/// own ping for the local user, their packet loss for everyone else.
fn connection_card(ui: &mut Ui, p: &Participant<'_>) {
    widgets::group_card(ui, "Verbindung", |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(widgets::SIGNAL_BARS_W * 2.0, 36.0),
                        egui::Sense::hover(),
                    );
                    if let Some(ms) = p.rtt_ms {
                        widgets::signal_bars(ui, rect, main_window::rtt_bars(ms));
                    }
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(ping_word(p.rtt_ms)).size(14.5).strong());
                        let ms_text = match p.rtt_ms {
                            Some(ms) => format!("Ping {ms} ms"),
                            None => "Ping unbekannt".to_string(),
                        };
                        ui.label(
                            RichText::new(ms_text)
                                .size(12.0)
                                .color(theme::tokens(ui.visuals()).dim_text),
                        );
                    });
                });
            });
        hairline(ui);
        if let Some(mode) = p.mode {
            plain_row(
                ui,
                &Row::new(
                    "Modus",
                    match mode {
                        Mode::Ptt => "Push-to-Talk",
                        Mode::Vad => "Sprachaktivierung",
                    },
                ),
            );
            hairline(ui);
            plain_row(
                ui,
                &Row::new(
                    "Ping (eigen)",
                    opt(p.client_rtt_ms.map(|v| format!("{v} ms"))),
                ),
            );
        } else {
            let (loss, tone) = loss_word(p.loss_permille);
            plain_row(ui, &Row::new("Paketverlust", loss).tone(tone));
        }
        hairline(ui);
        plain_row(
            ui,
            &Row::mono("Nutzer · Sitzung", format!("#{} · {}", p.user_id, p.session)),
        );
    });
}

/// A word for `rtt_bars`' 1–5, the same scale the list's signal bars use.
fn ping_word(ms: Option<u16>) -> &'static str {
    match ms {
        None => "Unbekannt",
        Some(ms) => match main_window::rtt_bars(ms) {
            5 => "Ausgezeichnet",
            4 => "Gut",
            3 => "Mäßig",
            _ => "Schlecht",
        },
    }
}

/// What this client loses of this person's audio, toned like the
/// Verbindung window tones transport loss: none of it is fine, a little is
/// a warning, a lot is a fault.
fn loss_word(loss: Option<u16>) -> (String, Tone) {
    match loss {
        None => (NONE.to_string(), Tone::Plain),
        Some(0) => ("0 ‰".to_string(), Tone::Good),
        Some(l) if l <= 20 => (format!("{l} ‰"), Tone::Warn),
        Some(l) => (format!("{l} ‰"), Tone::Bad),
    }
}

/// States plainly that this is the one shared, encrypted session to the
/// server — not a private channel to this person — and links to the full
/// Verbindung window for the rest. Returns whether that link was clicked.
fn encryption_card(ui: &mut Ui) -> bool {
    let mut open_connection = false;
    widgets::group_card(ui, "Verschlüsselung", |ui| {
        plain_row(
            ui,
            &Row::new("Sitzung", "QUIC v1 · TLS 1.3 · mTLS").tone(Tone::Good),
        );
        hairline(ui);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "Gilt für die ganze Verbindung zum Server – diese Person teilt sie mit \
                         allen im Raum, es gibt keinen eigenen Schlüssel nur für sie.",
                    )
                    .size(11.5)
                    .color(theme::tokens(ui.visuals()).dim_text),
                );
                ui.add_space(6.0);
                if widgets::button(
                    ui,
                    widgets::ButtonKind::Neutral,
                    icon::LOCK_KEY,
                    "Alle Verbindungsdetails",
                )
                .clicked()
                {
                    open_connection = true;
                }
            });
    });
    open_connection
}

/// The playback volume this client applies to this person — moved
/// verbatim from the old inline accordion, only in a card of its own.
fn volume_card(ui: &mut Ui, p: &Participant<'_>, mut v: f32, send: &impl Fn(Command)) {
    widgets::group_card(ui, "Lautstärke (nur für dich)", |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    widgets::chip(ui, &format!("{v:.0} %"), 46.0);
                    // One click to silence somebody for yourself, and back.
                    let muted = v <= 0.0;
                    let glyph = if muted {
                        icon::SPEAKER_X
                    } else {
                        icon::SPEAKER_HIGH
                    };
                    if widgets::icon_button(ui, glyph, 24.0, muted)
                        .on_hover_text(if muted {
                            "Wieder hören"
                        } else {
                            "Für mich stummschalten"
                        })
                        .clicked()
                    {
                        send(Command::SetVolume(
                            p.user_id,
                            if muted { 100.0 } else { 0.0 },
                        ));
                    }
                });
                ui.add_space(6.0);
                let name = format!("Lautstärke {}", p.name);
                if widgets::named_slider(ui, &mut v, 0.0..=200.0, &name).changed() {
                    send(Command::SetVolume(p.user_id, v));
                }
            });
    });
}
