//! Builder C owns these main-window surfaces; `winamp/` belongs to Builder A.

pub mod library;
pub mod main_window;
pub mod queue;
pub mod search;
pub mod settings;
pub mod winamp;

pub use main_window::show;
