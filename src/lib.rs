//! ytamp — YouTube Music, native and skinned.
//!
//! Architecture and contracts: docs/DESIGN.md.

pub mod app;
pub mod audio;
pub mod eq;
#[cfg(feature = "login-webview")]
pub mod login;
pub mod model;
#[cfg(target_os = "linux")]
pub mod mpris;
pub mod player;
pub mod settings;
pub mod skin;
pub mod ui;
pub mod vis;
pub mod winamp;
pub mod yt;

pub use model::{PlaybackState, PlayerCommand, PlayerEvent, SpectrumFrame, Track};
