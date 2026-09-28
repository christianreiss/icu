//! eframe/egui user interface: main window, settings, setup and enrollment
//! (`Client/IMPLEMENTATION.md` §10).

pub mod connection;
pub mod fonts;
pub mod main_window;
pub mod settings;
pub mod setup;
pub mod theme;
pub mod user;
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
            // Left blank in the dialog: no key works, whatever was proposed.
            return if assigned.is_empty() {
                "—".to_string()
            } else {
                assigned
            };
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

/// Whether a file dropped onto a window arrives: winit 0.30 delivers
/// `DroppedFile` on X11 and Windows, not on Wayland — which it uses
/// whenever `WAYLAND_DISPLAY` is set, unless `WINIT_UNIX_BACKEND=x11`
/// forces X11. The first-start window promises drag & drop only where it
/// works.
pub(crate) fn file_drop_works() -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    let forced_x11 = std::env::var("WINIT_UNIX_BACKEND").is_ok_and(|v| v == "x11");
    !wayland || forced_x11
}

/// Whether the windows run as native Wayland clients — winit's own choice:
/// Wayland whenever `WAYLAND_DISPLAY` is set, unless `WINIT_UNIX_BACKEND=x11`
/// forces X11. The clipboard then goes through the compositor, where ICU
/// can neither mark a secret for clipboard managers nor be sure to take it
/// off again (`settings::copy_secret`).
pub(crate) fn native_wayland() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        && !std::env::var("WINIT_UNIX_BACKEND").is_ok_and(|v| v == "x11")
}

/// Work area in egui screen coordinates, including the taskbar exclusion.
/// winit supplies the monitor size but not its usable rectangle.
#[cfg(windows)]
pub(crate) fn work_area(ctx: &egui::Context) -> Option<egui::Rect> {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    };
    // No window position, no window to fit: the headless test harness has
    // none, and would otherwise be held to the desktop it happens to run on.
    let (center, scale) = ctx.input(|i| {
        Some((
            i.viewport().outer_rect?.center(),
            i.viewport().native_pixels_per_point.unwrap_or(1.0),
        ))
    })?;
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

/// ICU's eye for the title bar, taskbar and Alt+Tab (eframe would show its
/// own logo); the child windows inherit it. Rendered from
/// `packaging/files/icu.svg`, like `assets/icu.ico` for `icu.exe`.
pub(crate) fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icu.png")).unwrap_or_default()
}

/// eframe's options for a window of its own: ICU's icon, and on Windows
/// the DX12 shader compiler pinned to FXC (`d3dcompiler_47.dll`, part of
/// Windows). wgpu's default first looks for `dxcompiler.dll`, which no
/// Windows ships — a copy lying next to a downloaded `.icu` file must not
/// be what renders ICU.
pub(crate) fn native_options(viewport: egui::ViewportBuilder) -> eframe::NativeOptions {
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport: viewport.with_icon(app_icon()),
        ..Default::default()
    };
    #[cfg(windows)]
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .shader_compiler = eframe::wgpu::Dx12Compiler::Fxc;
    }
    options
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
mod native_options_tests {
    /// The window gets ICU's icon, not eframe's (a default `IconData` would
    /// mean none); on Windows DX12 compiles with FXC.
    #[test]
    fn windows_carry_the_icu_icon_and_no_dxc() {
        let icon = super::app_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        let options = super::native_options(egui::ViewportBuilder::default());
        assert_eq!(options.viewport.icon.as_deref(), Some(&icon));
        #[cfg(windows)]
        match &options.wgpu_options.wgpu_setup {
            eframe::egui_wgpu::WgpuSetup::CreateNew(setup) => assert!(matches!(
                setup
                    .instance_descriptor
                    .backend_options
                    .dx12
                    .shader_compiler,
                eframe::wgpu::Dx12Compiler::Fxc
            )),
            other => panic!("unexpected wgpu setup: {other:?}"),
        }
    }
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
