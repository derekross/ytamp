//! ytamp — YouTube Music, native and skinned.
//!
//! Architecture and contracts: docs/DESIGN.md.

pub mod audio;
pub mod eq;
pub mod model;
pub mod player;
pub mod skin;
pub mod ui;
pub mod vis;
pub mod winamp;
pub mod yt;

pub use model::{PlayerCommand, PlayerEvent, PlaybackState, SpectrumFrame, Track};
