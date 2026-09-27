//! eframe/egui user interface: main window, settings, setup and enrollment
//! (`Client/IMPLEMENTATION.md` §10).

pub mod connection;
pub mod fonts;
pub mod main_window;
pub mod settings;
pub mod setup;
pub mod theme;
pub mod widgets;

use std::net::SocketAddr;

use crate::{
    config::{Identity, Paths},
    engine::Engine,
};

pub use main_window::Exit;

/// The push-to-talk binding as a person would say it: `KEY_RIGHTCTRL`
/// becomes `RIGHTCTRL`. Through the Portal it is the key the desktop
/// assigned (its dialog may take another than the one proposed), else the
/// proposed trigger (`CTRL+SHIFT+F12`).
pub(crate) fn ptt_key_label(hotkeys: &crate::config::HotkeySettings, backend: &str) -> String {
    let binding = &hotkeys.ptt;
    if backend == "portal" {
        if let Some(assigned) = crate::hotkey::portal_trigger(crate::hotkey::Action::Ptt) {
            return assigned;
        }
    }
    if backend == "portal"
        || (backend == crate::hotkey::BACKEND_RAWINPUT && binding.key.trim().is_empty())
    {
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

/// Work area in egui screen coordinates, including the taskbar exclusion.
/// winit supplies the monitor size but not its usable rectangle.
#[cfg(windows)]
pub(crate) fn work_area(ctx: &egui::Context) -> Option<egui::Rect> {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    };
    let (center, scale) = ctx.input(|i| {
        (
            i.viewport()
                .outer_rect
                .map(|r| r.center())
                .unwrap_or_default(),
            i.viewport().native_pixels_per_point.unwrap_or(1.0),
        )
    });
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: (center.x * scale) as i32,
                y: (center.y * scale) as i32,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let r = info.rcWork;
    Some(egui::Rect::from_min_max(
        egui::pos2(r.left as f32 / scale, r.top as f32 / scale),
        egui::pos2(r.right as f32 / scale, r.bottom as f32 / scale),
    ))
}

/// Reserve room for native borders and the title bar; keep the action row
/// reachable on small or scaled desktops. Content can scroll inside.
#[cfg(any(windows, test))]
pub(crate) fn fit_window(size: [f32; 2], area: egui::Rect) -> (egui::Vec2, egui::Pos2) {
    let available = (area.size() - egui::vec2(32.0, 72.0)).max(egui::vec2(1.0, 1.0));
    let size = egui::vec2(size[0], size[1]).min(available);
    let pos = area.min + (area.size() - size) * 0.5 - egui::vec2(0.0, 12.0);
    (size, pos)
}

#[cfg(windows)]
pub(crate) fn position_window(ctx: &egui::Context, size: [f32; 2], min: [f32; 2]) -> bool {
    let Some(area) = work_area(ctx) else {
        return false;
    };
    let (size, pos) = fit_window(size, area);
    ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(
        egui::vec2(min[0], min[1]).min(size),
    ));
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
    true
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
    #[cfg(windows)]
    let max = work_area(ctx).map_or(max, |area| {
        let top = ctx
            .input(|i| i.viewport().outer_rect.map(|r| r.top()))
            .unwrap_or(area.top());
        max.min((area.bottom() - top - 48.0).max(inner.height()))
    });
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
    main_window::run_enroll(paths, server_addr, None)
}

/// The enrollment window with an invite or passport already filled in —
/// `icu <file>` from a file manager or a double-click on an `.icu` file:
/// the window shows where it leads and joins on one click.
pub fn run_enroll_window_with(
    paths: Paths,
    server_addr: Option<SocketAddr>,
    text: String,
) -> Result<Option<Identity>, String> {
    main_window::run_enroll(paths, server_addr, Some(text))
}

#[cfg(test)]
mod window_fit_tests {
    #[test]
    fn dialog_actions_fit_a_small_or_scaled_work_area() {
        for (width, height) in [(1024.0, 728.0), (682.0, 485.0)] {
            let area =
                egui::Rect::from_min_size(egui::pos2(-width, 0.0), egui::vec2(width, height));
            let (size, pos) = super::fit_window([600.0, 720.0], area);
            assert!(pos.x >= area.left() && pos.y >= area.top());
            assert!(pos.x + size.x + 8.0 <= area.right());
            assert!(pos.y + size.y + 40.0 <= area.bottom());
        }
    }
}
