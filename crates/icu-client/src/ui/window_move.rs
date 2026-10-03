//! Smooth window dragging on Windows.
//!
//! Dragging a window by its title bar runs a modal loop inside that
//! window's own message handling: the window only moves while the UI
//! thread is free. winit reports every step as `Moved`, egui repaints for
//! each one, and ICU's periodic repaints render inside that loop as well,
//! so the window could only move between two frames. On a Windows 11 VM
//! the main window followed the pointer with 12–16 position updates a second
//! against the pointer's 125; Notepad kept up at 122
//! (`experiments/spike-window-drag`).
//!
//! While one of ICU's windows is dragged, its moves bypass winit — egui
//! reads the position from the window each frame anyway — and the periodic
//! repaints pause ([`super::repaint_after`]); one repaint catches up once
//! the drag ends. Resizing still repaints on every step, as eframe wants.

use std::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            DefWindowProcW, EnumThreadWindows, GetWindowLongW, GWL_STYLE, SWP_NOSIZE, WINDOWPOS,
            WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_NCDESTROY, WM_WINDOWPOSCHANGED, WS_CAPTION,
        },
    },
};

/// Any of ICU's windows is being dragged or resized right now.
static ACTIVE: AtomicBool = AtomicBool::new(false);
thread_local! {
    /// Woken once a drag ends, to draw what it held back. The window that
    /// is up now: enrollment and main window are separate eframe runs, each
    /// with its own context, on the same thread.
    static CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}
const SUBCLASS_ID: usize = 0x1c0_d4a6;

/// Whether one of ICU's windows is in its move or size loop.
pub(super) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Watches every top-level window with a title bar on the calling (UI)
/// thread. Idempotent and cheap, so it runs every frame and picks up
/// viewports opened since.
pub(super) fn watch(ctx: &egui::Context) {
    CTX.with_borrow_mut(|current| {
        if current.as_ref() != Some(ctx) {
            *current = Some(ctx.clone());
        }
    });
    unsafe {
        let _ = EnumThreadWindows(GetCurrentThreadId(), Some(each), LPARAM(0));
    }
}

unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> windows::core::BOOL {
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    if style & WS_CAPTION.0 == WS_CAPTION.0 {
        // Installing again only replaces the (unused) reference data.
        let _ = unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, 0) };
    }
    true.into()
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    match msg {
        WM_ENTERSIZEMOVE => ACTIVE.store(true, Ordering::Relaxed),
        WM_EXITSIZEMOVE => {
            ACTIVE.store(false, Ordering::Relaxed);
            CTX.with_borrow(|ctx| {
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            });
        }
        // A plain move during the loop: Windows' own handling, without
        // winit's `Moved` and the repaint it brings.
        WM_WINDOWPOSCHANGED if active() => {
            // SAFETY: WM_WINDOWPOSCHANGED carries a WINDOWPOS in `lparam`.
            let pos = unsafe { &*(lparam.0 as *const WINDOWPOS) };
            if pos.flags.contains(SWP_NOSIZE) {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
        }
        WM_NCDESTROY => {
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID) };
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
