//! Windows overlay: a layered, click-through, topmost popup that never takes
//! the focus (`WS_EX_LAYERED | TRANSPARENT | TOPMOST | NOACTIVATE |
//! TOOLWINDOW`), filled per frame through `UpdateLayeredWindow` with
//! per-pixel alpha from a top-down 32-bit DIB section the scene draws into
//! directly. Shows over borderless and windowed games; exclusive fullscreen
//! hides it, as any window.
//!
//! Placed in the work area of the primary monitor (taskbar left out), at
//! the configured corner or edge plus offset. The window is sized in
//! physical pixels: tiles, names and offset are scaled by the monitor's
//! DPI (`Scene::scale`), so at 150 % the overlay is as large as elsewhere.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use windows::{
    core::w,
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
            MonitorFromPoint, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
            MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::HiDpi::GetDpiForWindow,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
            RegisterClassExW, SetWindowPos, ShowWindow, HWND_TOPMOST, MSG, PM_REMOVE,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA,
            WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
            WS_EX_TRANSPARENT, WS_POPUP,
        },
    },
};

use super::{dpi_scale, scaled, Scene};
use crate::config::Corner;

/// Runs the overlay until `stop`; reports on `ready` whether it came up.
pub(super) fn run(
    mut scene: Scene,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let hwnd = match unsafe { create_window() } {
        Ok(hwnd) => hwnd,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let mut surface: Option<Dib> = None;
    let mut visible = false;
    let mut raised_at = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let tick_started = std::time::Instant::now();
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe { DispatchMessageW(&msg) };
        }
        // The window sits on the primary monitor; per-monitor DPI aware,
        // Windows reports that monitor's scaling (it may change at any time).
        scene.scale = dpi_scale(unsafe { GetDpiForWindow(hwnd) });
        let update = scene.update();
        if !update.shown {
            if visible {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                visible = false;
            }
        } else {
            let (w, h) = (scene.width, scene.height);
            if surface
                .as_ref()
                .is_none_or(|s| s.width != w || s.height != h)
            {
                surface = None;
                match Dib::new(w, h) {
                    Ok(dib) => surface = Some(dib),
                    Err(e) => tracing::warn!(%e, "overlay bitmap"),
                }
            }
            if let Some(dib) = surface.as_mut() {
                if !scene.draw(dib.pixels()) {
                    std::thread::sleep(scene.interval().saturating_sub(tick_started.elapsed()));
                    continue;
                }
                let at = position(&scene, w, h);
                match dib.present(hwnd, at) {
                    Ok(()) => scene.report_displayed(),
                    Err(e) => tracing::warn!(%e, "overlay update"),
                }
                if !visible {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                    visible = true;
                    tracing::info!(w, h, tiles = scene.tiles.len(), "overlay shown");
                }
                // Another topmost window (a game going borderless) may
                // have come up above; about once a second, go back on top.
                if raised_at.elapsed() >= std::time::Duration::from_secs(1) {
                    raised_at = std::time::Instant::now();
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
            }
        }
        std::thread::sleep(scene.interval().saturating_sub(tick_started.elapsed()));
    }
    drop(surface);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe fn create_window() -> Result<HWND, String> {
    let instance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: w!("icu-overlay"),
        ..Default::default()
    };
    // A second overlay in the same process finds the class registered.
    unsafe { RegisterClassExW(&class) };
    unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            w!("icu-overlay"),
            w!("ICU Overlay"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW: {e}"))
}

/// The work area of the primary monitor.
fn work_area() -> RECT {
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }
}

/// Top left corner of a `w × h` overlay for the configured corner and
/// offset.
fn position(scene: &Scene, w: u32, h: u32) -> POINT {
    place(
        scene.settings.corner,
        scene.settings.offset,
        scene.scale,
        work_area(),
        w,
        h,
    )
}

/// `offset` is in layout pixels, `area`, `w` and `h` in physical ones.
fn place(corner: Corner, offset: [u32; 2], scale: f32, area: RECT, w: u32, h: u32) -> POINT {
    let (ox, oy) = (
        scaled(offset[0], scale) as i32,
        scaled(offset[1], scale) as i32,
    );
    let (w, h) = (w as i32, h as i32);
    let left = area.left + ox;
    let right = area.right - w - ox;
    let top = area.top + oy;
    let bottom = area.bottom - h - oy;
    let center = area.left + (area.right - area.left - w) / 2;
    let (x, y) = match corner {
        Corner::TopLeft => (left, top),
        Corner::TopRight => (right, top),
        Corner::BottomLeft => (left, bottom),
        Corner::BottomRight => (right, bottom),
        Corner::Top => (center, top),
        Corner::Bottom => (center, bottom),
    };
    POINT { x, y }
}

/// A top-down 32-bit DIB section selected into a memory DC.
struct Dib {
    width: u32,
    height: u32,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
}

impl Dib {
    fn new(width: u32, height: u32) -> Result<Dib, String> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative: rows top to bottom, as the scene draws them.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let mut bits = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(b) if !bits.is_null() => b,
                Ok(b) => {
                    let _ = DeleteObject(b.into());
                    let _ = DeleteDC(dc);
                    return Err("CreateDIBSection: no bits".into());
                }
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(format!("CreateDIBSection: {e}"));
                }
            };
            let old = SelectObject(dc, bitmap.into());
            Ok(Dib {
                width,
                height,
                dc,
                bitmap,
                old,
                bits: bits.cast(),
            })
        }
    }

    /// The pixels, premultiplied BGRA, top row first.
    fn pixels(&mut self) -> &mut [u8] {
        let len = self.width as usize * self.height as usize * 4;
        // The DIB section owns `len` bytes for as long as `self` lives.
        unsafe { std::slice::from_raw_parts_mut(self.bits, len) }
    }

    /// Puts the pixels on screen at `at`.
    fn present(&self, hwnd: HWND, at: POINT) -> Result<(), String> {
        let size = SIZE {
            cx: self.width as i32,
            cy: self.height as i32,
        };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::UpdateLayeredWindow(
                hwnd,
                None,
                Some(&at),
                Some(&size),
                Some(self.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
        .map_err(|e| format!("UpdateLayeredWindow: {e}"))
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_and_edges_sit_inside_the_work_area() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let p = |c| place(c, [40, 20], 1.0, area, 166, 100);
        assert_eq!((p(Corner::TopLeft).x, p(Corner::TopLeft).y), (40, 20));
        assert_eq!((p(Corner::TopRight).x, p(Corner::TopRight).y), (1714, 20));
        assert_eq!(
            (p(Corner::BottomRight).x, p(Corner::BottomRight).y),
            (1714, 920)
        );
        assert_eq!(p(Corner::Top).x, 877);
        assert_eq!(p(Corner::Bottom).y, 920);
        // At 150 % the offset from the edge is 60×30 physical pixels.
        let p = place(Corner::BottomRight, [40, 20], 1.5, area, 249, 150);
        assert_eq!((p.x, p.y), (1920 - 249 - 60, 1040 - 150 - 30));
    }
}
