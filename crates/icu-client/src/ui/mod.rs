//! eframe/egui user interface: main window, settings and enrollment
//! (`Client/IMPLEMENTATION.md` §10).

pub mod main_window;
pub mod settings;
pub mod theme;

use std::net::SocketAddr;

use crate::{
    config::{Identity, Paths},
    engine::Engine,
    hotkey::Hotkeys,
};

/// Runs the main window until the user quits. Owns the engine.
pub fn run_main_window(
    engine: Engine,
    hotkeys: Option<Hotkeys>,
    debug_stats: bool,
) -> Result<(), String> {
    main_window::run(engine, hotkeys, debug_stats)
}

/// Shows the enrollment window; returns the identity once enrolled (and
/// saved), or `None` if the user closed the window.
pub fn run_enroll_window(
    paths: Paths,
    server_addr: Option<SocketAddr>,
) -> Result<Option<Identity>, String> {
    main_window::run_enroll(paths, server_addr)
}
