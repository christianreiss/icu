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

use egui_kittest::{kittest::Queryable, Harness};
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
    let mut harness = Harness::builder()
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
    }
}

fn peer(user_id: u32, name: &str, flags: u8, rtt: u16) -> Peer {
    let mut p = Peer::new(
        UserInfo {
            user_id,
            session: user_id as u16 + 10,
            name: name.into(),
            flags: UserFlags(flags),
        },
        100.0,
    );
    p.rtt_ms = Some(rtt);
    p.recv_loss_permille = Some(3);
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
        conn: ConnStatus::Connected,
        server: "icu.example.org:4711".into(),
        server_name: "icu.example.org".into(),
        me: Some(UserInfo {
            user_id: 1,
            session: 11,
            name: "Chris".into(),
            flags: UserFlags(UserFlags::ADMIN | UserFlags::VIEWING | UserFlags::PTT),
        }),
        peers,
        muted: false,
        deafened: false,
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
const PAGES: [&str; 7] = [
    "Audio",
    "Klang",
    "Tastenkürzel",
    "Kamera",
    "Allgemein",
    "Nutzer",
    "Über ICU",
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
                let may_grow = advanced || page == "Nutzer";
                overflows.check_window(&what, &h, SETTINGS, may_grow, *overflow.borrow());
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
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Automatisch")
        .click();
    h.run();
    let sent = last_update(&cmds.borrow());
    assert_eq!(sent.audio.mode, ModeSetting::Vad);
    shared.borrow_mut().settings = sent;
    h.run();

    // A hotkey flips back to push-to-talk behind the window's back.
    shared.borrow_mut().settings.audio.mode = ModeSetting::Ptt;
    h.run();

    // The next edit in the window keeps push-to-talk.
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Senden beginnt oder endet")
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
        h.get_by_role_and_label(
            egui::accesskit::Role::Button,
            &format!("{}  Übernehmen", egui_phosphor::regular::CHECK),
        )
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
        let server = common::TestServer::start().await;
        server.invite_bundle("").await
    });
    let mut forged = icu_proto::passport::Passport::decode(&bundle).unwrap();
    let mut hot = Settings::default();
    hot.audio.vad_auto = false;
    hot.audio.vad_threshold = 0.0;
    forged.settings = toml::to_string(&hot).unwrap();
    let s = state();
    let dir = tempfile::tempdir().unwrap();
    let mut view = SettingsView::new(
        &s,
        Arc::new(DebugStats::default()),
        Paths::new(Some(dir.path().to_path_buf())),
    );
    view.import_text(forged.encode());
    let cmds = Rc::new(RefCell::new(Vec::<Command>::new()));
    let cmds_out = cmds.clone();
    let mut h = harness(SETTINGS, "light", true, move |ui| {
        cmds_out.borrow_mut().extend(view.ui(ui, &s, false).cmds);
    });
    assert!(h.query_by_label("Einstellungen übernehmen").is_none());
    h.get_by_role_and_label(
        egui::accesskit::Role::Button,
        &format!("{}  Übernehmen", egui_phosphor::regular::CHECK),
    )
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
    h.get_by_role_and_label(
        egui::accesskit::Role::Button,
        &format!("{}  Übernehmen", egui_phosphor::regular::CHECK),
    );
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
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Klang")
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
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Audio")
        .click();
    h.run();
    let before = s.settings.audio.output_volume;
    let volume = h.get_by_role_and_label(egui::accesskit::Role::Slider, "Lautstärke");
    volume.focus();
    h.run();
    h.key_press(egui::Key::ArrowRight);
    h.key_press(egui::Key::ArrowRight);
    h.run();
    let after = last_update(&cmds.borrow()).audio.output_volume;
    assert!(after > before, "volume {before} → {after}");
    h.key_press(egui::Key::ArrowLeft);
    h.run();
    let back = last_update(&cmds.borrow()).audio.output_volume;
    assert!(back < after && back > before, "one step back: {back}");
}

/// No way to read keys: the banner says what to do and switches to voice
/// activation with one click, on the Tastenkürzel page and in the setup.
#[test]
fn no_hotkey_backend_offers_voice_activation() {
    let switch = format!(
        "{}  Stattdessen automatisch senden",
        egui_phosphor::regular::WAVEFORM
    );
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
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Tastenkürzel")
        .click();
    h.run();
    if cfg!(target_os = "linux") {
        h.get_by_label_contains("sudo usermod -aG input $USER");
    }
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
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Allgemein")
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

/// The setup mutes on its first frame; „Fertig“ at the end marks it done
/// and unmutes, the way the main window closes it (`App::setup_window`).
#[test]
fn setup_finish_marks_done_and_unmutes() {
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
        matches!(cmds.borrow().first(), Some(Command::SetMuted(true))),
        "muted while the setup is open"
    );
    // The engine applies it.
    shared.borrow_mut().muted = true;
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
            .any(|c| matches!(c, Command::SetMuted(false))),
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
        matches!(cmds.borrow().last(), Some(Command::SetMuted(false))),
        "unmuted by „Fertig“"
    );
}

/// Quitting ICU with the setup open lifts the setup's own mute
/// (`App::on_exit`): the engine keeps a mute for the next start, and that
/// must be the user's, not the setup's.
#[test]
fn quitting_with_the_setup_open_lifts_its_mute() {
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
        Some(Command::SetMuted(true))
    ));
    shared.borrow_mut().muted = true;
    h.run();
    drop(h);
    let state = shared.borrow();
    assert!(matches!(
        view.borrow_mut().release_mute(&state),
        Some(Command::SetMuted(false))
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
