//! Every window of the client, drawn headless at the size it opens with, in
//! both themes: the everyday pages must fit without a scroll bar and without
//! growing the window; the expert pages may grow it, but must then fit on a
//! 1080p screen. With
//! `ICU_UI_SHOTS=<dir>` each window is also saved as a PNG for a visual
//! check (needs a GPU or lavapipe):
//!
//! ```sh
//! ICU_UI_SHOTS=/tmp/icu-shots cargo test -p icu-client --test ui_layout -- --nocapture
//! ```
//!
//! The two tests that need a real invite and passport run on Unix only:
//! the in-process server (`common`) does. The rest renders on Windows too
//! (its pages differ: Tastenkürzel, window fitting).

use std::{cell::RefCell, net::SocketAddr, rc::Rc, sync::Arc, time::Instant};

use egui_kittest::{
    kittest::{NodeT, Queryable},
    Harness,
};
use icu_client::{
    audio::{DeviceInfo, DeviceKind},
    config::{ModeSetting, Paths, Settings},
    conninfo::{CertInfo, ConnInfo, ConnStats},
    engine::{AppState, Command, ConnStatus, Peer},
    problem::Problem,
    stats::DebugStats,
    ui::{
        connection::ConnectionView,
        fonts,
        main_window::{MainEnv, MainView},
        settings::SettingsView,
        setup::{SetupView, Step},
        theme,
        user::UserView,
    },
};
use icu_proto::{AudioParams, UserFlags, UserInfo, UserSummary};

#[cfg(unix)]
mod common;

/// A dropped file for tests: `egui::DroppedFile` is a trait (egui 0.36), the
/// same shape egui-winit's native backend implements for a real OS drop.
#[cfg(unix)]
#[derive(Debug)]
struct TestDroppedFile(std::path::PathBuf);

#[cfg(unix)]
impl egui::DroppedFile for TestDroppedFile {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// What the windows open with (`ui::main_window`).
const MAIN: [f32; 2] = icu_client::ui::main_window::MAIN_SIZE;
const SETTINGS: [f32; 2] = icu_client::ui::main_window::SETTINGS_SIZE;
const CONNECTION: [f32; 2] = icu_client::ui::main_window::CONNECTION_SIZE;
const SETUP: [f32; 2] = icu_client::ui::main_window::SETUP_SIZE;
const USER: [f32; 2] = icu_client::ui::main_window::USER_SIZE;
#[cfg(unix)]
const ENROLL: [f32; 2] = [560.0, 600.0];

const THEMES: [&str; 2] = ["light", "dark"];

/// One window body in a real eframe root, the way the app shows it: the main
/// window draws straight into the root, the others into a central panel of
/// their own viewport (`main_window::immediate_window`).
struct Window<F: FnMut(&mut egui::Ui)> {
    body: F,
    panel: bool,
}

impl<F: FnMut(&mut egui::Ui)> eframe::App for Window<F> {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.panel {
            egui::CentralPanel::default().show(root, |ui| (self.body)(ui));
        } else {
            (self.body)(root);
        }
    }
}

fn harness<'a, F: FnMut(&mut egui::Ui) + 'static>(
    size: [f32; 2],
    theme_name: &'static str,
    panel: bool,
    body: F,
) -> Harness<'a, Window<F>> {
    harness_scaled(size, theme_name, panel, 1.0, body)
}

fn harness_scaled<'a, F: FnMut(&mut egui::Ui) + 'static>(
    size: [f32; 2],
    theme_name: &'static str,
    panel: bool,
    pixels_per_point: f32,
    body: F,
) -> Harness<'a, Window<F>> {
    let mut harness = Harness::builder()
        .with_pixels_per_point(pixels_per_point)
        .with_size(egui::vec2(size[0], size[1]))
        .with_theme(egui_theme(theme_name))
        .build_eframe(move |cc| {
            fonts::install(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx, theme_name);
            Window { body, panel }
        });
    harness.run();
    harness
}

/// The kittest theme matching a theme setting: `theme::apply` styles the
/// context's current theme, and kittest would otherwise pick dark.
fn egui_theme(theme_name: &str) -> egui::Theme {
    if theme_name == "dark" {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    }
}

/// Saves the current frame when `ICU_UI_SHOTS` names a directory.
fn shot<S>(harness: &mut Harness<'_, S>, name: &str) {
    let Ok(dir) = std::env::var("ICU_UI_SHOTS") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let image = harness.render().expect("render");
    image.save(format!("{dir}/{name}.png")).unwrap();
}

/// Layout fixtures answer the view's refresh with the same authoritative list
/// on the next frame, so full moderation controls remain covered by fitting tests.
fn user_with_refreshed_list(
    view: &mut UserView,
    ui: &mut egui::Ui,
    state: &mut AppState,
    send: &impl Fn(Command),
) -> icu_client::ui::user::UserOutput {
    let commands = RefCell::new(Vec::new());
    let output = view.ui(ui, state, &|_| None, &|c| commands.borrow_mut().push(c));
    for command in commands.into_inner() {
        if matches!(command, Command::RequestUserList) {
            state.user_list_generation = state.user_list_generation.wrapping_add(1);
            ui.ctx().request_repaint();
        }
        send(command);
    }
    output
}

/// The tallest a window may grow to: a 1080p screen minus title bar and
/// panel.
const MAX_HEIGHT: f32 = 980.0;

/// Everything that overflowed, collected so one run lists all of it.
#[derive(Default)]
struct Overflows(Vec<String>);

impl Overflows {
    fn check(&mut self, what: &str, overflow: f32) {
        eprintln!("{what:<44} overflow {overflow:>7.1} px");
        if overflow > 0.5 {
            self.0.push(format!("{what}: {overflow:.0} px"));
        }
    }

    /// A window that grows itself: after settling it must show everything,
    /// and it may have grown only if `may_grow`, and then only so far.
    fn check_window<S>(
        &mut self,
        what: &str,
        h: &Harness<'_, S>,
        open: [f32; 2],
        may_grow: bool,
        overflow: f32,
    ) {
        let height = h.ctx.content_rect().height();
        let grew = height - open[1];
        eprintln!("{what:<44} overflow {overflow:>7.1} px, grew {grew:>6.1} px");
        if overflow > 0.5 {
            self.0
                .push(format!("{what}: still {overflow:.0} px too tall"));
        }
        if grew > 0.5 && !may_grow {
            self.0
                .push(format!("{what}: grew the window by {grew:.0} px"));
        }
        if height > MAX_HEIGHT {
            self.0.push(format!("{what}: {height:.0} px tall"));
        }
    }

    fn assert_none(self) {
        assert!(
            self.0.is_empty(),
            "needs a scroll bar at its default size:\n  {}",
            self.0.join("\n  ")
        );
    }
}

fn cert(subject: &str, issuer: &str, days: i64) -> CertInfo {
    CertInfo {
        subject: subject.into(),
        issuer: issuer.into(),
        not_before: "2026-09-20".into(),
        not_after: "2027-09-20".into(),
        expires_in_days: days,
        valid_days: 365,
        fingerprint: "3F:A1:07:9C:55:D2:8E:41:0B:77:C3:9A:12:6F:E8:04:\
                      9D:2B:61:F0:34:AA:5E:C7:88:13:DE:92:4C:0A:B5:67"
            .replace(' ', ""),
        sig_alg: "ecdsa-with-SHA256".into(),
        signature_valid: Some(true),
        valid_from: Some(icu_client::net::conn::unix_now() - 86400),
        valid_until: Some(icu_client::net::conn::unix_now() + days * 86400),
        ..CertInfo::default()
    }
}

fn peer(user_id: u32, name: &str, flags: u8, rtt: u16) -> Peer {
    let mut p = Peer::new(
        UserInfo {
            user_id,
            session: user_id as u16 + 10,
            name: name.into(),
            flags: UserFlags(flags),
            corona_color: None,
        },
        100.0,
    );
    p.rtt_ms = Some(rtt);
    p.recv_loss_permille = Some(3);
    p.certificate = Some(Ok(Arc::new(cert(name, "ICU CA", 365))));
    p
}

fn summary(user_id: u32, name: &str) -> UserSummary {
    UserSummary {
        user_id,
        name: name.into(),
        created: "2026-09-21T18:00:00Z".into(),
        is_admin: false,
        disabled: false,
        revoked: false,
        online: false,
        pending: false,
        muted: false,
        deafened: false,
    }
}

/// A connected admin in a room of six, the way a real evening looks.
fn state() -> AppState {
    let mut settings = Settings::default();
    settings.audio.mode = ModeSetting::Ptt;
    // Shows the hold and return-delay sliders on the Overlay page.
    settings.overlay.shy = true;
    let peers = vec![
        peer(
            2,
            "Anna",
            UserFlags::SPEAKING | UserFlags::PTT | UserFlags::VIEWING,
            18,
        ),
        peer(3, "Bernd", UserFlags::MUTED, 34),
        peer(4, "Carla", UserFlags::CAMERA_ON | UserFlags::VIEWING, 61),
        peer(5, "Dennis", UserFlags::DEAFENED | UserFlags::MUTED, 170),
        peer(6, "Eva-Maria Schulte", UserFlags::ADMIN, 22),
    ];
    let mut users: Vec<UserSummary> = [
        (1, "Chris"),
        (2, "Anna"),
        (3, "Bernd"),
        (4, "Carla"),
        (5, "Dennis"),
        (6, "Eva-Maria Schulte"),
        (7, "Frank"),
    ]
    .into_iter()
    .map(|(id, name)| summary(id, name))
    .collect();
    for u in &mut users {
        u.online = u.user_id <= 6;
        u.is_admin = u.user_id == 1 || u.user_id == 6;
    }
    users.push(UserSummary {
        pending: true,
        ..summary(8, "neu-3fa2")
    });
    users.push(UserSummary {
        disabled: true,
        ..summary(9, "Gregor")
    });
    users.push(UserSummary {
        revoked: true,
        ..summary(10, "Hanna")
    });
    let device = |id: &str, description: &str, kind| DeviceInfo {
        id: id.into(),
        description: description.into(),
        kind,
    };
    AppState {
        tuning_status: String::new(),
        ducking_status: Default::default(),
        audio_health: Default::default(),
        can_restore_audio: false,
        conn: ConnStatus::Connected,
        server: "icu.example.org:4711".into(),
        server_name: "icu.example.org".into(),
        me: Some(UserInfo {
            user_id: 1,
            session: 11,
            name: "Chris".into(),
            flags: UserFlags(UserFlags::ADMIN | UserFlags::VIEWING | UserFlags::PTT),
            corona_color: None,
        }),
        peers,
        video_viewers: Vec::new(),
        muted: false,
        deafened: false,
        system_muted: false,
        setup_active: false,
        privacy_active: false,
        privacy_epoch: 0,
        input_clipping: false,
        ptt_down: false,
        sending: false,
        voice: false,
        level: icu_client::dsp::meter::Level {
            peak_db: -12.0,
            rms_db: -24.0,
        },
        vad_prob: 0.1,
        vad_threshold: 0.5,
        rtt_ms: Some(21.4),
        server_rtt_ms: Some(20),
        clock_offset_ms: Some(3),
        conn_info: Some(Arc::new(ConnInfo {
            remote: "203.0.113.7:4711".parse::<SocketAddr>().unwrap(),
            local_ip: Some("192.168.1.20".parse().unwrap()),
            alpn: "icu/1".into(),
            minor: icu_proto::MINOR,
            server_minor: icu_proto::MINOR,
            caps: 0,
            min_client: "0.1.0".into(),
            client_string: "icu/0.1.0 linux".into(),
            audio: AudioParams {
                sample_rate: 48_000,
                frame_ms: 20,
                max_bitrate: 64_000,
            },
            connected_at: Instant::now(),
            server_cert: cert("icu.example.org", "ICU CA icu.example.org", 300),
            client_cert: cert("Chris", "ICU CA icu.example.org", 280),
            ca_cert: cert("ICU CA icu.example.org", "ICU CA icu.example.org", 3400),
        })),
        conn_stats: Some(ConnStats {
            rtt_ms: 21.0,
            min_rtt_ms: 19.0,
            cwnd: 120_000,
            sent_packets: 40_000,
            lost_packets: 12,
            current_mtu: 1452,
            max_datagram_size: Some(1400),
            tx_kbps: 74.0,
            ..ConnStats::default()
        }),
        update_required: None,
        banner: None,
        notice: None,
        name_error: None,
        rename_pending: false,
        is_admin: true,
        last_invite: None,
        invite_error: None,
        user_list: users,
        user_list_generation: 1,
        user_action_error: None,
        restricted_muted: false,
        restricted_deafened: false,
        devices: vec![
            device("mic", "Jabra Evolve2 65 Mono", DeviceKind::Input),
            device("cam", "HD Pro Webcam C920 Analog Stereo", DeviceKind::Input),
            device("hs", "Jabra Evolve2 65 Mono", DeviceKind::Output),
            device(
                "hdmi",
                "GA104 High Definition Audio Controller Digital Stereo (HDMI 2)",
                DeviceKind::Output,
            ),
        ],
        active_input: None,
        active_output: None,
        backend: "pipewire",
        aec_available: true,
        aec_stats: Default::default(),
        aec_reference: true,
        aec_suspect: false,
        aec_no_echo: false,
        aec_reference_silent: false,
        mic_test: false,
        camera_on: false,
        hotkey_backend: "portal".into(),
        settings,
        hotkey_retries: 0,
    }
}

fn main_window(
    state: AppState,
    theme_name: &'static str,
) -> (Harness<'static, impl eframe::App>, Rc<RefCell<f32>>) {
    let overflow = Rc::new(RefCell::new(0.0));
    let out = overflow.clone();
    let (cmd, _rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let mut view = MainView::default();
    let harness = harness(MAIN, theme_name, false, move |root| {
        let env = MainEnv {
            cmd: &cmd,
            video_tier: icu_client::video::VideoTier::T0,
            video: &|_| None,
            self_video: None,
            settings_open: false,
            user_open: None,
            overlay_available: true,
        };
        *out.borrow_mut() = view.ui(root, &state, &env).list_overflow;
    });
    (harness, overflow)
}

/// Four people, the room the main window opens for: oneself and three
/// more, the longest name speaking with every badge column in use, so the
/// narrow window is checked at its widest row.
fn four() -> AppState {
    let mut s = state();
    s.peers = vec![
        peer(2, "Anna", UserFlags::PTT | UserFlags::VIEWING, 18),
        peer(5, "Dennis", UserFlags::DEAFENED | UserFlags::MUTED, 170),
        peer(
            6,
            "Eva-Maria Schulte",
            UserFlags::SPEAKING | UserFlags::ADMIN | UserFlags::CAMERA_ON | UserFlags::VIEWING,
            22,
        ),
    ];
    s
}

#[test]
fn main_window_fits() {
    let mut overflows = Overflows::default();
    let cases: Vec<(&str, AppState)> = vec![
        ("four", four()),
        ("privacy", {
            let mut s = four();
            s.privacy_active = true;
            s.privacy_epoch = 1;
            s.camera_on = true;
            s
        }),
        ("alone", {
            let mut s = state();
            s.peers.clear();
            s
        }),
        ("fatal", {
            let mut s = state();
            s.conn = ConnStatus::Fatal(Problem::plain(
                "Der Server kennt dieses Zertifikat nicht. Bitte eine neue Einladung anfordern.",
            ));
            s.peers.clear();
            s.me = None;
            s
        }),
        ("restricted", {
            let mut s = four();
            s.muted = true;
            s.restricted_muted = true;
            s
        }),
        ("muted-talking", {
            // Muted while the VAD hears speech: the speak surface must stay
            // put ("Stumm"), no flip-flopping label/pulse/tooltip.
            let mut s = four();
            s.muted = true;
            s.voice = true;
            s
        }),
    ];
    for theme_name in THEMES {
        for (name, s) in &cases {
            let (mut h, overflow) = main_window(s.clone(), theme_name);
            let what = format!("main/{name}/{theme_name}");
            overflows.check(&what, *overflow.borrow());
            shot(&mut h, &what.replace('/', "-"));
        }
    }
    overflows.assert_none();
}

/// Privacy remains reachable at minimum size and inhibits the ordinary media
/// switches without narrowing the four existing tiles.
#[test]
fn privacy_button_controls_and_minimum_layout() {
    for theme_name in THEMES {
        let state = Rc::new(RefCell::new(four()));
        let rendered = state.clone();
        let (cmd, mut rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let mut view = MainView::default();
        let mut h = harness(
            icu_client::ui::main_window::MAIN_MIN_SIZE,
            theme_name,
            false,
            move |root| {
                let env = MainEnv {
                    cmd: &cmd,
                    video_tier: icu_client::video::VideoTier::T0,
                    video: &|_| None,
                    self_video: None,
                    settings_open: false,
                    user_open: None,
                    overlay_available: true,
                };
                view.ui(root, &rendered.borrow(), &env);
            },
        );
        h.get_by_label("Privacy").click();
        h.run();
        assert!(std::iter::from_fn(|| rx.try_recv().ok())
            .any(|c| matches!(c, Command::SetPrivacy(true))));
        state.borrow_mut().privacy_active = true;
        h.run();
        assert!(h.get_by_label("Stumm").accesskit_node().is_disabled());
        assert!(h.get_by_label("Kamera").accesskit_node().is_disabled());
        shot(&mut h, &format!("privacy-minimum-{theme_name}"));
        h.get_by_label("Privacy").click();
        h.run();
        assert!(std::iter::from_fn(|| rx.try_recv().ok())
            .any(|c| matches!(c, Command::SetPrivacy(false))));
    }
}

/// A held PTT key is an input, not evidence that audio is leaving. All mute
/// sources must also keep the send surface quiet, including the OS microphone.
#[test]
fn speak_surface_reflects_transmission_and_all_mute_sources() {
    fn visit(
        shape: &egui::Shape,
        rect: egui::Rect,
        labels: &mut Vec<String>,
        fills: &mut Vec<egui::Color32>,
    ) {
        match shape {
            egui::Shape::Vec(children) => {
                for child in children {
                    visit(child, rect, labels, fills);
                }
            }
            egui::Shape::Text(text) if rect.contains(text.pos) => {
                labels.push(text.galley.job.text.clone());
            }
            egui::Shape::Rect(surface) if surface.rect == rect => fills.push(surface.fill),
            _ => {}
        }
    }
    for theme_name in THEMES {
        for mode in [ModeSetting::Ptt, ModeSetting::Vad] {
            for guard in ["none", "local", "system", "setup", "privacy"] {
                let mut s = four();
                s.settings.audio.mode = mode;
                s.ptt_down = true;
                // Even a lagging sending snapshot cannot override a mute.
                s.sending = true;
                s.muted = guard == "local";
                s.system_muted = guard == "system";
                s.setup_active = guard == "setup";
                s.privacy_active = guard == "privacy";
                let (mut h, _) = main_window(s.clone(), theme_name);
                h.run();
                let rect = h.get_by_label_contains("Sendemodus wechseln:").rect();
                let mut labels = Vec::new();
                let mut fills = Vec::new();
                for shape in &h.output().shapes {
                    visit(&shape.shape, rect, &mut labels, &mut fills);
                }
                let expected = if guard == "none" {
                    "Sendet"
                } else if guard == "privacy" {
                    "Privacy an"
                } else {
                    "Stumm"
                };
                assert!(
                    labels.iter().any(|text| text.ends_with(expected)),
                    "{theme_name}/{mode:?}/{guard}: {labels:?}"
                );
                let style = h.ctx.style_of(egui_theme(theme_name));
                let tokens = theme::tokens(&style.visuals);
                fills.clear();
                let mic_rect = h.get_by_label("Stumm").rect();
                for shape in &h.output().shapes {
                    visit(&shape.shape, mic_rect, &mut Vec::new(), &mut fills);
                }
                let fill = if guard == "none" {
                    tokens.accent_tint
                } else {
                    tokens.error_tint
                };
                let fill = if guard == "privacy" {
                    fill.gamma_multiply(style.visuals.disabled_alpha())
                } else {
                    fill
                };
                assert!(
                    fills.contains(&fill),
                    "{theme_name}/{mode:?}/{guard}: {fills:?}"
                );
                if mode == ModeSetting::Ptt && guard == "system" {
                    shot(&mut h, &format!("speak-system-muted-{theme_name}"));
                }
                if mode == ModeSetting::Ptt {
                    let mut view = UserView::new(1);
                    let user = harness(USER, theme_name, true, move |ui| {
                        view.ui(ui, &s, &|_| None, &|_| {});
                    });
                    let expected = if guard == "none" {
                        "Spricht"
                    } else {
                        "Stummgeschaltet"
                    };
                    let mut statuses = Vec::new();
                    for shape in &user.output().shapes {
                        visit(
                            &shape.shape,
                            user.ctx.content_rect(),
                            &mut statuses,
                            &mut Vec::new(),
                        );
                    }
                    assert_eq!(statuses.iter().filter(|text| *text == expected).count(), 2);
                    if guard != "none" {
                        assert!(!statuses.iter().any(|text| text == "Spricht"));
                    }
                }
            }
        }
        for (held, sending, expected) in [(true, false, "Push-to-Talk"), (false, true, "Sendet")] {
            let mut s = four();
            s.settings.audio.mode = ModeSetting::Ptt;
            s.ptt_down = held;
            s.sending = sending;
            let (mut h, _) = main_window(s, theme_name);
            h.run();
            let rect = h.get_by_label_contains("Sendemodus wechseln:").rect();
            let mut labels = Vec::new();
            for shape in &h.output().shapes {
                visit(&shape.shape, rect, &mut labels, &mut Vec::new());
            }
            assert!(
                labels.iter().any(|text| text.ends_with(expected)),
                "{theme_name}/held={held}/sending={sending}: {labels:?}"
            );
        }
        // Received speech indicators can trail a peer's mute/deafen update.
        // The participant row and path view must prefer that authoritative flag.
        for (flag, expected) in [
            (UserFlags::MUTED, "Stummgeschaltet"),
            (UserFlags::SYSTEM_MUTED, "Stummgeschaltet"),
            (UserFlags::DEAFENED, "Hört nicht zu"),
        ] {
            let mut s = four();
            s.peers[0].speaking = true;
            s.peers[0].info.flags = s.peers[0].info.flags.with_bit(flag, true);
            let mut view = UserView::new(2);
            let user = harness(USER, theme_name, true, move |ui| {
                view.ui(ui, &s, &|_| None, &|_| {});
            });
            let mut labels = Vec::new();
            for shape in &user.output().shapes {
                visit(
                    &shape.shape,
                    user.ctx.content_rect(),
                    &mut labels,
                    &mut Vec::new(),
                );
            }
            assert_eq!(labels.iter().filter(|text| *text == expected).count(), 1);
            assert!(!labels.iter().any(|text| text == "Spricht"));
        }
    }
}

/// The settings page follows the same Privacy guard as the main window and
/// ends an existing preview instead of silently restarting it after Privacy.
#[cfg(feature = "camera")]
#[test]
fn privacy_camera_settings_lock_and_stop_preview() {
    for theme_name in THEMES {
        let state = Rc::new(RefCell::new(state()));
        let rendered = state.clone();
        let output = Rc::new(RefCell::new(None));
        let captured = output.clone();
        let mut view = SettingsView::new(
            &state.borrow(),
            Arc::new(DebugStats::default()),
            Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
        );
        let mut h = harness(SETTINGS, theme_name, true, move |ui| {
            *captured.borrow_mut() = Some(view.ui(ui, &rendered.borrow(), false));
        });
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Kamera")
            .click();
        h.run();
        let preview_label = "Vorschau";
        h.get_by_label(&preview_label).click();
        h.run();
        assert!(output.borrow().as_ref().unwrap().camera_preview);

        state.borrow_mut().privacy_active = true;
        h.run();
        assert!(!output.borrow().as_ref().unwrap().camera_preview);
        assert!(output.borrow().as_ref().unwrap().cmds.is_empty());
        assert!(h
            .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Kamera einschalten")
            .accesskit_node()
            .is_disabled());
        assert!(h
            .get_by_label(&preview_label)
            .accesskit_node()
            .is_disabled());
        h.get_by_label_contains("Privacy ist an: Einschalten und Vorschau sind gesperrt.");
        assert!(output.borrow().as_ref().unwrap().overflow <= 0.5);
        shot(&mut h, &format!("settings-camera-privacy-{theme_name}"));

        state.borrow_mut().privacy_active = false;
        h.run();
        assert!(!output.borrow().as_ref().unwrap().camera_preview);
        assert!(!h
            .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Kamera einschalten")
            .accesskit_node()
            .is_disabled());
        assert!(!h
            .get_by_label(&preview_label)
            .accesskit_node()
            .is_disabled());
        h.get_by_label(&preview_label).click();
        h.run();
        assert!(output.borrow().as_ref().unwrap().camera_preview);
    }
}

/// The normal participant self-view renders the same burned-in privacy image.
#[cfg(feature = "camera")]
#[test]
fn privacy_self_video_is_visible_in_the_main_window() {
    use icu_client::camera::{FrameSource, PrivacySource};
    for theme_name in THEMES {
        let frame = PrivacySource::default().next_frame().unwrap();
        let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb);
        let mut texture = None;
        let mut state = four();
        state.privacy_active = true;
        state.privacy_epoch = 1;
        state.camera_on = true;
        let (cmd, _rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let mut view = MainView::default();
        let mut h = harness(MAIN, theme_name, false, move |root| {
            let texture = texture.get_or_insert_with(|| {
                root.ctx()
                    .load_texture("privacy-self", image.clone(), egui::TextureOptions::LINEAR)
            });
            let env = MainEnv {
                cmd: &cmd,
                video_tier: icu_client::video::VideoTier::T0,
                video: &|_| None,
                self_video: Some(egui::load::SizedTexture::from_handle(texture)),
                settings_open: false,
                user_open: None,
                overlay_available: true,
            };
            assert!(view.ui(root, &state, &env).list_overflow <= 0.0);
        });
        h.get_by_label_contains("Sendemodus wechseln: Privacy an");
        shot(&mut h, &format!("privacy-self-video-{theme_name}"));
    }
}

/// More than four people do not fit the window as it opens; after
/// connecting it grows to fit them (`App::fit_to_list`), and a room of six
/// must still fit on a 1080p screen.
#[test]
fn main_window_grows_for_a_bigger_room() {
    let mut overflows = Overflows::default();
    for theme_name in THEMES {
        let (mut h, overflow) = main_window(state(), theme_name);
        let what = format!("main/room/{theme_name}");
        let overflow = *overflow.borrow();
        let height = MAIN[1] + overflow.max(0.0);
        eprintln!("{what:<44} overflow {overflow:>7.1} px, grows to {height:>6.1} px");
        if height > MAX_HEIGHT {
            overflows.0.push(format!("{what}: {height:.0} px tall"));
        }
        shot(&mut h, &what.replace('/', "-"));
    }
    overflows.assert_none();
}

/// The settings pages as the sidebar names them.
const PAGES: [&str; 6] = [
    "Stimme & Hören",
    "Kamera",
    "Overlay & Tasten",
    "App",
    "Administration",
    "Diagnose",
];

#[test]
fn settings_pages_fit() {
    let mut overflows = Overflows::default();
    for theme_name in THEMES {
        for advanced in [false, true] {
            for page in PAGES {
                let mut s = state();
                s.settings.ui.show_advanced = advanced;
                let overflow = Rc::new(RefCell::new(0.0));
                let out = overflow.clone();
                let mut view = SettingsView::new(
                    &s,
                    Arc::new(DebugStats::default()),
                    Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
                );
                let mut h = harness(SETTINGS, theme_name, true, move |ui| {
                    *out.borrow_mut() = view.ui(ui, &s, false).overflow;
                });
                h.get_by_role_and_label(egui::accesskit::Role::Button, page)
                    .click();
                h.run();
                let mode = if advanced { "advanced" } else { "plain" };
                let what = format!("settings/{page}/{mode}/{theme_name}");
                // The member list is as long as the group is big.
                let may_grow = false;
                overflows.check_window(&what, &h, SETTINGS, may_grow, 0.0);
                shot(&mut h, &what.replace('/', "-"));
            }
        }
    }
    overflows.assert_none();
}

/// The Verbindung window's pages as its rail names them.
const CONNECTION_PAGES: [&str; 6] = ["Server", "Netz", "Krypto", "Audio", "Video", "Wer"];

#[test]
fn connection_pages_fit() {
    let mut overflows = Overflows::default();
    for theme_name in THEMES {
        for page in CONNECTION_PAGES {
            let s = state();
            let overflow = Rc::new(RefCell::new(0.0));
            let out = overflow.clone();
            let mut view = ConnectionView::new(Arc::new(DebugStats::default()));
            let mut h = harness(CONNECTION, theme_name, true, move |ui| {
                *out.borrow_mut() = view.ui(ui, &s);
            });
            h.get_by_role_and_label(egui::accesskit::Role::Button, page)
                .click();
            h.run();
            let what = format!("connection/{page}/{theme_name}");
            // The first page fits as the window opens; the long ones grow it.
            let may_grow = page != "Server";
            overflows.check_window(&what, &h, CONNECTION, may_grow, *overflow.borrow());
            shot(&mut h, &what.replace('/', "-"));
        }
    }
    overflows.assert_none();
}

/// The per-user window, opened on the local user (id 1, Push-to-Talk, no
/// moderation card) and on a peer (id 2, admin viewer ⇒ volume and
/// moderation cards both show).
#[test]
fn user_window_fits() {
    let mut overflows = Overflows::default();
    for theme_name in THEMES {
        for (label, user_id) in [("self", 1u32), ("peer", 2u32), ("member", 2u32)] {
            let mut s = state();
            if label == "member" {
                s.is_admin = false;
            }
            let (cmd, _rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
            let overflow = Rc::new(RefCell::new(0.0));
            let out = overflow.clone();
            let mut view = UserView::new(user_id);
            let mut h = harness(USER, theme_name, true, move |ui| {
                let send = |c: Command| {
                    let _ = cmd.send(c);
                };
                *out.borrow_mut() = user_with_refreshed_list(&mut view, ui, &mut s, &send).overflow;
            });
            h.run();
            let what = format!("user/{label}/{theme_name}");
            // Self fits the initial size. The shared card rows and peer
            // controls may grow the window, still within a 1080p desktop.
            overflows.check_window(&what, &h, USER, label != "self", *overflow.borrow());
            if label == "member" {
                assert!(h.ctx.content_rect().height() <= 780.0);
            }
            shot(&mut h, &what.replace('/', "-"));
        }
    }
    overflows.assert_none();
}

/// The first-start window: empty, with an invite (asks for the name) and
/// with a passport (does not, but offers to take its settings along), each
/// checked where it leads before joining.
#[cfg(unix)]
#[test]
fn enroll_window_fits() {
    use icu_client::ui::main_window::EnrollApp;
    use std::sync::Mutex;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (bundle, passport, sending) = rt.block_on(async {
        let server = common::TestServer::start().await;
        let bundle = server.invite_bundle("neu-3fa2").await;
        let (identity, _paths, _dir) = server.enroll("Chris").await;
        let (passport, with_settings) = identity.to_passport_with_settings(&Settings::default());
        assert!(with_settings, "the join page shows the import option");
        // Settings that change when this device can be heard: listed.
        let mut there = Settings::default();
        there.audio.mode = ModeSetting::Ptt;
        there.audio.cue_tx = true;
        there.ui.start_minimized = true;
        let sending = identity.to_passport_with_settings(&there).0;
        (bundle, passport, sending)
    });
    let mut overflows = Overflows::default();
    for theme_name in THEMES {
        for (case, text) in [
            ("paste", None),
            ("invite", Some(bundle.clone())),
            ("passport", Some(passport.clone())),
            ("sending", Some(sending.clone())),
            // The longest refusal the verdict line under the field shows.
            (
                "refused",
                Some(format!(
                    "{}\nAAAA\n{}\n",
                    icu_proto::passport::ARMOR_BEGIN,
                    icu_proto::passport::ARMOR_END
                )),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::new(Some(dir.path().to_path_buf()));
            let result = Arc::new(Mutex::new(None));
            let mut h = Harness::builder()
                .with_size(egui::vec2(ENROLL[0], ENROLL[1]))
                .with_theme(egui_theme(theme_name))
                .build_eframe(move |cc| {
                    fonts::install(&cc.egui_ctx);
                    theme::apply(&cc.egui_ctx, theme_name);
                    let mut app = EnrollApp::new(paths, None, result);
                    if let Some(text) = text {
                        app.paste(text);
                    }
                    app
                });
            // The window asks for a repaint every 100 ms (the registration
            // thread's result), so it never settles on its own.
            h.run_steps(6);
            let what = format!("enroll/{case}/{theme_name}");
            overflows.check_window(&what, &h, ENROLL, false, h.state().overflow());
            shot(&mut h, &what.replace('/', "-"));
            if case == "sending" {
                h.get_by_label_contains("Senden: Taste halten (bisher Automatisch)");
            }
            if case == "invite" {
                // One page: where it leads, the name, one button.
                h.get_by_role_and_label(
                    egui::accesskit::Role::Button,
                    &format!("{}  Beitreten", egui_phosphor::regular::SIGN_IN),
                );
            }
        }
    }
    overflows.assert_none();
}

/// The latest `UpdateSettings` the settings window handed out.
fn last_update(cmds: &[Command]) -> Settings {
    cmds.iter()
        .rev()
        .find_map(|c| match c {
            Command::UpdateSettings(s) => Some((**s).clone()),
            _ => None,
        })
        .expect("an UpdateSettings")
}

/// A hotkey, the tray or the main window change settings while the settings
/// window is open: the window takes the change over instead of writing its
/// stale copy back with the next edit (review 2026-09-23, Netz #4).
#[test]
fn settings_window_keeps_outside_changes() {
    let shared = Rc::new(RefCell::new(state()));
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let (state_in, cmds_out) = (shared.clone(), cmds.clone());
    let mut view = SettingsView::new(
        &shared.borrow(),
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
    );
    let mut h = harness(SETTINGS, "light", true, move |ui| {
        let out = view.ui(ui, &state_in.borrow(), false);
        cmds_out.borrow_mut().extend(out.cmds);
    });

    // The user switches to voice activation; the engine applies it.
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Automatisch")
        .click();
    h.run();
    let sent = last_update(&cmds.borrow());
    assert_eq!(sent.audio.mode, ModeSetting::Vad);
    shared.borrow_mut().settings = sent;
    h.run();

    // A hotkey flips back to push-to-talk behind the window's back.
    shared.borrow_mut().settings.audio.mode = ModeSetting::Ptt;
    h.run();

    h.get_by_role_and_label(egui::accesskit::Role::Button, "App")
        .click();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Senden")
        .scroll_to_me();
    h.run();
    // The next edit in the window keeps push-to-talk.
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Senden")
        .click();
    h.run();
    let sent = last_update(&cmds.borrow());
    assert_eq!(
        sent.audio.mode,
        ModeSetting::Ptt,
        "outside change written back"
    );
    assert!(sent.audio.cue_tx);
}

/// ⚙ → Allgemein: a pasted passport that carries settings offers to take
/// them along, on by default, and names what they change about sending;
/// switched off, „Übernehmen“ sends the identity alone. The CA's
/// fingerprint is on the confirmation (review 2026-09-27, C-3, C-4).
#[cfg(unix)]
#[test]
fn settings_import_takes_the_settings_unless_switched_off() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (passport, ca_der) = rt.block_on(async {
        let server = common::TestServer::start().await;
        let (identity, _paths, _dir) = server.enroll("Chris").await;
        let mut there = Settings::default();
        there.audio.mode = ModeSetting::Ptt;
        there.ui.start_minimized = true;
        (
            identity.to_passport_with_settings(&there).0,
            identity.ca_der,
        )
    });
    let fingerprint = icu_client::conninfo::CertInfo::from_der(&ca_der).fingerprint;
    let first_half = fingerprint
        .split(':')
        .take(16)
        .collect::<Vec<_>>()
        .join(":");
    for take in [true, false] {
        let s = state();
        let dir = tempfile::tempdir().unwrap();
        let mut view = SettingsView::new(
            &s,
            Arc::new(DebugStats::default()),
            Paths::new(Some(dir.path().to_path_buf())),
        );
        view.import_text(passport.clone());
        let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
        let cmds_out = cmds.clone();
        let mut h = harness(SETTINGS, "light", true, move |ui| {
            cmds_out.borrow_mut().extend(view.ui(ui, &s, false).cmds);
        });
        shot(&mut h, &format!("settings-import-{take}"));
        h.get_by_label_contains(&first_half);
        h.get_by_label("Minimiert starten: an (bisher aus)");
        // The switch carries the name of its row.
        if !take {
            h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Einstellungen übernehmen")
                .click_accesskit();
            h.run();
            assert!(
                h.query_by_label("Minimiert starten: an (bisher aus)")
                    .is_none(),
                "nothing changes without the settings"
            );
        }
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Übernehmen")
            .click_accesskit();
        h.run();
        let settings = cmds
            .borrow()
            .iter()
            .find_map(|c| match c {
                Command::ReplaceIdentity(_, settings) => Some(settings.clone()),
                _ => None,
            })
            .expect("a ReplaceIdentity");
        match settings {
            Some(settings) => {
                assert!(take, "switched off, yet the settings came along");
                assert_eq!(settings.audio.mode, ModeSetting::Ptt);
            }
            None => assert!(!take, "the settings were left behind"),
        }
    }
}

/// An invitation imported in ⚙ → Allgemein never brings settings, even
/// one that carries some: the command line and the first-start window
/// leave them behind too (review 2026-09-27, C-3).
#[cfg(unix)]
#[test]
fn settings_import_of_an_invitation_takes_no_settings() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let bundle = rt.block_on(async {
        use rcgen::SigningKey;
        let server = common::TestServer::start().await;
        let mut bundle =
            icu_proto::passport::Passport::decode(&server.invite_bundle("").await).unwrap();
        let mut hot = Settings::default();
        hot.audio.vad_auto = false;
        hot.audio.vad_threshold = 0.0;
        bundle.settings = toml::to_string(&hot).unwrap();
        // Settings belong to the signed payload. This test needs a genuine
        // invitation carrying settings; tampering is rejected before import.
        let key = rcgen::KeyPair::from_pem(
            &std::fs::read_to_string(server.dir.path().join("ca.key")).unwrap(),
        )
        .unwrap();
        bundle.signature = key.sign(&bundle.signable_bytes()).unwrap();
        bundle.encode()
    });
    let s = state();
    let dir = tempfile::tempdir().unwrap();
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(dir.path().to_path_buf())),
    );
    view.import_text(bundle);
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let cmds_out = cmds.clone();
    let mut h = harness(SETTINGS, "light", true, move |ui| {
        cmds_out.borrow_mut().extend(view.ui(ui, &s, false).cmds);
    });
    assert!(h.query_by_label("Einstellungen übernehmen").is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Übernehmen")
        .click_accesskit();
    h.run();
    let settings = cmds
        .borrow()
        .iter()
        .find_map(|c| match c {
            Command::ReplaceIdentity(identity, settings) => {
                assert!(identity.is_registration());
                Some(settings.clone())
            }
            _ => None,
        })
        .expect("a ReplaceIdentity");
    assert!(settings.is_none(), "an invitation brought settings");
}

/// A passport or invite dropped onto the settings import area works the
/// same as pasting it: no separate "browse" affordance is needed, and both
/// ways in end at the same "Übernehmen" confirmation.
#[cfg(unix)]
#[test]
fn settings_import_area_accepts_a_dropped_file() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let bundle = rt.block_on(async {
        let server = common::TestServer::start().await;
        server.invite_bundle("dave").await
    });
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dave.icu");
    std::fs::write(&file, &bundle).unwrap();

    let s = state();
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(dir.path().to_path_buf())),
    );
    view.open_import();
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let cmds_out = cmds.clone();
    let mut h = harness(SETTINGS, "light", true, move |ui| {
        cmds_out.borrow_mut().extend(view.ui(ui, &s, false).cmds);
    });
    h.input_mut().dropped_files =
        vec![Arc::new(TestDroppedFile(file.clone())) as Arc<dyn egui::DroppedFile + Send + Sync>];
    h.run();
    // The drop reached the same preview a paste would: the confirm button
    // is there, and nothing prompted for the field to be filled by hand.
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Übernehmen");
}

/// Switches and sliders carry the name of their row for a screen reader,
/// and a focused slider moves with the arrow keys.
#[test]
fn switches_and_sliders_are_named_and_keyboard_driven() {
    let s = state();
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let cmds_out = cmds.clone();
    let state_in = s.clone();
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
    );
    let mut h = harness(SETTINGS, "light", true, move |ui| {
        cmds_out
            .borrow_mut()
            .extend(view.ui(ui, &state_in, false).cmds);
    });
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Stimme & Hören")
        .click();
    h.run();
    for name in [
        "Rauschunterdrückung",
        "Echo-Unterdrückung",
        "Pegel automatisch",
        "Selbst gleich laut ankommen",
        "Andere gleich laut hören",
        "Stimmoptimierung",
    ] {
        h.get_by_role_and_label(egui::accesskit::Role::CheckBox, name);
    }
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Pegel automatisch")
        .focus();
    h.run();
    shot(&mut h, "settings-keyboard-toggle-focus");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Stimme & Hören")
        .click();
    h.run();
    let before = s.settings.audio.output_volume;
    let volume = h.get_by_role_and_label(egui::accesskit::Role::Slider, "Wiedergabelautstärke");
    volume.focus();
    h.run();
    h.key_press(egui::Key::ArrowRight);
    h.key_press(egui::Key::ArrowRight);
    h.run();
    shot(&mut h, "settings-keyboard-volume-focus");
    let after = last_update(&cmds.borrow()).audio.output_volume;
    assert!(after > before, "volume {before} → {after}");
    h.key_press(egui::Key::ArrowLeft);
    h.run();
    let back = last_update(&cmds.borrow()).audio.output_volume;
    assert!(back < after && back > before, "one step back: {back}");
}

/// The everyday picker sets the own corona color, which others see: it
/// stays usable with the local corona switched off, and resets to default.
#[test]
fn corona_color_is_available_in_plain_settings_and_persists() {
    for theme_name in THEMES {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let mut s = state();
        s.settings.ui.show_advanced = false;
        s.settings.overlay.enabled = true;
        let initial = s.settings.overlay.frame_color;
        let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
        let cmds_out = cmds.clone();
        let state_in = s.clone();
        let mut view = SettingsView::new(&s, Arc::new(DebugStats::default()), paths.clone());
        let mut h = harness(SETTINGS, theme_name, true, move |ui| {
            cmds_out
                .borrow_mut()
                .extend(view.ui(ui, &state_in, false).cmds);
        });
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Overlay & Tasten")
            .click();
        h.run();
        assert!(!h
            .get_by_label("Meine Coronafarbe")
            .accesskit_node()
            .is_disabled());
        shot(&mut h, &format!("corona-plain-{theme_name}"));
        h.get_by_label("Meine Coronafarbe").scroll_to_me();
        h.run();
        h.get_by_label("Meine Coronafarbe").click();
        h.run();
        h.get_all_by_role(egui::accesskit::Role::SpinButton)
            .next()
            .expect("RGB editor")
            .focus();
        h.run();
        h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
        h.event(egui::Event::Text("200".into()));
        h.key_press(egui::Key::Enter);
        h.run();
        shot(&mut h, &format!("corona-picker-{theme_name}"));
        let color = last_update(&cmds.borrow()).overlay.frame_color;
        assert_ne!(color, initial, "picker sends a live settings update");
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Sprech-Corona")
            .click();
        h.run();
        assert!(
            !h.get_by_label("Meine Coronafarbe")
                .accesskit_node()
                .is_disabled(),
            "others still see the own color"
        );
        let saved = last_update(&cmds.borrow());
        assert!(!saved.overlay.speaking_frame);
        assert_eq!(saved.overlay.frame_color, color, "off retains the color");
        saved.save(&paths).unwrap();
        let restored = Settings::load(&paths).unwrap();
        assert_eq!(
            restored.overlay.frame_color, color,
            "reload retains the color"
        );
        h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Sprech-Corona")
            .click();
        h.run();
        assert_eq!(last_update(&cmds.borrow()).overlay.frame_color, color);
        h.get_by_label("Standardfarbe").click();
        h.run();
        assert_eq!(
            last_update(&cmds.borrow()).overlay.frame_color,
            icu_client::config::DEFAULT_FRAME_COLOR
        );
    }
}

/// The return-delay control is reachable and updates the persisted draft.
#[test]
fn shy_return_delay_slider_is_named_and_keyboard_driven() {
    let mut s = state();
    s.settings.ui.show_advanced = true;
    s.settings.overlay.enabled = true;
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let cmds_out = cmds.clone();
    let state_in = s.clone();
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
    );
    let mut h = harness([SETTINGS[0], 1000.0], "light", true, move |ui| {
        cmds_out
            .borrow_mut()
            .extend(view.ui(ui, &state_in, false).cmds);
    });
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Overlay & Tasten")
        .click();
    h.run();
    let delay = h.get_by_role_and_label(egui::accesskit::Role::Slider, "Zurück nach");
    delay.focus();
    h.run();
    h.key_press(egui::Key::ArrowRight);
    h.run();
    assert!(last_update(&cmds.borrow()).overlay.shy_return_ms > 3000);
}

/// The mic test runs on the Klang page too, where Stimmoptimierung and its
/// Stärke are switched: until 2026-09-28 the tab change stopped it, and the
/// presets could only be compared in silence. Other pages still stop it.
#[test]
fn mic_test_stays_on_the_klang_page() {
    let test = "Testen";
    let s = state();
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let on_test_page = Rc::new(RefCell::new(false));
    let (cmds_out, page_out, state_in) = (cmds.clone(), on_test_page.clone(), s.clone());
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
    );
    let mut h = harness(SETTINGS, "light", true, move |ui| {
        let out = view.ui(ui, &state_in, false);
        *page_out.borrow_mut() = out.mic_test_page;
        cmds_out.borrow_mut().extend(out.cmds);
    });
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Stimme & Hören")
        .click();
    h.run();
    assert!(*on_test_page.borrow(), "Klang stops the mic test");
    h.get_by_role_and_label(egui::accesskit::Role::Button, &test)
        .click();
    h.run();
    assert!(
        cmds.borrow()
            .iter()
            .any(|c| matches!(c, Command::SetMicTest(true))),
        "„Testen“ on Klang starts no mic test"
    );
    h.get_by_role_and_label(egui::accesskit::Role::Button, "App")
        .click();
    h.run();
    assert!(
        !*on_test_page.borrow(),
        "Allgemein keeps the mic test running"
    );
}

/// No way to read keys: the banner says what to do and switches to voice
/// activation with one click, on the Tastenkürzel page and in the setup.
#[test]
fn no_hotkey_backend_offers_voice_activation() {
    let switch = "Stattdessen automatisch senden";
    let mut s = state();
    s.hotkey_backend = "none".into();
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let (cmds_out, state_in) = (cmds.clone(), s.clone());
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
    );
    let mut h = harness(SETTINGS, "light", true, move |ui| {
        cmds_out
            .borrow_mut()
            .extend(view.ui(ui, &state_in, false).cmds);
    });
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Overlay & Tasten")
        .click();
    h.run();
    if cfg!(target_os = "linux") {
        h.get_by_label_contains("sudo usermod -aG input $USER");
    }
    h.get_by_role_and_label(egui::accesskit::Role::Button, &switch)
        .scroll_to_me();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, &switch)
        .click();
    h.run();
    assert_eq!(last_update(&cmds.borrow()).audio.mode, ModeSetting::Vad);

    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let cmds_out = cmds.clone();
    s.settings.ui.setup_done = false;
    let mut view = SetupView::new(&s);
    let mut h = harness(SETUP, "light", true, move |ui| {
        cmds_out.borrow_mut().extend(view.ui(ui, &s).cmds);
    });
    h.get_by_role_and_label(egui::accesskit::Role::Button, &next_label())
        .click();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, &switch)
        .click();
    h.run();
    assert_eq!(last_update(&cmds.borrow()).audio.mode, ModeSetting::Vad);
}

/// The passport's warning names the clipboard, which „Kopieren“ uses.
#[test]
fn the_passport_warning_names_the_clipboard() {
    let s = state();
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
    );
    let mut h = harness(SETTINGS, "light", true, move |ui| {
        view.ui(ui, &s, false);
    });
    h.get_by_role_and_label(egui::accesskit::Role::Button, "App")
        .click();
    h.run();
    h.get_by_label_contains("Zwischenablage-Verlauf");
}

/// „Weiter“ in the setup's bottom bar.
fn next_label() -> String {
    format!("Weiter  {}", egui_phosphor::regular::ARROW_RIGHT)
}

/// The setup's four steps, walked through with „Weiter“ the way a new user
/// does: each fits the window it opens with. Sprechen once for
/// push-to-talk and once for voice activation; Klang with the optimizer on,
/// its tallest form. A banner about the keys may grow the window.
#[test]
fn setup_steps_fit() {
    let mut overflows = Overflows::default();
    for theme_name in THEMES {
        for mode in [ModeSetting::Ptt, ModeSetting::Vad] {
            let mut s = state();
            s.settings.ui.setup_done = false;
            s.settings.audio.mode = mode;
            s.settings.audio.voice_opt = true;
            let overflow = Rc::new(RefCell::new(0.0));
            let step = Rc::new(RefCell::new(Step::Devices));
            let (out, at) = (overflow.clone(), step.clone());
            let mut view = SetupView::new(&s);
            let mut h = harness(SETUP, theme_name, true, move |ui| {
                *out.borrow_mut() = view.ui(ui, &s).overflow;
                *at.borrow_mut() = view.step();
            });
            for i in 0..Step::ALL.len() {
                let now = *step.borrow();
                assert_eq!(now, Step::ALL[i]);
                let what = format!("setup/{now:?}/{mode:?}/{theme_name}");
                overflows.check_window(&what, &h, SETUP, false, *overflow.borrow());
                shot(&mut h, &what.replace('/', "-"));
                if i + 1 < Step::ALL.len() {
                    h.get_by_role_and_label(egui::accesskit::Role::Button, &next_label())
                        .click();
                    h.run();
                }
            }
        }
        for backend in ["starting", "", "none", "portal-declined"] {
            let mut s = state();
            s.hotkey_backend = backend.into();
            let overflow = Rc::new(RefCell::new(0.0));
            let out = overflow.clone();
            let mut view = SetupView::new(&s);
            let mut h = harness(SETUP, theme_name, true, move |ui| {
                *out.borrow_mut() = view.ui(ui, &s).overflow;
            });
            h.get_by_role_and_label(egui::accesskit::Role::Button, &next_label())
                .click();
            h.run();
            let what = format!("setup/Speak/backend-{backend}/{theme_name}");
            overflows.check_window(&what, &h, SETUP, true, *overflow.borrow());
            shot(&mut h, &what.replace('/', "-"));
        }
    }
    overflows.assert_none();
}

/// Setup blocks transmission independently of user mute; finishing releases it.
#[test]
fn setup_finish_marks_done_and_releases_transmission_guard() {
    let shared = Rc::new(RefCell::new(state()));
    shared.borrow_mut().settings.ui.setup_done = false;
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let (state_in, cmds_out) = (shared.clone(), cmds.clone());
    let mut view = SetupView::new(&shared.borrow());
    let mut h = harness(SETUP, "light", true, move |ui| {
        let state = state_in.borrow();
        let out = view.ui(ui, &state);
        cmds_out.borrow_mut().extend(out.cmds);
        if out.done {
            cmds_out.borrow_mut().extend(view.close(&state));
        }
    });
    assert!(
        matches!(cmds.borrow().first(), Some(Command::SetSetupActive(true))),
        "muted while the setup is open"
    );
    // The engine applies it.
    shared.borrow_mut().setup_active = true;
    h.run();

    for _ in 1..Step::ALL.len() {
        h.get_by_role_and_label(egui::accesskit::Role::Button, &next_label())
            .click();
        h.run();
    }
    assert!(
        !cmds
            .borrow()
            .iter()
            .any(|c| matches!(c, Command::SetSetupActive(false))),
        "still muted before „Fertig“"
    );
    h.get_by_role_and_label(
        egui::accesskit::Role::Button,
        &format!("{}  Fertig", egui_phosphor::regular::CHECK),
    )
    .click();
    h.run();
    assert!(last_update(&cmds.borrow()).ui.setup_done);
    assert!(
        matches!(cmds.borrow().last(), Some(Command::SetSetupActive(false))),
        "unmuted by „Fertig“"
    );
}

/// Quitting releases the temporary guard without changing the persisted user mute.
#[test]
fn quitting_with_the_setup_open_releases_transmission_guard() {
    let shared = Rc::new(RefCell::new(state()));
    shared.borrow_mut().settings.ui.setup_done = false;
    let view = Rc::new(RefCell::new(SetupView::new(&shared.borrow())));
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let (state_in, view_in, cmds_out) = (shared.clone(), view.clone(), cmds.clone());
    let mut h = harness(SETUP, "light", true, move |ui| {
        let out = view_in.borrow_mut().ui(ui, &state_in.borrow());
        cmds_out.borrow_mut().extend(out.cmds);
    });
    assert!(matches!(
        cmds.borrow().first(),
        Some(Command::SetSetupActive(true))
    ));
    shared.borrow_mut().setup_active = true;
    h.run();
    drop(h);
    let state = shared.borrow();
    assert!(matches!(
        view.borrow_mut().release_mute(&state),
        Some(Command::SetSetupActive(false))
    ));
    assert!(
        view.borrow_mut().release_mute(&state).is_none(),
        "only once"
    );
}

/// „Überspringen“ on the first step ends the setup for good without
/// touching anything else; a user who was muted already stays muted.
#[test]
fn setup_skip_keeps_everything_and_an_earlier_mute() {
    let mut s = state();
    s.settings.ui.setup_done = false;
    s.muted = true;
    let before = s.settings.clone();
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let cmds_out = cmds.clone();
    let mut view = SetupView::new(&s);
    let mut h = harness(SETUP, "dark", true, move |ui| {
        let out = view.ui(ui, &s);
        cmds_out.borrow_mut().extend(out.cmds);
        if out.done {
            cmds_out.borrow_mut().extend(view.close(&s));
        }
    });
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Überspringen")
        .click();
    h.run();
    let sent = last_update(&cmds.borrow());
    assert!(sent.ui.setup_done);
    assert_eq!(
        Settings {
            ui: before.ui.clone(),
            ..sent.clone()
        },
        before,
        "nothing but the flag changed"
    );
    assert!(
        !cmds
            .borrow()
            .iter()
            .any(|c| matches!(c, Command::SetMuted(_))),
        "a mute from before the setup is not the setup's to lift"
    );
}

/// Certificate selection, copy, ordinary-member controls, missing data and
/// narrow/long-name layouts in both themes.
#[test]
fn user_path_details_and_missing_data() {
    for theme_name in THEMES {
        for (case, width, missing, long) in [
            ("normal", 900.0, false, false),
            ("narrow-long", 760.0, false, true),
            ("missing", 760.0, true, false),
            ("pending", 760.0, false, false),
            ("invalid", 760.0, false, false),
            ("expiring", 760.0, false, false),
        ] {
            let mut s = state();
            s.is_admin = false;
            if long {
                s.peers[0].info.name = "Alexandra-Christiane mit einem langen Anzeigenamen".into();
                s.server_name = "voice-server-mit-langem-namen.example.org".into();
            }
            if missing {
                s.peers[0].certificate = Some(Err("Zertifikat unlesbar".into()));
                s.peers[0].rtt_ms = None;
                s.peers[0].recv_loss_permille = None;
                s.server_rtt_ms = None;
            }
            match case {
                "pending" => s.peers[0].certificate = None,
                "invalid" => {
                    let mut c = cert("Anna", "ICU CA", 365);
                    c.signature_valid = Some(false);
                    s.peers[0].certificate = Some(Ok(Arc::new(c)));
                }
                "expiring" => {
                    s.peers[0].certificate = Some(Ok(Arc::new(cert("Anna", "ICU CA", 7))))
                }
                _ => {}
            }
            let mut view = UserView::new(2);
            let height = if width < 900.0 { 520.0 } else { 640.0 };
            let mut h = harness([width, height], theme_name, true, move |ui| {
                ui.ctx().input_mut(|input| {
                    for viewport in input.raw.viewports.values_mut() {
                        viewport.monitor_size = Some(egui::vec2(1920.0, 1080.0));
                    }
                });
                user_with_refreshed_list(&mut view, ui, &mut s, &|_| {});
                assert!(
                    ui.min_rect().right() <= ui.max_rect().right() + 0.5,
                    "user path overflows horizontally"
                );
            });
            h.run();
            shot(&mut h, &format!("user-path-{case}-{theme_name}"));
            for (name, label) in [
                ("peer", "Nutzerzertifikat"),
                ("server", "Serverzertifikat"),
                ("own", "Dein Zertifikat"),
            ] {
                let glyph = egui_phosphor::regular::CERTIFICATE;
                h.get_by_label(&format!("{glyph} {label}")).click();
                h.run();
                shot(&mut h, &format!("user-path-{case}-{name}-{theme_name}"));
                // Same button collapses the details again.
                h.get_by_label(&format!("{glyph} {label}")).click();
                h.run();
            }
        }
    }
}

/// All cards share their bounds and row baselines even when one column
/// wraps. Inspect rendered geometry rather than duplicating layout formulas.
#[test]
fn user_path_cards_align_with_wrapping_and_scaling() {
    fn rects(shapes: &[egui::epaint::ClippedShape], out: &mut Vec<egui::Rect>) {
        fn visit(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Rect(r)
                    if r.rect.width() > 150.0
                        && r.rect.height() > 100.0
                        && r.rect.top() < 100.0 =>
                {
                    out.push(r.rect)
                }
                egui::Shape::Vec(children) => children.iter().for_each(|s| visit(s, out)),
                _ => {}
            }
        }
        for shape in shapes {
            visit(&shape.shape, out);
        }
    }
    for theme_name in THEMES {
        for (case, width, scale) in [
            ("wide", 900.0, 1.0),
            ("narrow", 760.0, 1.0),
            ("125", 760.0, 1.25),
            ("150", 760.0, 1.5),
        ] {
            let mut s = state();
            s.peers[0].info.name = "Alexandra-Christiane mit einem langen Anzeigenamen".into();
            s.peers[0].info.flags =
                UserFlags(UserFlags::CAMERA_ON | UserFlags::VIEWING | UserFlags::MUTED);
            s.peers[0].speaking = false;
            s.server_name = "voice-server-mit-langem-namen.example.org".into();
            s.server = "voice-server-mit-langem-namen.example.org:4711".into();
            let mut view = UserView::new(2);
            let mut h = harness_scaled([width, 980.0], theme_name, true, scale, move |ui| {
                user_with_refreshed_list(&mut view, ui, &mut s, &|_| {});
                assert!(ui.min_rect().right() <= ui.max_rect().right() + 0.5);
            });
            h.run();
            let mut cards = Vec::new();
            rects(&h.output().shapes, &mut cards);
            cards.retain(|r| r.width() < width / 2.0);
            cards.sort_by(|a, b| a.left().total_cmp(&b.left()));
            assert_eq!(cards.len(), 3, "{case}/{theme_name}: {cards:?}");
            for r in &cards[1..] {
                assert!((r.width() - cards[0].width()).abs() < 0.5);
                assert!((r.top() - cards[0].top()).abs() < 0.5);
                assert!((r.bottom() - cards[0].bottom()).abs() < 0.5);
            }
            let roles = ["Mitglied", "Medienvermittlung", "Admin"]
                .map(|label| h.get_by_label(label).rect());
            for r in &roles[1..] {
                assert!((r.top() - roles[0].top()).abs() < 0.5);
            }
            let certs: Vec<_> = h
                .get_all_by_label_contains("CA-Signatur geprüft")
                .map(|n| n.rect())
                .collect();
            assert_eq!(certs.len(), 3);
            for r in &certs[1..] {
                assert!((r.top() - certs[0].top()).abs() < 0.5);
            }
            for (i, role) in roles.iter().enumerate() {
                assert!(cards[i].contains_rect(*role));
            }
            let mut name_top = None;
            for (i, name) in [
                "Alexandra-Christiane mit einem langen Anzeigenamen",
                "voice-server-mit-langem-namen.example.org",
                "Chris",
            ]
            .into_iter()
            .enumerate()
            {
                let name = h.get_by_label(name).rect();
                let top = *name_top.get_or_insert(name.top());
                assert!(
                    (name.top() - top).abs() < 0.5,
                    "{case}: name baselines differ"
                );
                assert!(
                    cards[i].contains_rect(name),
                    "{case}: name escapes card {i}"
                );
                assert!(
                    name.bottom() < roles[i].top(),
                    "{case}: name overlaps role {i}"
                );
            }
            let selectors = ["Nutzerzertifikat", "Serverzertifikat", "Dein Zertifikat"];
            for (i, label) in selectors.into_iter().enumerate() {
                let selector = h
                    .get_by_label(&format!("{} {label}", egui_phosphor::regular::CERTIFICATE))
                    .rect();
                assert!((selector.center().x - cards[i].center().x).abs() < 1.0);
            }
            shot(&mut h, &format!("user-grid-{case}-{theme_name}"));
        }
    }
}

#[test]
fn user_path_requests_once_and_retries_on_new_connection() {
    let mut s = state();
    s.is_admin = false;
    s.peers[0].certificate = None;
    let session = s.peers[0].info.session;
    let s = Rc::new(RefCell::new(s));
    let shared = s.clone();
    let sent = Rc::new(RefCell::new(Vec::new()));
    let captured = sent.clone();
    let mut view = UserView::new(2);
    let mut h = harness(USER, "dark", true, move |ui| {
        view.ui(ui, &shared.borrow(), &|_| None, &|cmd| {
            captured.borrow_mut().push(cmd)
        });
    });
    h.run();
    h.run();
    assert_eq!(
        sent.borrow()
            .iter()
            .filter(|c| matches!(c, Command::RequestPeerCertificate(s) if *s == session))
            .count(),
        1
    );
    Arc::make_mut(s.borrow_mut().conn_info.as_mut().unwrap()).connected_at +=
        std::time::Duration::from_secs(1);
    h.run();
    assert_eq!(
        sent.borrow()
            .iter()
            .filter(|c| matches!(c, Command::RequestPeerCertificate(s) if *s == session))
            .count(),
        2
    );
}

#[test]
fn user_path_keeps_volume_moderation_and_certificate_copy() {
    let s = Rc::new(RefCell::new(state()));
    let current = s.clone();
    let sent = Rc::new(RefCell::new(Vec::new()));
    let captured = sent.clone();
    let expected = s.borrow().peers[0]
        .certificate
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .fingerprint
        .clone();
    let clipboard = Rc::new(RefCell::new(Vec::new()));
    let captured_clipboard = clipboard.clone();
    let mut view = UserView::new(2);
    let mut h = harness(USER, "dark", true, move |ui| {
        view.ui(ui, &current.borrow(), &|_| None, &|c| {
            captured.borrow_mut().push(c)
        });
        ui.ctx().output(|o| {
            for command in &o.commands {
                if let egui::OutputCommand::CopyText(text) = command {
                    captured_clipboard.borrow_mut().push(text.clone());
                }
            }
        });
    });
    s.borrow_mut().user_list_generation += 1;
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Für mich stummschalten")
        .click();
    h.run();
    assert!(sent
        .borrow()
        .iter()
        .any(|c| matches!(c, Command::SetVolume(2, v) if *v == 0.0)));
    s.borrow_mut().peers[0].volume = 0.0;
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Wieder hören")
        .click();
    h.run();
    assert!(sent
        .borrow()
        .iter()
        .any(|c| matches!(c, Command::SetVolume(2, v) if *v == 100.0)));
    h.get_by_label(&format!(
        "{}  Stummschalten",
        egui_phosphor::regular::MICROPHONE_SLASH
    ))
    .click();
    h.run();
    assert!(sent.borrow().iter().any(|c| matches!(
        c,
        Command::SetUserRestriction {
            user_id: 2,
            muted: true,
            deafened: false
        }
    )));
    h.get_by_label(&format!(
        "{} Nutzerzertifikat",
        egui_phosphor::regular::CERTIFICATE
    ))
    .click();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Fingerabdruck kopieren")
        .click();
    h.run();
    assert!(
        clipboard.borrow().contains(&expected),
        "copy keeps the complete unwrapped SHA-256 fingerprint"
    );
}

#[test]
fn moderation_waits_for_loaded_restrictions_and_preserves_deafen() {
    for theme_name in THEMES {
        for cached in [false, true] {
            let mut initial = state();
            if !cached {
                initial.user_list.clear();
            }
            let current = Rc::new(RefCell::new(initial));
            let state_in = current.clone();
            let sent = Rc::new(RefCell::new(Vec::new()));
            let captured = sent.clone();
            let mut view = UserView::new(2);
            let mut h = harness([USER[0], 980.0], theme_name, true, move |ui| {
                view.ui(ui, &state_in.borrow(), &|_| None, &|c| {
                    captured.borrow_mut().push(c)
                });
            });
            assert!(sent
                .borrow()
                .iter()
                .any(|c| matches!(c, Command::RequestUserList)));
            assert!(h
                .query_by_label(&format!(
                    "{}  Stummschalten",
                    egui_phosphor::regular::MICROPHONE_SLASH
                ))
                .is_none());
            h.get_by_label("Moderationsstatus wird geladen …");
            shot(&mut h, &format!("moderation-loading-{cached}-{theme_name}"));

            let mut restricted = summary(2, "Anna");
            restricted.deafened = true;
            current.borrow_mut().user_list = vec![restricted];
            // Even a populated (possibly stale) list is not a response to this view.
            h.run();
            h.get_by_label("Moderationsstatus wird geladen …");
            current.borrow_mut().user_list_generation += 1;
            h.run();
            h.get_by_label(&format!(
                "{}  Stummschalten",
                egui_phosphor::regular::MICROPHONE_SLASH
            ))
            .click();
            h.run();
            assert!(sent.borrow().iter().any(|c| matches!(
                c,
                Command::SetUserRestriction {
                    user_id: 2,
                    muted: true,
                    deafened: true
                }
            )));
            assert!(!sent.borrow().iter().any(|c| matches!(
                c,
                Command::SetUserRestriction {
                    deafened: false,
                    ..
                }
            )));
            shot(
                &mut h,
                &format!("moderation-preserves-deafen-{cached}-{theme_name}"),
            );
        }
    }
}

#[test]
fn own_participant_shows_system_microphone_mute() {
    for theme_name in THEMES {
        let mut s = state();
        s.peers.truncate(1);
        s.system_muted = true;
        let (mut h, _) = main_window(s, theme_name);
        h.get_by_label("Chris").hover();
        h.run_steps(4);
        h.get_by_label("stummgeschaltet");
        shot(&mut h, &format!("own-system-mute-{theme_name}"));
    }
}

#[test]
fn context_moderation_waits_for_its_refresh_and_preserves_deafen() {
    let current = Rc::new(RefCell::new(state()));
    let state_in = current.clone();
    let (cmd, mut commands) = tokio::sync::mpsc::unbounded_channel();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let opened_in = opened.clone();
    let mut view = MainView::default();
    let mut h = harness([MAIN[0], 700.0], "dark", false, move |ui| {
        let env = MainEnv {
            cmd: &cmd,
            video_tier: icu_client::video::VideoTier::T0,
            video: &|_| None,
            self_video: None,
            settings_open: false,
            user_open: None,
            overlay_available: true,
        };
        if let Some(id) = view.ui(ui, &state_in.borrow(), &env).open_user {
            opened_in.borrow_mut().push(id);
        }
    });
    h.get_by_label("Anna").click();
    h.run();
    assert_eq!(
        *opened.borrow(),
        [2],
        "the name opens the same peer view as the row"
    );
    h.get_by_label("Anna").click_secondary();
    h.run();
    h.get_by_label("Moderationsstatus wird geladen …");
    assert!(matches!(commands.try_recv(), Ok(Command::RequestUserList)));
    assert!(h
        .query_by_label(&format!(
            "{}  Stummschalten",
            egui_phosphor::regular::MICROPHONE_SLASH
        ))
        .is_none());

    {
        let mut state = current.borrow_mut();
        state
            .user_list
            .iter_mut()
            .find(|u| u.user_id == 2)
            .unwrap()
            .deafened = true;
        state.user_list_generation += 1;
    }
    h.run();
    h.get_by_label(&format!(
        "{}  Stummschalten",
        egui_phosphor::regular::MICROPHONE_SLASH
    ))
    .click();
    h.run();
    assert!(matches!(
        commands.try_recv(),
        Ok(Command::SetUserRestriction {
            user_id: 2,
            muted: true,
            deafened: true
        })
    ));
}

#[test]
fn autotune_details_keep_controls_accessible_and_independent() {
    for theme in THEMES {
        let mut s = state();
        s.tuning_status =
            "Eingeregelt · Hardware · System +2.0 dB · Software +0.8 dB · Peer +0.5 dB (aktiv)"
                .into();
        let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
        let output = cmds.clone();
        let mut view = SettingsView::new(
            &s,
            Arc::new(DebugStats::default()),
            Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
        );
        let mut h = harness(SETTINGS, theme, true, move |ui| {
            output.borrow_mut().extend(view.ui(ui, &s, false).cmds);
        });
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Stimme & Hören")
            .click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Details")
            .scroll_to_me();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Details")
            .click();
        h.run();
        h.get_by_role_and_label(
            egui::accesskit::Role::CheckBox,
            "Systemmikrofon automatisch",
        );
        shot(&mut h, &format!("autotune-details-{theme}"));
        h.get_by_role_and_label(
            egui::accesskit::Role::CheckBox,
            "Pegel mit Empfangsfeedback",
        )
        .click_accesskit();
        h.run();
        let settings = last_update(&cmds.borrow()).clone();
        assert!(!settings.audio.peer_assistance);
        assert!(settings.audio.tune_system_mic && settings.audio.auto_level);
    }
}

#[test]
fn last_good_audio_actions_work_even_with_denoise_disabled() {
    for theme_name in THEMES {
        let mut s = state();
        s.settings.audio.denoise = false;
        s.can_restore_audio = true;
        s.audio_health.profile_saved = true;
        s.audio_health.profile_saved_at = Some(1_800_000_000);
        let commands = Rc::new(RefCell::new(Vec::new()));
        let collected = commands.clone();
        let mut view = SettingsView::new(
            &s,
            Arc::new(DebugStats::default()),
            Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
        );
        let mut h = harness(SETTINGS, theme_name, true, move |ui| {
            collected.borrow_mut().extend(view.ui(ui, &s, false).cmds);
        });
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Stimme & Hören")
            .click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Details")
            .scroll_to_me();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Details")
            .click();
        h.run();
        shot(&mut h, &format!("audio-last-good-{theme_name}"));
        h.get_by_role_and_label(
            egui::accesskit::Role::Button,
            "Letzten guten Zustand wiederherstellen",
        )
        .click();
        h.run();
        assert!(commands
            .borrow()
            .iter()
            .any(|c| matches!(c, icu_client::engine::Command::RestoreGoodAudio)));
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Details")
            .click();
        h.run();
        h.get_by_role_and_label(
            egui::accesskit::Role::Button,
            "Für dieses Mikrofon neu lernen",
        )
        .click();
        h.run();
        assert!(commands
            .borrow()
            .iter()
            .any(|c| matches!(c, icu_client::engine::Command::ForgetGoodAudio)));
    }
}

/// The native settings window allows 640 logical pixels, including on HiDPI.
/// Controls must remain inside the content column when labels wrap.
#[test]
fn settings_minimum_width_keeps_controls_reachable() {
    use egui::accesskit::Role;
    let mut failures = Vec::new();
    for theme_name in THEMES {
        for scale in [1.0, 1.5] {
            for page in PAGES {
                let mut s = state();
                s.settings.ui.show_advanced = true;
                let mut view = SettingsView::new(
                    &s,
                    Arc::new(DebugStats::default()),
                    Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
                );
                let mut h = harness_scaled([640.0, 980.0], theme_name, true, scale, move |ui| {
                    view.ui(ui, &s, false);
                });
                h.get_by_role_and_label(Role::Button, page).click();
                h.run();
                for role in [
                    Role::Button,
                    Role::CheckBox,
                    Role::Slider,
                    Role::ComboBox,
                    Role::TextInput,
                ] {
                    for node in h.query_all_by_role(role) {
                        let rect = node.rect();
                        if !(rect.left() >= -0.5
                            && rect.right() <= 618.5
                            && (rect.right() <= 208.5 || rect.left() >= 231.5))
                        {
                            failures.push(format!("{page}/{theme_name}/{scale}: control escapes content: {node:?} {rect:?}"));
                        }
                    }
                }
                if page == "Overlay & Tasten" {
                    let key = h.get_by_label("CTRL+SHIFT+F12").rect();
                    for button in h.query_all_by_role(Role::Button) {
                        if key.intersects(button.rect()) {
                            failures.push(format!(
                                "{page}/{theme_name}/{scale}: key {key:?} obscured by {button:?}"
                            ));
                        }
                    }
                }
                shot(
                    &mut h,
                    &format!("settings-narrow-{page}-{theme_name}-{scale}"),
                );
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn focused_speak_button_holds_and_releases_from_keyboard() {
    use egui::{accesskit::Role, Key};
    let s = four();
    let (cmd, mut rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let mut view = MainView::default();
    let mut h = harness(MAIN, "light", false, move |ui| {
        view.ui(
            ui,
            &s,
            &MainEnv {
                cmd: &cmd,
                video_tier: icu_client::video::VideoTier::T0,
                video: &|_| None,
                self_video: None,
                settings_open: false,
                user_open: None,
                overlay_available: true,
            },
        );
    });
    let drain = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<Command>| {
        let mut events = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            if let Command::Ptt(held) = cmd {
                events.push(held);
            }
        }
        events
    };
    for key in [Key::Space, Key::Enter] {
        if h.query_by_label("Sprechen").is_none() {
            h.get_by_label_contains("Sendemodus wechseln:")
                .click_secondary();
            h.run();
        }
        h.get_by_role_and_label(Role::Button, "Sprechen").focus();
        h.run_steps(2);
        drain(&mut rx);
        h.key_down(key);
        h.run_steps(2);
        assert_eq!(drain(&mut rx), [true]);
        h.key_up(key);
        h.run_steps(2);
        assert_eq!(drain(&mut rx), [false]);
    }
    if h.query_by_label("Sprechen").is_none() {
        h.get_by_label_contains("Sendemodus wechseln:")
            .click_secondary();
        h.run();
    }
    h.get_by_label("Sprechen").focus();
    h.run_steps(2);
    h.key_down(Key::Space);
    h.run_steps(2);
    assert_eq!(drain(&mut rx), [true]);
    h.get_by_role_and_label(Role::CheckBox, "Stumm").focus();
    h.run_steps(2);
    assert_eq!(drain(&mut rx), [false], "moving focus must release PTT");
    h.key_up(Key::Space);
    h.run_steps(2);
    h.get_by_role_and_label(Role::Button, "Sprechen").focus();
    h.run_steps(2);
    h.key_down(Key::Enter);
    h.run_steps(2);
    assert_eq!(drain(&mut rx), [true]);
    h.input_mut().focused = false;
    h.run_steps(2);
    assert_eq!(drain(&mut rx), [false], "window blur must release PTT");
}

#[cfg(unix)]
#[test]
fn passport_enter_on_settings_option_does_not_import_identity() {
    use icu_client::{config::Identity, ui::main_window::EnrollApp};
    use std::sync::Mutex;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let passport = rt.block_on(async {
        let server = common::TestServer::start().await;
        let (identity, _, _dir) = server.enroll("Chris").await;
        identity.to_passport_with_settings(&Settings::default()).0
    });
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(Some(dir.path().to_path_buf()));
    let read_paths = paths.clone();
    let result = Arc::new(Mutex::new(None));
    let result_out = result.clone();
    let mut h = Harness::builder()
        .with_size(egui::vec2(ENROLL[0], ENROLL[1]))
        .build_eframe(move |cc| {
            fonts::install(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx, "light");
            let mut app = EnrollApp::new(paths, None, result_out);
            app.paste(passport);
            app
        });
    h.run_steps(6);
    let option = h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Von dort übernehmen");
    let before = option.accesskit_node().toggled();
    option.focus();
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_ne!(
        h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Von dort übernehmen")
            .accesskit_node()
            .toggled(),
        before
    );
    assert!(
        result.lock().unwrap().is_none(),
        "checkbox activation must not import"
    );
    assert!(Identity::load(&read_paths).unwrap().is_none());
}

#[test]
fn audio_controls_describe_the_effective_processing_mode() {
    use egui::accesskit::Role;
    for theme_name in THEMES {
        let shared = Rc::new(RefCell::new(state()));
        {
            let mut s = shared.borrow_mut();
            s.settings.audio.mode = ModeSetting::Vad;
            s.settings.audio.denoise = false;
            s.settings.audio.vad_auto = false;
            s.settings.ui.show_advanced = true;
        }
        let mut view = SettingsView::new(
            &shared.borrow(),
            Arc::new(DebugStats::default()),
            Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
        );
        let state_in = shared.clone();
        let mut h = harness(SETTINGS, theme_name, true, move |ui| {
            view.ui(ui, &state_in.borrow(), false);
        });
        assert!(h
            .query_by_role_and_label(Role::Slider, "Schwelle")
            .is_none());
        assert!(h
            .get_by_role_and_label(Role::CheckBox, "Schwelle automatisch")
            .accesskit_node()
            .is_disabled());
        h.get_by_label_contains("fester Lautstärkepegel");
        shot(&mut h, &format!("settings-energy-vad-{theme_name}"));
        {
            let mut s = shared.borrow_mut();
            s.settings.audio.denoise = true;
            s.settings.audio.auto_level = true;
            s.settings.audio.gain_db = -12.0;
        }
        h.run();
        assert_eq!(
            h.get_by_role_and_label(Role::Slider, "Verstärkung")
                .accesskit_node()
                .numeric_value(),
            Some(-12.0)
        );
        shot(&mut h, &format!("settings-active-input-trim-{theme_name}"));
    }
}

#[test]
fn settings_navigation_does_not_inherit_another_pages_scroll() {
    use egui::accesskit::Role;
    let mut s = state();
    s.settings.ui.show_advanced = true;
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
    );
    let mut h = harness([640.0, 500.0], "light", true, move |ui| {
        ui.ctx().input_mut(|input| {
            for viewport in input.raw.viewports.values_mut() {
                viewport.monitor_size = Some(egui::vec2(800.0, 550.0));
            }
        });
        view.ui(ui, &s, false);
    });
    let original = h
        .get_by_role_and_label(Role::Slider, "Wiedergabelautstärke")
        .rect()
        .top();
    h.get_by_role_and_label(Role::Slider, "Wiedergabelautstärke")
        .scroll_down();
    h.run();
    let scrolled = h
        .get_by_role_and_label(Role::Slider, "Wiedergabelautstärke")
        .rect()
        .top();
    assert!(
        scrolled < original - 20.0,
        "test must scroll the original page"
    );
    h.get_by_role_and_label(Role::Button, "App").click();
    h.run();
    let first = h.get_by_role_and_label(Role::Label, "App").rect();
    assert!(
        first.top() >= 24.0 && first.bottom() < 90.0,
        "fresh tab must start at its heading: {first:?}"
    );
    h.get_by_role_and_label(Role::Button, "Stimme & Hören")
        .click();
    h.run();
    let restored = h
        .get_by_role_and_label(Role::Slider, "Wiedergabelautstärke")
        .rect()
        .top();
    assert!(
        (restored - scrolled).abs() < 1.0,
        "returning restores that tab's own position"
    );
}

#[test]
fn setup_shows_raw_overload_and_energy_voice_detection() {
    for theme_name in THEMES {
        let mut s = state();
        s.settings.audio.mode = ModeSetting::Vad;
        s.settings.audio.denoise = false;
        s.settings.audio.vad_auto = false;
        s.level.peak_db = -25.0;
        s.level.rms_db = -35.0;
        s.input_clipping = true;
        let mut view = SetupView::new(&s);
        let mut h = harness(SETUP, theme_name, true, move |ui| {
            view.ui(ui, &s);
        });
        h.get_by_label_contains("Eingang übersteuert");
        shot(&mut h, &format!("setup-raw-overload-{theme_name}"));
        h.get_by_role_and_label(egui::accesskit::Role::Button, &next_label())
            .click();
        h.run();
        assert!(h
            .query_by_role_and_label(egui::accesskit::Role::Slider, "Schwelle")
            .is_none());
        shot(&mut h, &format!("setup-energy-vad-{theme_name}"));
    }
}

#[test]
fn overlay_positions_are_named_and_keyboard_accessible() {
    use egui::accesskit::Role;
    let mut s = state();
    s.settings.overlay.enabled = true;
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let cmds_out = cmds.clone();
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-layout"))),
    );
    let mut h = harness(SETTINGS, "dark", true, move |ui| {
        cmds_out.borrow_mut().extend(view.ui(ui, &s, false).cmds);
    });
    h.get_by_role_and_label(Role::Button, "Overlay & Tasten")
        .click();
    h.run();
    for label in [
        "Oben links",
        "Oben rechts",
        "Unten links",
        "Unten rechts",
        "Oben mittig",
        "Unten mittig",
    ] {
        h.get_by_role_and_label(Role::RadioButton, label);
    }
    h.get_by_role_and_label(Role::RadioButton, "Unten links")
        .focus();
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(
        last_update(&cmds.borrow()).overlay.corner,
        icu_client::config::Corner::BottomLeft
    );
    shot(&mut h, "settings-keyboard-overlay-position");
}

#[test]
fn settings_search_reveals_targets_without_changing_values() {
    use egui::accesskit::Role;
    let s = state();
    let commands = Rc::new(RefCell::new(Vec::new()));
    let collected = commands.clone();
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(std::env::temp_dir().join("icu-ui-search"))),
    );
    let mut h = harness(SETTINGS, "dark", true, move |ui| {
        collected.borrow_mut().extend(view.ui(ui, &s, false).cmds);
    });
    for (query, label) in [
        ("andere Apps", "Absenkung"),
        ("echo", "Echo-Unterdrückung"),
        ("Mikrofonkanal", "Mikrofonkanal"),
        ("theme", "Farben"),
        ("PTT", "Push-to-Talk"),
    ] {
        h.get_by_label("Einstellungen suchen").focus();
        h.run();
        h.get_by_label("Einstellungen suchen").type_text(query);
        h.run();
        h.get_by_role_and_label(Role::Button, label).click();
        h.run_steps(12);
        let rect = h.get_by_role_and_label(Role::Label, label).rect();
        assert!(
            rect.top() >= 0.0 && rect.bottom() <= SETTINGS[1] - 50.0,
            "{query}: target is outside the visible page: {rect:?}"
        );
        assert!(
            commands.borrow().is_empty(),
            "search/navigation emitted a command"
        );
        assert_eq!(h.ctx.content_rect().height(), SETTINGS[1]);
        shot(&mut h, &format!("settings-search-{query}"));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_updates_are_dismissible_information_with_manual_checks_only() {
    use icu_client::ui::updates;
    use icu_client::update::{manifest::Release, Action, Snapshot};
    let state = Rc::new(RefCell::new(Snapshot {
        available: Some(Arc::new(Release {
            version: semver::Version::new(99, 0, 0),
            files: vec![],
            envelope: Arc::new(vec![]),
        })),
        ..Default::default()
    }));
    let view = state.clone();
    let mut h = harness([400., 240.], "dark", true, move |ui| {
        let snapshot = view.borrow().clone();
        if matches!(
            updates::notice(ui, &snapshot, None, true, &mut false),
            Some(Action::Dismiss)
        ) {
            view.borrow_mut().dismissed = true;
        }
    });
    assert!(h.query_by_label(updates::LINUX_HINT).is_some());
    assert!(h.query_by_label("Jetzt aktualisieren").is_none());
    assert!(h.query_by_label("Herunterladen und neu starten").is_none());
    h.get_by_label("Später").click();
    h.run();
    assert!(state.borrow().dismissed);
    assert!(h.query_by_label(updates::LINUX_HINT).is_none());
    let checked = Rc::new(std::cell::Cell::new(false));
    let action = checked.clone();
    let mut settings = harness([600., 260.], "light", true, move |ui| {
        if matches!(
            updates::settings(ui, &state.borrow(), true, &mut false),
            Some(Action::Check)
        ) {
            action.set(true);
        }
    });
    assert!(settings.query_by_label(updates::LINUX_HINT).is_some());
    assert!(settings.query_by_label("Jetzt aktualisieren").is_none());
    settings.get_by_label("Nach Updates suchen").click();
    settings.run();
    assert!(checked.get());
}
