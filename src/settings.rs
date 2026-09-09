//! ytamp — user settings, persisted to `~/.config/ytamp/settings.json`.
//!
//! Builder C. Mirrors fastpotify's `settings.rs` pattern: one readable
//! JSON file, `#[serde(default)]` so older files keep loading, atomic
//! writes, and a round-trip test.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Equalizer snapshot the UI hands to the skin engine and the player.
///
/// INTEGRATOR: swap to `crate::eq::EqSettings` (Builder A) once `eq.rs`
/// lands; keep field names `enabled`, `gains_db`, `preamp_db` so the
/// `From` impl below becomes the constructor.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqSettings {
    pub enabled: bool,
    /// Band gains in dB, 60 Hz .. 16 kHz (Winamp's classic curve).
    pub gains_db: [f64; 10],
    /// Pre-amp gain in dB, usually <= 0.
    pub preamp_db: f64,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            gains_db: [0.0; 10],
            preamp_db: 0.0,
        }
    }
}

/// Winamp's ten band centers, in Hz, low to high.
pub const EQ_BAND_CENTERS_HZ: [f64; 10] = [
    60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0,
];

/// A band center as Winamp labels it: `60`, `170`, …, `1k`, `3k`, `16k`.
pub fn eq_band_label(hz: f64) -> String {
    if hz >= 1000.0 && (hz as i64) % 1000 == 0 {
        format!("{}k", (hz / 1000.0) as i64)
    } else {
        format!("{hz:.0}")
    }
}

/// Everything persisted between runs. Stored twice: this JSON file and
/// eframe's own storage (window geometry lives there).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Master volume, 0..=1.
    pub volume: f32,
    /// The ten-band equalizer.
    pub eq: EqSettings,
    /// The worn skin's file name inside the skins directory, or `None` for
    /// the built-in look.
    pub skin: Option<String>,
    /// Mini player integer pixel scale, 1..=4.
    pub skin_scale: u8,
    /// The Winamp mini player is the open window.
    pub winamp_window: bool,
    /// Path to a Netscape-format cookie jar for YouTube Music, if any.
    pub cookie_path: Option<String>,
    /// Recent search terms, newest first.
    pub search_history: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.7,
            eq: EqSettings::default(),
            skin: None,
            skin_scale: 2,
            winamp_window: false,
            cookie_path: None,
            search_history: Vec::new(),
        }
    }
}

impl Settings {
    /// Reads the file, falling back to defaults when it is missing or bad.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                log::warn!("settings at {} are unreadable: {error}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes atomically: a temporary file, then a rename over the target.
    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = match serde_json::to_string_pretty(self) {
            Ok(text) => text,
            Err(error) => {
                log::warn!("unable to encode settings: {error}");
                return;
            }
        };
        let temporary = path.with_extension("json.tmp");
        let written =
            std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, path));
        if let Err(error) = written {
            log::warn!("unable to save settings to {}: {error}", path.display());
        }
    }

    /// Remembers a query, newest first, without duplicates, capped.
    pub fn remember_search(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.search_history.retain(|entry| entry != query);
        self.search_history.insert(0, query.to_string());
        self.search_history.truncate(12);
    }
}

/// The application's config home: `~/.config/ytamp` on Linux.
pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "ytamp")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(fallback_config_dir)
}

/// Last resort when `directories` cannot place us: `$XDG_CONFIG_HOME` or
/// `$HOME/.config`, then `ytamp`.
fn fallback_config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ytamp")
}

/// The user's skin library.
pub fn skins_dir() -> PathBuf {
    config_dir().join("skins")
}

/// Cover-art and misc cache.
pub fn cache_dir() -> PathBuf {
    config_dir().join("cache")
}

/// Where a user-provided Netscape cookie jar lives by convention.
pub fn default_cookie_path() -> PathBuf {
    config_dir().join("cookies.txt")
}

/// Creates the directory skeleton under the config home.
pub fn ensure_config_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(skins_dir())?;
    std::fs::create_dir_all(cache_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_json() {
        let settings = Settings {
            volume: 0.31,
            eq: EqSettings {
                enabled: true,
                gains_db: [-2.0, 0.0, 1.5, 3.0, 4.5, 4.5, 3.0, 0.0, -1.0, -3.5],
                preamp_db: -1.5,
            },
            skin: Some("base-2.91.wsz".into()),
            skin_scale: 3,
            winamp_window: true,
            cookie_path: Some("/tmp/cookies.txt".into()),
            search_history: vec!["lofi beats".into()],
        };
        let json = serde_json::to_string_pretty(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, settings);
    }

    #[test]
    fn older_and_empty_settings_load_with_defaults() {
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Settings::default());
        assert!(!empty.winamp_window);
        assert_eq!(empty.skin_scale, 2);
        assert!(!empty.eq.enabled);

        let partial: Settings = serde_json::from_str(r#"{"volume": 0.5}"#).unwrap();
        assert_eq!(partial.volume, 0.5);
        assert_eq!(partial.eq, EqSettings::default());
    }

    #[test]
    fn load_and_save_survive_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(Settings::load(&path), Settings::default());

        let mut settings = Settings::default();
        settings.volume = 0.9;
        settings.skin = Some("Zaxon.wsz".into());
        settings.save(&path);

        assert_eq!(Settings::load(&path), settings);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn unreadable_settings_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn search_history_is_deduplicated_newest_first_and_capped() {
        let mut settings = Settings::default();
        for i in 0..15 {
            settings.remember_search(&format!("query {i}"));
        }
        settings.remember_search("query 12");

        assert_eq!(settings.search_history.len(), 12);
        assert_eq!(settings.search_history[0], "query 12");
        assert_eq!(
            settings
                .search_history
                .iter()
                .filter(|q| *q == "query 12")
                .count(),
            1
        );
        settings.remember_search("   ");
        assert_eq!(settings.search_history.len(), 12);
    }

    #[test]
    fn eq_band_labels_match_winamp() {
        let labels: Vec<String> = EQ_BAND_CENTERS_HZ
            .iter()
            .map(|hz| eq_band_label(*hz))
            .collect();
        assert_eq!(
            labels,
            [
                "60", "170", "310", "600", "1k", "3k", "6k", "12k", "14k", "16k"
            ]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
        );
    }
}
