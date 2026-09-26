//! eframe/egui user interface: main window, settings and enrollment
//! (`Client/IMPLEMENTATION.md` §10).

pub mod connection;
pub mod fonts;
pub mod main_window;
pub mod settings;
pub mod theme;
pub mod widgets;

use std::net::SocketAddr;

use crate::{
    config::{Identity, Paths},
    engine::Engine,
};

pub use main_window::Exit;

/// The push-to-talk binding as a person would say it: `KEY_RIGHTCTRL`
/// becomes `RIGHTCTRL`, and the Portal backend shows its own trigger
/// (`CTRL+SHIFT+F12`) because that is what it actually bound.
pub(crate) fn ptt_key_label(hotkeys: &crate::config::HotkeySettings, backend: &str) -> String {
    let binding = &hotkeys.ptt;
    if backend.contains("portal") {
        if let Some(trigger) = &binding.trigger {
            return trigger.clone();
        }
    }
    if binding.key.is_empty() {
        return "—".to_string();
    }
    binding
        .key
        .strip_prefix("KEY_")
        .or_else(|| binding.key.strip_prefix("BTN_"))
        .unwrap_or(&binding.key)
        .to_string()
}

/// Grows the window `ctx` draws into by `overflow` points so its content
/// fits without a scroll bar, up to 90 % of the monitor. Never shrinks: a
/// window the user resized, or one that grew for a taller page, stays.
pub(crate) fn grow_to_fit(ctx: &egui::Context, overflow: f32) {
    if overflow <= 0.5 {
        return;
    }
    // `content_rect`, not `viewport().inner_rect`: the latter needs the
    // window position, which Wayland never reports, so it stays `None`.
    let inner = ctx.content_rect();
    let max = ctx
        .input(|i| i.viewport().monitor_size)
        .map_or(f32::INFINITY, |m| m.y * 0.9);
    let height = (inner.height() + overflow).min(max).ceil();
    if height > inner.height() + 0.5 {
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            inner.width(),
            height,
        )));
    }
}

/// Runs the main window until the user quits or logs out. Owns the engine.
pub fn run_main_window(engine: Engine, debug_stats: bool, show: bool) -> Result<Exit, String> {
    main_window::run(engine, debug_stats, show)
}

/// Shows the enrollment window; returns the identity once enrolled (and
/// saved), or `None` if the user closed the window.
pub fn run_enroll_window(
    paths: Paths,
    server_addr: Option<SocketAddr>,
) -> Result<Option<Identity>, String> {
    main_window::run_enroll(paths, server_addr)
}
