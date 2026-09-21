//! ICU voice client library: everything but the argument parsing lives here
//! so the integration tests can drive a client in-process.

pub mod audio;
#[cfg(feature = "camera")]
pub mod camera;
pub mod config;
pub mod cues;
pub mod dsp;
pub mod engine;
pub mod hotkey;
pub mod net;
pub mod netcond;
#[cfg(feature = "overlay")]
pub mod overlay;
pub mod quality;
pub mod stats;
#[cfg(feature = "tray")]
pub mod tray;
#[cfg(feature = "gui")]
pub mod ui;
pub mod uri;
pub mod video;

pub use engine::{AppState, Command, ConnStatus, Engine, EngineConfig, Peer};
