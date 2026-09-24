//! The Verbindung window: everything about the live connection, opened by
//! clicking the status pill in the header. Read-only — it sends no
//! `Command` and changes no setting.
//!
//! Six pages behind the same icon rail `ui::settings` uses — sixty readings
//! in one scroll is a wall of text, and the app already knows how to switch
//! pages.
//!
//! Each page's content is built as a list of sections first and rendered
//! from that, rather than painted straight into the `Ui`. Two reasons: the
//! „Bericht kopieren" button can then dump every tab through the same
//! formatting the screen uses, instead of a second copy that drifts, and
//! every row gets the same label/value/tone treatment for free.

use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use egui::{RichText, Ui};
use egui_phosphor::regular as icon;

use super::{theme, widgets};
use crate::{
    audio::DeviceKind,
    conninfo::CertInfo,
    engine::{AppState, ConnStatus},
    stats::DebugStats,
    video::VideoTier,
};

/// Placeholder for a value that does not exist right now — a reading taken
/// before the first tick, or anything the platform declines to report.
const NONE: &str = "—";

/// How long „Kopiert" stays under a fingerprint after the copy button.
const COPY_FEEDBACK: Duration = Duration::from_secs(2);

/// Transport loss above this reads as a warning, and twice it as a fault.
const LOSS_WARN_PERCENT: f32 = 1.0;

/// A certificate closer than this to its expiry is called out.
const CERT_WARN_DAYS: i64 = 30;

/// What a value means, which is all a row has to say: the color follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    /// Ordinary reading.
    Plain,
    /// Working as intended, worth confirming (encryption, healthy link).
    Good,
    /// Degraded but not broken.
    Warn,
    /// Broken or expired.
    Bad,
}

/// One label/value line inside a [`Section`].
#[derive(Debug, Clone)]
struct Row {
    label: String,
    value: String,
    tone: Tone,
    /// Render the value in a monospace chip — numbers and identifiers, so
    /// columns of them line up and a changing digit does not reflow the row.
    mono: bool,
    /// Put the value on its own line under the label, full width and
    /// wrapping. `setting_row` right-aligns its control inside a fixed
    /// 30 px-high strip, so a value that is longer than the space left over
    /// would be clipped rather than wrapped — device descriptions,
    /// certificate subjects and cipher lists all get long enough for that.
    block: bool,
    /// Add a copy button. Only meaningful with [`Self::block`].
    copy: bool,
}

impl Row {
    fn new(label: impl Into<String>, value: impl Into<String>) -> Row {
        Row {
            label: label.into(),
            value: value.into(),
            tone: Tone::Plain,
            mono: false,
            block: false,
            copy: false,
        }
    }

    /// A numeric or identifier row.
    fn mono(label: impl Into<String>, value: impl Into<String>) -> Row {
        Row {
            mono: true,
            ..Row::new(label, value)
        }
    }

    fn tone(mut self, tone: Tone) -> Row {
        self.tone = tone;
        self
    }

    /// Put the value on its own line, but only if it is long enough to
    /// need one. A short subject or device name reads better inline, and a
    /// window this dense cannot afford two lines for „localhost".
    fn block_if_long(mut self) -> Row {
        self.block = self.value.chars().count() > 34;
        self
    }

    /// A full-width, copyable, monospace block — used for fingerprints.
    fn copyable(mut self) -> Row {
        self.block = true;
        self.copy = true;
        self.mono = true;
        self
    }
}

/// A titled card's worth of rows, plus an optional closing note.
#[derive(Debug, Clone)]
struct Section {
    title: String,
    rows: Vec<Row>,
    note: Option<String>,
}

impl Section {
    fn new(title: impl Into<String>) -> Section {
        Section {
            title: title.into(),
            rows: Vec::new(),
            note: None,
        }
    }

    fn row(mut self, row: Row) -> Section {
        self.rows.push(row);
        self
    }

    fn note(mut self, note: impl Into<String>) -> Section {
        self.note = Some(note.into());
        self
    }
}

/// Which page the rail is on. Same rail as `ui::settings`: sixty readings
/// in one scroll is a wall of text, and the app already knows how to switch
/// pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Server,
    Transport,
    Crypto,
    Audio,
    Video,
    People,
}

/// One card in the window.
#[derive(Debug, Clone)]
enum Card {
    /// A titled card of rows.
    Plain(Section),
    /// The three certificates share one card behind a picker: their rows are
    /// identical in shape, so showing all three at once triples the card
    /// without adding one new kind of information.
    Certs(Vec<Section>),
}

/// Which certificate the Zertifikat card is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CertPick {
    Server,
    Own,
    Ca,
}

impl CertPick {
    /// Index into the three sections [`cert_sections`] builds, in order.
    fn index(self) -> usize {
        match self {
            CertPick::Server => 0,
            CertPick::Own => 1,
            CertPick::Ca => 2,
        }
    }
}

/// Jitter percentiles, refreshed on a timer. The histograms live behind
/// mutexes the audio path also takes; locking them at 30 fps would be a
/// needless contention point, so this mirrors `settings::DiagnoseCache`.
#[derive(Default)]
struct HistCache {
    refreshed: Option<Instant>,
    jitter: String,
}

impl HistCache {
    /// At most one refresh every two seconds.
    fn get(&mut self, stats: &DebugStats) -> &str {
        let now = Instant::now();
        if self
            .refreshed
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(2))
        {
            return &self.jitter;
        }
        self.refreshed = Some(now);
        let h = stats.jitter_delay.lock().unwrap_or_else(|e| e.into_inner());
        let fmt = |v: f64| {
            if v.is_infinite() {
                ">1000 ms".to_string()
            } else {
                format!("{v:.1} ms")
            }
        };
        self.jitter = if h.count() == 0 {
            NONE.to_string()
        } else {
            format!("{} / {}", fmt(h.quantile_ms(0.5)), fmt(h.quantile_ms(0.95)))
        };
        &self.jitter
    }
}

/// State of the Verbindung window. Owns nothing the engine needs; it is
/// dropped and rebuilt every time the window is closed and reopened.
pub struct ConnectionView {
    stats: Arc<DebugStats>,
    cache: HistCache,
    /// Which page the rail is on.
    tab: Tab,
    /// Which certificate the Zertifikat card shows.
    cert: CertPick,
    /// Which row was copied, and when, for the inline confirmation.
    copied: Option<(String, Instant)>,
}

impl ConnectionView {
    /// A fresh view over the engine's counters.
    pub fn new(stats: Arc<DebugStats>) -> ConnectionView {
        ConnectionView {
            stats,
            cache: HistCache::default(),
            tab: Tab::Server,
            cert: CertPick::Server,
            copied: None,
        }
    }

    /// Draws the whole window body: the status strip, then the icon rail
    /// and whichever page it is on.
    pub fn ui(&mut self, ui: &mut Ui, state: &AppState) {
        hero(ui, state);
        ui.add_space(10.0);

        egui::Panel::left("connection_rail")
            .resizable(false)
            // Same arithmetic as the settings rail: 64 px buttons, 3 px
            // frame inset either side, 8 px panel margins.
            .default_size(88.0)
            .show(ui, |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .stroke(egui::Stroke::new(
                        1.0,
                        ui.visuals().widgets.inactive.bg_stroke.color,
                    ))
                    .corner_radius(egui::CornerRadius::same(theme::R_CARD))
                    .inner_margin(3)
                    .show(ui, |ui| {
                        for (tab, glyph, label) in [
                            (Tab::Server, icon::PLUGS_CONNECTED, "Server"),
                            (Tab::Transport, icon::GAUGE, "Netz"),
                            (Tab::Crypto, icon::LOCK_KEY, "Krypto"),
                            (Tab::Audio, icon::MICROPHONE, "Audio"),
                            (Tab::Video, icon::VIDEO_CAMERA, "Video"),
                            (Tab::People, icon::USERS_THREE, "Wer"),
                        ] {
                            if widgets::rail_button(ui, glyph, label, self.tab == tab) {
                                self.tab = tab;
                            }
                            ui.add_space(3.0);
                        }
                    });
            });

        let jitter = self.cache.get(&self.stats).to_string();
        let cards = self.page(state, &jitter);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for card in &cards {
                    match card {
                        Card::Plain(section) => self.draw_section(ui, section),
                        Card::Certs(certs) => self.draw_certs(ui, certs),
                    }
                }
                self.footer(ui, state, &jitter);
            });
    }

    /// The cards the current tab shows.
    fn page(&self, state: &AppState, jitter: &str) -> Vec<Card> {
        match self.tab {
            Tab::Server => vec![Card::Plain(server_section(state))],
            Tab::Transport => vec![Card::Plain(transport_section(state, &self.stats))],
            Tab::Crypto => vec![
                Card::Plain(encryption_section(state)),
                Card::Certs(cert_sections(state)),
            ],
            Tab::Audio => vec![Card::Plain(audio_section(state, &self.stats, jitter))],
            Tab::Video => vec![Card::Plain(video_section(state, &self.stats))],
            Tab::People => vec![Card::Plain(participants_section(state))],
        }
    }

    /// One card.
    fn draw_section(&mut self, ui: &mut Ui, section: &Section) {
        widgets::group_card(ui, &section.title, |ui| {
            // `widgets::setting_row` is built around a 30 px control and
            // pays `item_spacing.y` twice around its separator — 72 px a
            // row. That is right for a panel of switches and wrong for
            // sixty readings, so this window lays its own rows out.
            ui.spacing_mut().item_spacing.y = 0.0;
            self.draw_rows(ui, section);
        });
    }

    /// The three certificates in one card, one at a time.
    fn draw_certs(&mut self, ui: &mut Ui, certs: &[Section]) {
        widgets::group_card(ui, "Zertifikat", |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Zeigt").size(12.5));
                        widgets::rtl(ui, 22.0, |ui| {
                            widgets::segmented(
                                ui,
                                &mut self.cert,
                                &[
                                    (CertPick::Server, "Server"),
                                    (CertPick::Own, "Eigenes"),
                                    (CertPick::Ca, "CA"),
                                ],
                            );
                        });
                    });
                });
            if let Some(section) = certs.get(self.cert.index()) {
                hairline(ui);
                self.draw_rows(ui, section);
            }
        });
    }

    /// The rows (and closing note) of one section, without its own card.
    fn draw_rows(&mut self, ui: &mut Ui, section: &Section) {
        for (i, row) in section.rows.iter().enumerate() {
            if i > 0 {
                hairline(ui);
            }
            if row.block {
                self.block_row(ui, row);
            } else {
                plain_row(ui, row);
            }
        }
        if let Some(note) = &section.note {
            if !section.rows.is_empty() {
                hairline(ui);
            }
            let tok = theme::tokens(ui.visuals());
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 7))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(note).size(11.5).color(tok.dim_text)).wrap(),
                    );
                });
        }
    }

    /// Label on one line, the value wrapping underneath at full width, and
    /// optionally a copy button beside the label.
    fn block_row(&mut self, ui: &mut Ui, row: &Row) {
        let tok = theme::tokens(ui.visuals());
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                // `ui.horizontal`, not `widgets::ltr`: the latter is
                // `with_layout`, which claims the parent's entire remaining
                // height (`Client/IMPLEMENTATION.md` §16) — inside a
                // non-shrinking `ScrollArea` that is the whole viewport, and
                // the card balloons to fill it.
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&row.label).size(12.5));
                    if row.copy {
                        widgets::rtl(ui, 22.0, |ui| {
                            if widgets::icon_button(ui, icon::COPY, 22.0, false)
                                .on_hover_text("In die Zwischenablage")
                                .clicked()
                            {
                                ui.ctx().copy_text(row.value.replace('\n', ""));
                                self.copied = Some((row.label.clone(), Instant::now()));
                            }
                            if self.copied.as_ref().is_some_and(|(l, t)| {
                                *l == row.label && t.elapsed() < COPY_FEEDBACK
                            }) {
                                ui.label(
                                    RichText::new(format!("{} Kopiert", icon::CHECK))
                                        .size(11.0)
                                        .color(tok.connected),
                                );
                            }
                        });
                    }
                });
                ui.add_space(2.0);
                let mut text = RichText::new(&row.value)
                    .size(if row.mono { 11.0 } else { 12.5 })
                    .color(tone_color(row.tone, &tok).unwrap_or(tok.muted_text));
                if row.mono {
                    text = text.monospace();
                }
                ui.add(egui::Label::new(text).wrap());
            });
    }

    /// „Bericht kopieren", under the last card.
    fn footer(&mut self, ui: &mut Ui, state: &AppState, jitter: &str) {
        widgets::card_note(ui, |ui| {
            // `ui.horizontal`, not `widgets::ltr`: see `block_row`.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if widgets::button(
                    ui,
                    widgets::ButtonKind::Neutral,
                    icon::COPY,
                    "Bericht kopieren",
                )
                .on_hover_text("Alle Reiter als Text, nicht nur den sichtbaren")
                .clicked()
                {
                    ui.ctx()
                        .copy_text(report(state, &build(state, &self.stats, jitter)));
                    self.copied = Some(("__report".to_string(), Instant::now()));
                }
                if self
                    .copied
                    .as_ref()
                    .is_some_and(|(l, t)| l == "__report" && t.elapsed() < COPY_FEEDBACK)
                {
                    let tok = theme::tokens(ui.visuals());
                    ui.label(
                        RichText::new(format!("{} Kopiert", icon::CHECK))
                            .size(11.5)
                            .color(tok.connected),
                    );
                }
            });
        });
        ui.add_space(8.0);
    }
}

/// A one-pixel rule between rows. `ui.separator()` would add
/// `item_spacing.y` above and below it, which is most of what makes a
/// settings row twice as tall as a reading needs to be.
fn hairline(ui: &mut Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
}

/// Label left, value right, on one line.
fn plain_row(ui: &mut Ui, row: &Row) {
    let tok = theme::tokens(ui.visuals());
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 4))
        .show(ui, |ui| {
            // `interact_size.y` is 30 app-wide so buttons and combo boxes
            // line up; a row that only prints a value should not reserve it.
            ui.spacing_mut().interact_size.y = 18.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new(&row.label).size(12.5));
                widgets::rtl(ui, 18.0, |ui| value_label(ui, row, &tok));
            });
        });
}

/// The value at the right of a normal row.
fn value_label(ui: &mut Ui, row: &Row, tok: &theme::Tokens) -> egui::Response {
    let color = tone_color(row.tone, tok);
    if row.mono && color.is_none() {
        // A reading, not prose: the chip keeps it aligned with the readings
        // above and below it.
        widgets::chip(ui, &row.value, 0.0)
    } else {
        let mut text = RichText::new(&row.value).size(13.0);
        if row.mono {
            text = text.monospace().size(12.0);
        }
        if let Some(c) = color {
            text = text.color(c);
        }
        ui.label(text)
    }
}

/// The color a [`Tone`] paints with, or `None` for the default text color.
fn tone_color(tone: Tone, tok: &theme::Tokens) -> Option<egui::Color32> {
    match tone {
        Tone::Plain => None,
        Tone::Good => Some(tok.connected),
        Tone::Warn => Some(tok.connecting),
        Tone::Bad => Some(tok.error),
    }
}

// ---------------------------------------------------------------------------
// Hero
// ---------------------------------------------------------------------------

/// The strip above the cards: who we are connected to, since when, how fast.
fn hero(ui: &mut Ui, state: &AppState) {
    let tok = theme::tokens(ui.visuals());
    let (color, word, detail) = match &state.conn {
        ConnStatus::Connected => (tok.connected, "Verbunden", String::new()),
        ConnStatus::Connecting => (tok.connecting, "Verbindet", String::new()),
        ConnStatus::Disconnected { reason, attempt } => (
            tok.connecting,
            "Getrennt",
            format!("{reason} — Versuch {attempt}"),
        ),
        ConnStatus::Fatal(reason) => (tok.error, "Fehler", reason.clone()),
    };

    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().widgets.inactive.bg_stroke.color,
        ))
        .corner_radius(egui::CornerRadius::same(theme::R_CARD))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // A dot with a halo, so the status reads before the words do.
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), 11.0, color.gamma_multiply(0.18));
                ui.painter().circle_filled(rect.center(), 5.5, color);

                ui.vertical(|ui| {
                    ui.label(theme::strong_text(&state.server_name));
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(RichText::new(word).size(12.0).color(color));
                        if let Some(info) = &state.conn_info {
                            ui.label(RichText::new("·").size(12.0).color(tok.dim_text));
                            ui.label(
                                RichText::new(format!(
                                    "seit {}",
                                    fmt_uptime(info.connected_at.elapsed())
                                ))
                                .size(12.0)
                                .color(tok.muted_text),
                            );
                        }
                        if !detail.is_empty() {
                            ui.label(RichText::new("·").size(12.0).color(tok.dim_text));
                            ui.label(RichText::new(&detail).size(12.0).color(tok.muted_text));
                        }
                    });
                });

                widgets::rtl(ui, 26.0, |ui| {
                    widgets::chip(
                        ui,
                        &state.rtt_ms.map(fmt_ms).unwrap_or_else(|| NONE.to_string()),
                        62.0,
                    )
                    .on_hover_text("Rundlaufzeit, von diesem Client gemessen");
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

/// Every card, in rail order — what the report dumps. The window draws one
/// tab's worth at a time (see [`ConnectionView::page`]).
fn build(state: &AppState, stats: &DebugStats, jitter: &str) -> Vec<Card> {
    vec![
        Card::Plain(server_section(state)),
        Card::Plain(transport_section(state, stats)),
        Card::Plain(encryption_section(state)),
        Card::Certs(cert_sections(state)),
        Card::Plain(audio_section(state, stats, jitter)),
        Card::Plain(video_section(state, stats)),
        Card::Plain(participants_section(state)),
    ]
}

fn server_section(state: &AppState) -> Section {
    let info = state.conn_info.as_ref();
    let mut s = Section::new("Server");

    let address = match info {
        Some(i) if i.remote.to_string() != state.server => {
            format!("{} → {}", state.server, i.remote)
        }
        Some(i) => i.remote.to_string(),
        None => state.server.clone(),
    };
    s = s
        .row(Row::mono("Adresse", address))
        .row(Row::mono(
            "Lokale Adresse",
            opt(info.and_then(|i| i.local_ip).map(|ip| ip.to_string())),
        ))
        .row(Row::mono("TLS-Name (SNI)", &state.server_name))
        .row(Row::mono(
            "Protokoll",
            opt(info.map(|i| format!("{} · Minor {}", i.alpn, i.minor))),
        ))
        .row(Row::mono(
            "Fähigkeiten",
            opt(info.map(|i| format!("0x{:08X}", i.caps))),
        ));

    let min_client = opt(info.map(|i| i.min_client.clone()));
    s = s.row(match &state.update_required {
        Some(v) => Row::mono("Mindest-Client", format!("{v} — Update nötig")).tone(Tone::Warn),
        None => Row::mono("Mindest-Client", min_client),
    });

    s = s
        .row(Row::mono(
            "Client-Kennung",
            opt(info.map(|i| i.client_string.clone())),
        ))
        .row(Row::mono(
            "Sitzung",
            opt(state.me.as_ref().map(|m| format!("#{}", m.session))),
        ))
        .row(Row::mono(
            "Benutzer-ID",
            opt(state.me.as_ref().map(|m| m.user_id.to_string())),
        ))
        .row(Row::new(
            "Rolle",
            if state.is_admin {
                "Administrator"
            } else {
                "Teilnehmer"
            },
        ))
        .row(Row::mono(
            "Teilnehmer",
            (state.peers.len() + usize::from(state.me.is_some())).to_string(),
        ))
        .row(Row::mono(
            "Verbunden seit",
            opt(info.map(|i| fmt_uptime(i.connected_at.elapsed()))),
        ))
        .row(Row::mono(
            "Zeitversatz (±RTT/2)",
            opt(state.clock_offset_ms.map(|o| format!("{o:+} ms"))),
        ))
        .row(Row::mono(
            "Keep-Alive / Leerlauf",
            format!(
                "{} s / {} s",
                icu_proto::limits::KEEP_ALIVE_INTERVAL_S,
                icu_proto::limits::MAX_IDLE_TIMEOUT_S
            ),
        ));
    s
}

fn transport_section(state: &AppState, stats: &DebugStats) -> Section {
    let cs = state.conn_stats.as_ref();
    let mut s = Section::new("Transport")
        .row(Row::mono(
            "RTT — eigener Ping",
            opt(state.rtt_ms.map(fmt_ms)),
        ))
        .row(Row::mono(
            "RTT — Server gemessen",
            opt(state.server_rtt_ms.map(|r| format!("{r} ms"))),
        ))
        .row(Row::mono(
            "RTT — QUIC geglättet",
            opt(cs.map(|c| fmt_ms(c.rtt_ms))),
        ))
        .row(Row::mono(
            "RTT — Minimum",
            opt(cs.map(|c| fmt_ms(c.min_rtt_ms))),
        ))
        .row(Row::mono(
            "Warteschlangen-Delay",
            opt(cs.map(|c| fmt_ms(c.cond.queuing_delay_ms))),
        ));

    s = s.row(match cs {
        Some(c) => {
            let pct = c.loss_percent();
            Row::mono(
                "Paketverlust",
                format!("{pct:.2} % ({} von {})", c.lost_packets, c.sent_packets),
            )
            .tone(if pct >= LOSS_WARN_PERCENT * 2.0 {
                Tone::Bad
            } else if pct >= LOSS_WARN_PERCENT {
                Tone::Warn
            } else {
                Tone::Plain
            })
        }
        None => Row::mono("Paketverlust", NONE),
    });

    s = s
        .row(Row::mono(
            "Verlorene Bytes",
            opt(cs.map(|c| fmt_bytes(c.lost_bytes))),
        ))
        .row(Row::mono(
            "Congestion-Events",
            opt(cs.map(|c| c.congestion_events.to_string())),
        ));

    s = s.row(match cs {
        Some(c) if c.black_holes > 0 => {
            Row::mono("Schwarze Löcher", c.black_holes.to_string()).tone(Tone::Bad)
        }
        Some(c) => Row::mono("Schwarze Löcher", c.black_holes.to_string()),
        None => Row::mono("Schwarze Löcher", NONE),
    });

    s = s
        .row(Row::mono(
            "Congestion-Window",
            opt(cs.map(|c| {
                format!(
                    "{}{}",
                    fmt_bytes(c.cwnd),
                    if c.cond.cwnd_limited {
                        " · ausgelastet"
                    } else {
                        ""
                    }
                )
            })),
        ))
        .row(Row::mono(
            "Pfad-MTU",
            opt(cs.map(|c| format!("{} B", c.current_mtu))),
        ))
        .row(Row::mono(
            "Max. Datagramm",
            opt(cs
                .and_then(|c| c.max_datagram_size)
                .map(|n| format!("{n} B"))),
        ))
        .row(Row::mono(
            "Sendepuffer",
            opt(cs.map(|c| {
                format!(
                    "{} von {}",
                    fmt_bytes(c.datagram_queue_bytes),
                    fmt_bytes(icu_proto::limits::DATAGRAM_BUFFER_BYTES as u64)
                )
            })),
        ))
        .row(Row::mono(
            "Durchsatz gesamt",
            opt(cs.map(|c| fmt_updown_kbps(c.tx_kbps, c.rx_kbps))),
        ))
        .row(Row::mono(
            "Übertragen",
            opt(cs.map(|c| {
                format!(
                    "↑ {} · ↓ {}",
                    fmt_bytes(c.udp_tx_bytes),
                    fmt_bytes(c.udp_rx_bytes)
                )
            })),
        ))
        .row(Row::mono(
            "UDP-Datagramme",
            opt(cs.map(|c| format!("↑ {} · ↓ {}", c.udp_tx_datagrams, c.udp_rx_datagrams))),
        ))
        .row(Row::mono(
            "Verlusthinweise",
            opt(cs.map(|c| {
                format!(
                    "Audio {} ‰ · Video {} ‰",
                    c.cond.audio_loss_hint_permille, c.cond.video_drop_hint_permille
                )
            })),
        ))
        .row(Row::mono(
            "Reconnects",
            DebugStats::get(&stats.reconnects).to_string(),
        ))
        .note(
            "„Verlusthinweise“ sind Rückmeldungen der Gegenseite: wie viel von dieser Stimme \
             bzw. diesem Bild bei den anderen verloren geht.",
        );
    s
}

fn encryption_section(state: &AppState) -> Section {
    let info = state.conn_info.as_ref();
    Section::new("Verschlüsselung")
        .row(Row::new("Transport", "QUIC v1 · TLS 1.3 · mTLS").tone(Tone::Good))
        .row(Row::mono("ALPN", opt(info.map(|i| i.alpn.clone()))))
        .row(Row::new("TLS-Versionen", "nur 1.3 — ältere abgelehnt").tone(Tone::Good))
        .row(Row::new("0-RTT / Early Data", "aus").tone(Tone::Good))
        .row(Row::new("Krypto-Anbieter", "ring"))
        .row(
            Row::new(
                "Verfahren (angeboten)",
                "AES-256-GCM · AES-128-GCM · ChaCha20-Poly1305",
            )
            .block_if_long(),
        )
        .row(Row::new(
            "Serverprüfung",
            "eigene CA, beim Beitritt gepinnt",
        ))
        .row(Row::new("Clientprüfung", "Client-Zertifikat (mTLS)"))
        .note(
            "Welches der angebotenen Verfahren tatsächlich ausgehandelt wurde, gibt die \
             QUIC-Bibliothek nicht preis — sie ist hier absichtlich nicht dafür umkonfiguriert. \
             Ton und Bild laufen in Datagrammen bzw. Unistreams genau dieser Verbindung und \
             sind mit deren Sitzungsschlüsseln geschützt; eine zweite Schicht wie SRTP gibt es \
             nicht.",
        )
}

/// The three certificates, in the order the picker offers them.
fn cert_sections(state: &AppState) -> Vec<Section> {
    let info = state.conn_info.as_ref();
    [
        ("Serverzertifikat", info.map(|i| &i.server_cert)),
        ("Eigenes Zertifikat", info.map(|i| &i.client_cert)),
        ("Zertifizierungsstelle", info.map(|i| &i.ca_cert)),
    ]
    .into_iter()
    .map(|(title, cert)| cert_section(title, cert))
    .collect()
}

fn cert_section(title: &str, cert: Option<&CertInfo>) -> Section {
    let Some(c) = cert else {
        return Section::new(title).note("Erst verfügbar, sobald die Verbindung steht.");
    };
    let validity = if c.expires_in_days < 0 {
        Row::new("Gültig bis", format!("{} — abgelaufen", c.not_after)).tone(Tone::Bad)
    } else if c.expires_in_days < CERT_WARN_DAYS {
        Row::new(
            "Gültig bis",
            format!("{} — noch {} Tage", c.not_after, c.expires_in_days),
        )
        .tone(Tone::Warn)
    } else {
        Row::new(
            "Gültig bis",
            format!("{} — noch {} Tage", c.not_after, c.expires_in_days),
        )
    };
    Section::new(title)
        .row(Row::new("Inhaber", &c.subject).block_if_long())
        .row(Row::new("Aussteller", &c.issuer).block_if_long())
        .row(Row::mono("Signatur", &c.sig_alg))
        .row(Row::new("Gültig ab", &c.not_before))
        .row(validity)
        .row(Row::new("Fingerabdruck (SHA-256)", wrap_fingerprint(&c.fingerprint)).copyable())
}

fn audio_section(state: &AppState, stats: &DebugStats, jitter: &str) -> Section {
    let cs = state.conn_stats.as_ref();
    let audio = &state.settings.audio;
    let server = state.conn_info.as_ref().map(|i| i.audio);

    Section::new("Audio")
        .row(Row::new("Codec", "Opus · VoIP · Mono").tone(Tone::Good))
        .row(Row::mono(
            "Abtastrate",
            format!("{} kHz", crate::dsp::opus::RATE / 1000),
        ))
        .row(Row::mono("Framelänge", format!("{} ms", audio.frame_ms)))
        .row(Row::mono("Bitrate", "48 kbit/s · CVBR"))
        .row(Row::mono(
            "Server-Obergrenze",
            opt(server.map(|a| format!("{} kbit/s", a.max_bitrate / 1000))),
        ))
        .row(Row::mono(
            "In-Band-FEC",
            format!(
                "an · Verlusthinweis {} %",
                stats.packet_loss_perc.load(Ordering::Relaxed)
            ),
        ))
        .row(Row::mono("Jitterpuffer p50/p95", jitter))
        .row(Row::mono(
            "Durchsatz",
            opt(cs.map(|c| fmt_updown_kbps(c.audio_tx_kbps, c.audio_rx_kbps))),
        ))
        .row(Row::mono(
            "Übertragen",
            format!(
                "↑ {} · ↓ {}",
                fmt_bytes(DebugStats::get(&stats.audio_bytes_sent)),
                fmt_bytes(DebugStats::get(&stats.audio_bytes_received))
            ),
        ))
        .row(Row::mono(
            "Pakete",
            format!(
                "↑ {} · ↓ {}",
                DebugStats::get(&stats.datagrams_sent),
                DebugStats::get(&stats.datagrams_received)
            ),
        ))
        .row(bad_packets_row(stats))
        .row(Row::mono(
            "Frames",
            format!(
                "{} dekodiert · {} FEC · {} PLC",
                DebugStats::get(&stats.frames_decoded),
                DebugStats::get(&stats.frames_fec),
                DebugStats::get(&stats.frames_plc)
            ),
        ))
        .row(Row::new("Echo-Unterdrückung", aec_text(state)))
        .row(Row::new(
            "Rauschunterdrückung",
            if audio.denoise { "RNNoise, an" } else { "aus" },
        ))
        .row(
            Row::new(
                "Mikrofon",
                device_name(state, DeviceKind::Input, audio.input_device.as_deref()),
            )
            .block_if_long(),
        )
        .row(
            Row::new(
                "Wiedergabe",
                device_name(state, DeviceKind::Output, audio.output_device.as_deref()),
            )
            .block_if_long(),
        )
        .row(Row::new("Backend", state.backend))
        .row(Row::mono(
            "Über-/Unterläufe",
            format!(
                "{} Aufnahme · {} Referenz",
                DebugStats::get(&stats.capture_overruns),
                DebugStats::get(&stats.reference_underruns)
            ),
        ))
        .note(
            "Ton reist in QUIC-Datagrammen dieser Verbindung: unzuverlässig und ungestaut, \
             damit ein verlorenes Paket nie ein späteres aufhält — verschlüsselt ist er \
             trotzdem, mit denselben Sitzungsschlüsseln wie alles andere hier.",
        )
}

/// Malformed datagrams deserve a color once there are any.
fn bad_packets_row(stats: &DebugStats) -> Row {
    let bad = DebugStats::get(&stats.datagrams_bad);
    let row = Row::mono("Fehlerhafte Pakete", bad.to_string());
    if bad > 0 {
        row.tone(Tone::Warn)
    } else {
        row
    }
}

fn video_section(state: &AppState, stats: &DebugStats) -> Section {
    let cs = state.conn_stats.as_ref();
    let tier = VideoTier::from_u8(stats.video_tier.load(Ordering::Relaxed) as u8);
    let tier_text = if tier.paused() {
        format!("T{} · pausiert", tier.as_u8())
    } else {
        format!(
            "T{} · {} kbit/s · jedes {}. Bild",
            tier.as_u8(),
            tier.bitrate() / 1000,
            tier.frame_divisor()
        )
    };

    Section::new("Video")
        .row(Row::new(
            "Eigene Kamera",
            if state.camera_on { "an" } else { "aus" },
        ))
        .row(Row::new("Codec", "H.264 Constrained Baseline · openh264"))
        .row(Row::mono("Aufnahme", capture_format()))
        .row(match tier {
            VideoTier::T0 => Row::mono("Qualitätsstufe", tier_text),
            VideoTier::T4 => Row::mono("Qualitätsstufe", tier_text).tone(Tone::Bad),
            _ => Row::mono("Qualitätsstufe", tier_text).tone(Tone::Warn),
        })
        .row(Row::mono(
            "Stufenwechsel",
            DebugStats::get(&stats.video_tier_changes).to_string(),
        ))
        .row(Row::mono(
            "Durchsatz",
            opt(cs.map(|c| fmt_updown_kbps(c.video_tx_kbps, c.video_rx_kbps))),
        ))
        .row(Row::mono(
            "Übertragen",
            format!(
                "↑ {} · ↓ {}",
                fmt_bytes(DebugStats::get(&stats.video_bytes_sent)),
                fmt_bytes(DebugStats::get(&stats.video_bytes_received))
            ),
        ))
        .row(Row::mono(
            "Bilder",
            format!(
                "↑ {} · ↓ {}",
                DebugStats::get(&stats.video_sent),
                DebugStats::get(&stats.video_received)
            ),
        ))
        .row(Row::mono(
            "Erzwungene Keyframes",
            DebugStats::get(&stats.keyframes_forced).to_string(),
        ))
        .row(Row::mono(
            "Verworfene Streams",
            DebugStats::get(&stats.video_open_uni_drops).to_string(),
        ))
        .note(
            "Jedes Bild bekommt einen eigenen QUIC-Unistream derselben Verbindung. Wird es \
             eng, fällt die Qualitätsstufe automatisch — Video gibt nach, damit die Stimme \
             stabil bleibt.",
        )
}

/// The fixed capture format, or a note when the camera was not compiled in.
fn capture_format() -> String {
    #[cfg(feature = "camera")]
    {
        format!(
            "{}×{} · {} fps",
            crate::camera::WIDTH,
            crate::camera::HEIGHT,
            crate::camera::FPS
        )
    }
    #[cfg(not(feature = "camera"))]
    {
        "nicht einkompiliert".to_string()
    }
}

fn participants_section(state: &AppState) -> Section {
    let mut s = Section::new("Teilnehmer");
    if let Some(me) = &state.me {
        s = s.row(Row::new(
            format!("{} (ich)", me.name),
            format!(
                "{} · {}",
                state
                    .server_rtt_ms
                    .map(|r| format!("{r} ms"))
                    .unwrap_or_else(|| NONE.to_string()),
                badges(me.flags, state.is_admin)
            ),
        ));
    }
    for p in &state.peers {
        s = s.row(Row::new(
            &p.info.name,
            format!(
                "{} · Verlust {} · {}",
                p.rtt_ms
                    .map(|r| format!("{r} ms"))
                    .unwrap_or_else(|| NONE.to_string()),
                p.recv_loss_permille
                    .map(|l| format!("{l} ‰"))
                    .unwrap_or_else(|| NONE.to_string()),
                badges(p.info.flags, p.info.flags.is_admin())
            ),
        ));
    }
    if s.rows.is_empty() {
        s = s.note("Noch niemand hier.");
    } else {
        s = s.note(
            "„Verlust“ ist, was dieser Client von der jeweiligen Stimme nicht rechtzeitig \
             bekommt — gemessen am eigenen Jitterpuffer, nicht am Netz insgesamt.",
        );
    }
    s
}

/// The short state summary behind a participant's name.
fn badges(flags: icu_proto::UserFlags, admin: bool) -> String {
    let mut out = Vec::new();
    if admin {
        out.push("Admin");
    }
    if flags.muted() {
        out.push("stumm");
    }
    if flags.deafened() {
        out.push("taub");
    }
    if flags.camera_on() {
        out.push("Kamera");
    }
    if out.is_empty() {
        "keine".to_string()
    } else {
        out.join(", ")
    }
}

/// What the AEC is doing, including its echo-return-loss enhancement when
/// the processor has an estimate.
fn aec_text(state: &AppState) -> String {
    if !state.settings.audio.aec {
        return "aus".to_string();
    }
    if !state.aec_available {
        return "eingeschaltet, aber nicht verfügbar".to_string();
    }
    if !state.aec_reference {
        return "eingeschaltet, aber keine Referenz (Backend)".to_string();
    }
    let mut text = "AEC3, an".to_string();
    if let Some(erle) = state.aec_stats.erle_db {
        text += &format!(" · ERLE {erle:.1} dB");
    }
    if let Some(delay) = state.aec_stats.delay_ms {
        text += &format!(" · Delay {delay} ms");
    }
    if state.aec_suspect {
        text += " · Echo wird nicht entfernt, Referenzgerät prüfen";
    }
    text
}

/// The human-readable name of the selected device, or „Standard“.
fn device_name(state: &AppState, kind: DeviceKind, id: Option<&str>) -> String {
    let Some(id) = id else {
        return "Standard".to_string();
    };
    state
        .devices
        .iter()
        .find(|d| d.kind == kind && d.id == id)
        .map(|d| d.description.clone())
        .unwrap_or_else(|| format!("{id} (nicht gefunden)"))
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Every row as plain text, for a bug report.
fn report(state: &AppState, cards: &[Card]) -> String {
    let mut out = format!(
        "ICU {} — Verbindungsbericht\nServer: {}\nStatus: {:?}\n",
        env!("CARGO_PKG_VERSION"),
        state.server_name,
        state.conn
    );
    // Every certificate, not just the one the picker happens to show: a
    // report that depends on where a control was left is not a report.
    let sections = cards.iter().flat_map(|c| match c {
        Card::Plain(s) => std::slice::from_ref(s),
        Card::Certs(v) => v.as_slice(),
    });
    for section in sections {
        out.push_str(&format!("\n[{}]\n", section.title));
        for row in &section.rows {
            out.push_str(&format!(
                "  {:<26} {}\n",
                row.label,
                row.value.replace('\n', " ")
            ));
        }
        if let Some(note) = &section.note {
            out.push_str(&format!("  ({note})\n"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Milliseconds with a decimal only while the number is small enough for it
/// to carry information — a loopback link sits well under 1 ms, and `0 ms`
/// says nothing.
fn fmt_ms(ms: f32) -> String {
    if ms < 10.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.0} ms")
    }
}

/// A value or [`NONE`].
fn opt(v: Option<String>) -> String {
    v.unwrap_or_else(|| NONE.to_string())
}

/// Bytes at a human scale. Binary prefixes, because these are buffers and
/// windows, not disk marketing.
fn fmt_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    let n = n as f64;
    if n < KIB {
        format!("{n:.0} B")
    } else if n < KIB * KIB {
        format!("{:.1} KiB", n / KIB)
    } else if n < KIB * KIB * KIB {
        format!("{:.1} MiB", n / (KIB * KIB))
    } else {
        format!("{:.2} GiB", n / (KIB * KIB * KIB))
    }
}

/// `↑ 74 · ↓ 148 kbit/s`, with one decimal while the rate is still small
/// enough for the decimal to mean something.
fn fmt_updown_kbps(tx: f32, rx: f32) -> String {
    let one = |v: f32| {
        if v < 10.0 {
            format!("{v:.1}")
        } else {
            format!("{v:.0}")
        }
    };
    format!("↑ {} · ↓ {} kbit/s", one(tx), one(rx))
}

/// „2 h 14 min“, keeping only the two largest units that carry information.
fn fmt_uptime(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else if s < 86_400 {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    } else {
        format!("{} d {} h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// Breaks a 32-pair fingerprint into two lines, so it fits a card without
/// shrinking the font to unreadable.
fn wrap_fingerprint(fp: &str) -> String {
    let pairs: Vec<&str> = fp.split(':').collect();
    if pairs.len() < 32 {
        return fp.to_string();
    }
    format!("{}\n{}", pairs[..16].join(":"), pairs[16..].join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_binary_prefixes() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024), "1.0 KiB");
        assert_eq!(fmt_bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
        assert!(fmt_bytes(u64::MAX).ends_with("GiB"));
    }

    #[test]
    fn uptime_picks_two_units() {
        assert_eq!(fmt_uptime(Duration::from_secs(9)), "9 s");
        assert_eq!(fmt_uptime(Duration::from_secs(600)), "10 min");
        assert_eq!(fmt_uptime(Duration::from_secs(8040)), "2 h 14 min");
        assert_eq!(fmt_uptime(Duration::from_secs(90_000)), "1 d 1 h");
    }

    #[test]
    fn rates_keep_a_decimal_only_while_small() {
        assert_eq!(fmt_updown_kbps(4.25, 0.0), "↑ 4.2 · ↓ 0.0 kbit/s");
        assert_eq!(fmt_updown_kbps(74.4, 148.6), "↑ 74 · ↓ 149 kbit/s");
    }

    #[test]
    fn fingerprints_wrap_in_half() {
        let fp = (0..32)
            .map(|i| format!("{i:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let wrapped = wrap_fingerprint(&fp);
        let lines: Vec<&str> = wrapped.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].split(':').count(), 16);
        assert_eq!(lines[1].split(':').count(), 16);
        // Anything that is not a full fingerprint is left alone.
        assert_eq!(wrap_fingerprint("unbekannt"), "unbekannt");
    }

    #[test]
    fn badges_list_only_what_is_set() {
        use icu_proto::UserFlags;
        assert_eq!(badges(UserFlags::NONE, false), "keine");
        assert_eq!(badges(UserFlags(UserFlags::MUTED), true), "Admin, stumm");
        assert_eq!(
            badges(UserFlags(UserFlags::CAMERA_ON | UserFlags::DEAFENED), false),
            "taub, Kamera"
        );
    }
}
