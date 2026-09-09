//! Shared model types. **Frozen contract** — see docs/DESIGN.md.
//! Builders: consume these; extend via new variants/fields is allowed, do not
//! rename or remove existing items.

/// A playable YouTube Music track.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// YouTube video id (11 chars).
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: Option<u64>,
    pub thumb_url: Option<String>,
}

impl Track {
    /// "Artist — Title" for displays.
    pub fn display(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.artist, self.title)
        }
    }
}

/// Commands from UI to the player engine.
#[derive(Clone, Debug)]
pub enum PlayerCommand {
    /// Jump to a queue index and play it.
    PlayAt(usize),
    /// Replace the queue; optionally start at an index.
    QueueReplace(Vec<Track>, Option<usize>),
    /// Append tracks to the queue.
    QueueAppend(Vec<Track>),
    Next,
    Prev,
    PlayPause,
    Pause,
    Resume,
    Stop,
    /// Seek to a fraction (0..=1) of the current track.
    SeekRatio(f64),
    /// Master volume 0..=1.
    SetVolume(f32),
    /// Ten-band equalizer settings. Band centers (Hz): 60, 170, 310, 600,
    /// 1000, 3000, 6000, 12000, 14000, 16000 — Winamp's classic curve.
    SetEq {
        enabled: bool,
        gains_db: [f64; 10],
        preamp_db: f64,
    },
}

/// Snapshot of everything a UI needs to draw.
#[derive(Clone, Debug, Default)]
pub struct PlaybackState {
    pub playing: bool,
    pub track: Option<Track>,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub volume: f32,
    pub queue: Vec<Track>,
    pub queue_index: Option<usize>,
}

/// One frame of analyser output: normalized 0..=1 magnitudes, log-spaced.
#[derive(Clone, Debug, Default)]
pub struct SpectrumFrame {
    pub bands: Vec<f32>,
}

/// Events from the player engine to the UI.
#[derive(Clone, Debug)]
pub enum PlayerEvent {
    State(PlaybackState),
    Spectrum(SpectrumFrame),
    /// Human-readable error for a toast/banner; never fatal.
    Error(String),
    Info(String),
}
