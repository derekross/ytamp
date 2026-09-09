//! ytamp — the application core: `YtampApp` owns settings, the player
//! engine handles, the search pipeline, the skin state, and the banners.
//! It implements [`WinampHost`], the frozen trait the skin engine draws
//! against (docs/DESIGN.md).
//!
//! Builder C. The `standins` module holds minimal local types for Builder
//! A (skin engine) and Builder B (YT + audio) so this shell compiles and
//! runs standalone; each carries an `INTEGRATOR:` swap note.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

use crate::model::{PlaybackState, PlayerCommand, PlayerEvent, SpectrumFrame, Track};
use crate::settings::{EqSettings, Settings};

// INTEGRATOR: after Builder A's branch lands, replace these re-exports with
// the real types (keeping the same names everywhere else in this shell):
//   Skin, SkinError          <- crate::skin
//   SkinTextures, WinampState, winamp_ui, WinampHost <- crate::ui::winamp
// INTEGRATOR: after Builder B's branch lands:
//   PlayerEngine             <- crate::player
//   YtClient                 <- crate::yt
pub use standins::{PlayerEngine, Skin, SkinError, WinampHost, WinampState, YtClient, winamp_ui};

/// How long a toast/banner stays on screen.
pub const TOAST_LIFETIME: Duration = Duration::from_secs(4);

/// Local stand-ins for Builder A and Builder B types, per docs/DESIGN.md
/// contracts. Each is the minimum the app shell needs; swap on integration.
pub mod standins {
    use std::path::Path;

    use super::*;

    // ------------------------------------------------------------------
    // Builder A: skin engine (src/skin/*, src/ui/winamp/*)
    // ------------------------------------------------------------------

    /// The host interface the Winamp UI draws against — exactly the frozen
    /// trait from docs/DESIGN.md.
    ///
    /// INTEGRATOR: delete this stand-in and import the trait from
    /// `crate::ui::winamp` (Builder A); the `impl WinampHost for YtampApp`
    /// below keeps working unchanged.
    pub trait WinampHost {
        fn cmd(&mut self, c: PlayerCommand);
        fn state(&self) -> &PlaybackState;
        fn spectrum(&self) -> SpectrumFrame;
        fn eq(&self) -> EqSettings;
        fn load_skin_file(&mut self, path: &Path);
    }

    /// INTEGRATOR: swap to `crate::skin::Skin` (Builder A).
    #[derive(Clone, Debug)]
    pub struct Skin {
        pub name: String,
    }

    /// INTEGRATOR: swap to `crate::skin::SkinError` (Builder A).
    #[derive(Clone, Debug)]
    pub struct SkinError(pub String);

    impl std::fmt::Display for SkinError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for SkinError {}

    impl Skin {
        /// The built-in look, worn when no archive is loaded.
        pub fn builtin() -> Skin {
            Skin {
                name: "built-in".into(),
            }
        }

        /// INTEGRATOR: swap to the real parser (zip + BMP sprite sheets).
        pub fn load(path: &Path) -> Result<Skin, SkinError> {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "skin".into());
            let bytes =
                std::fs::read(path).map_err(|e| SkinError(format!("{}: {e}", path.display())))?;
            Self::from_archive(&name, &bytes)
        }

        /// INTEGRATOR: swap to the real parser.
        pub fn from_archive(name: &str, bytes: &[u8]) -> Result<Skin, SkinError> {
            if bytes.len() < 4 || &bytes[..2] != b"PK" {
                return Err(SkinError("not a zip archive (.wsz)".into()));
            }
            Ok(Skin {
                name: super::skin_display_label(name).to_string(),
            })
        }
    }

    /// INTEGRATOR: swap to Builder A's texture cache (sprite sheets turned
    /// into egui handles).
    #[derive(Default)]
    pub struct SkinTextures;

    /// Mini player state: scale, toggles, remembered window position.
    ///
    /// INTEGRATOR: swap to Builder A's Winamp state type
    /// (`crate::winamp::WinampState` or `ui::winamp`'s). `wants_exit` is
    /// stand-in only (their eject-button path replaces it).
    #[derive(Clone, Debug)]
    pub struct WinampState {
        pub scale: u8,
        pub time_remaining: bool,
        pub restore_pos: Option<[f32; 2]>,
        pub last_pos: Option<[f32; 2]>,
        /// Stand-in only: the ✕ button asked to leave mini mode.
        pub wants_exit: bool,
        /// Stand-in only: the EQ button asked to flip the EQ panel.
        pub wants_eq: bool,
        /// Stand-in only: whether the EQ panel is open (mirrors the app's
        /// `eq_open`, decided here so the shell stays out of the draw).
        pub eq_open: bool,
        /// Stand-in only: the EQ sliders' working copy; each twist goes to
        /// the engine through `SetEq` and comes back via the echo channel.
        pub eq_scratch: EqSettings,
    }

    impl Default for WinampState {
        fn default() -> Self {
            Self {
                scale: 2,
                time_remaining: false,
                restore_pos: None,
                last_pos: None,
                wants_exit: false,
                wants_eq: false,
                eq_open: false,
                eq_scratch: EqSettings::default(),
            }
        }
    }

    impl WinampState {
        /// Where the window last was, if it ever told us.
        pub fn remember_position(&mut self) {
            if let Some(pos) = self.last_pos {
                self.restore_pos = Some(pos);
            }
        }

        /// The main window is classically 275x116 skin pixels.
        pub fn window_size(&self) -> egui::Vec2 {
            egui::vec2(275.0, 116.0) * self.scale as f32
        }
    }

    /// Renders the whole mini-player window.
    ///
    /// INTEGRATOR: swap to `crate::ui::winamp::winamp_ui` (Builder A) —
    /// same signature as docs/DESIGN.md.
    pub fn winamp_ui(
        ui: &mut egui::Ui,
        state: &mut WinampState,
        skin: &Skin,
        host: &mut dyn WinampHost,
        tex: &mut SkinTextures,
    ) {
        let _ = tex;
        mini_player::draw(ui, state, skin, host);
    }

    pub mod mini_player;

    // ------------------------------------------------------------------
    // Builder B: player engine (src/player.rs, src/audio/*)
    // ------------------------------------------------------------------

    /// Offline player engine: drains commands, keeps a local
    /// [`PlaybackState`], ticks position while "playing", and broadcasts
    /// state + an animated fake spectrum so the UI is demoable standalone.
    ///
    /// INTEGRATOR: swap to `crate::player::PlayerEngine::spawn` (Builder B).
    /// DESIGN.md freezes the two-argument shape; if their spawn grows an
    /// audio-output argument, adjust only the call site in `YtampApp::new`.
    pub struct PlayerEngine;

    impl PlayerEngine {
        pub fn spawn(
            commands: mpsc::Receiver<PlayerCommand>,
            events: broadcast::Sender<PlayerEvent>,
        ) -> anyhow::Result<Self> {
            std::thread::Builder::new()
                .name("player-engine-standin".into())
                .spawn(move || engine_loop(commands, events))
                .map_err(|e| anyhow::anyhow!("engine thread: {e}"))?;
            Ok(Self)
        }
    }

    fn engine_loop(
        mut commands: mpsc::Receiver<PlayerCommand>,
        events: broadcast::Sender<PlayerEvent>,
    ) {
        let mut state = PlaybackState::default();
        let info = PlayerEvent::Info(
            "Audio engine offline (stand-in): playback is simulated until the real engine lands."
                .into(),
        );
        let _ = events.send(info);
        let _ = events.send(PlayerEvent::State(state.clone()));

        let mut last_tick = Instant::now();
        let mut last_state: Option<Instant> = None;
        let mut last_spectrum: Option<Instant> = None;
        loop {
            // A batch of commands, then the clock.
            let mut commands_seen = false;
            while let Ok(command) = commands.try_recv() {
                commands_seen = true;
                apply_command(&mut state, &command);
            }
            if commands.is_closed() {
                break; // app went away
            }
            let elapsed = last_tick.elapsed().as_secs_f64();
            last_tick = Instant::now();
            if state.playing {
                state.position_secs += elapsed;
                if let Some(duration) = state.duration_secs
                    && state.position_secs >= duration
                {
                    state.position_secs = duration;
                    state.playing = false;
                }
            }

            let now = Instant::now();
            let playing = state.playing;
            if commands_seen
                || (playing && last_state.is_none_or(|t| now - t >= Duration::from_millis(250)))
            {
                last_state = Some(now);
                let _ = events.send(PlayerEvent::State(state.clone()));
            }
            if playing && last_spectrum.is_none_or(|t| now - t >= Duration::from_millis(66)) {
                last_spectrum = Some(now);
                let t = now.elapsed().as_secs_f64();
                let _ = events.send(PlayerEvent::Spectrum(fake_spectrum(t)));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn apply_command(state: &mut PlaybackState, command: &PlayerCommand) {
        let play_index = |state: &mut PlaybackState, index: usize| {
            if let Some(track) = state.queue.get(index).cloned() {
                state.queue_index = Some(index);
                state.track = Some(track.clone());
                state.position_secs = 0.0;
                state.duration_secs = track.duration_secs.map(|duration| duration as f64);
                state.playing = true;
            }
        };
        match command {
            PlayerCommand::PlayAt(i) => play_index(state, *i),
            PlayerCommand::QueueReplace(tracks, start) => {
                state.queue = tracks.clone();
                match start {
                    Some(i) => play_index(state, *i),
                    None => {
                        state.queue_index = None;
                        state.track = None;
                        state.playing = false;
                        state.position_secs = 0.0;
                        state.duration_secs = None;
                    }
                }
            }
            PlayerCommand::QueueAppend(tracks) => state.queue.extend(tracks.iter().cloned()),
            PlayerCommand::Next => {
                if let Some(i) = state.queue_index
                    && i + 1 < state.queue.len()
                {
                    play_index(state, i + 1);
                } else {
                    state.playing = false;
                }
            }
            PlayerCommand::Prev => {
                if state.position_secs > 3.0 {
                    state.position_secs = 0.0;
                } else if let Some(i) = state.queue_index.and_then(|i| i.checked_sub(1)) {
                    play_index(state, i);
                } else {
                    state.position_secs = 0.0;
                }
            }
            PlayerCommand::PlayPause => {
                if state.playing {
                    state.playing = false;
                } else if state.queue_index.is_some() {
                    state.playing = true;
                }
            }
            PlayerCommand::Pause => state.playing = false,
            PlayerCommand::Resume => {
                if state.queue_index.is_some() {
                    state.playing = true;
                }
            }
            PlayerCommand::Stop => {
                state.playing = false;
                state.position_secs = 0.0;
            }
            PlayerCommand::SeekRatio(ratio) => {
                if let Some(duration) = state.duration_secs {
                    state.position_secs = (ratio.clamp(0.0, 1.0) * duration).min(duration);
                }
            }
            PlayerCommand::SetVolume(volume) => state.volume = volume.clamp(0.0, 1.0),
            PlayerCommand::SetEq { .. } => {} // the UI owns the echo of this
        }
    }

    /// A gentle animated stand-in for the analyser tap.
    fn fake_spectrum(t: f64) -> SpectrumFrame {
        let bands = (0..20)
            .map(|i| {
                let f = i as f64;
                let pulse = 0.55 + 0.45 * (t * 1.7).sin();
                let wave = 0.5 + 0.5 * (t * (2.0 + f * 0.31) + f * 1.3).sin();
                let tilt = 1.0 - (f / 22.0) * 0.55;
                (pulse * wave * tilt).clamp(0.0, 1.0) as f32
            })
            .collect();
        SpectrumFrame { bands }
    }

    // ------------------------------------------------------------------
    // Builder B: YouTube Music client (src/yt/*)
    // ------------------------------------------------------------------

    /// Canned-results client so the search UI is demoable offline.
    ///
    /// INTEGRATOR: swap to `crate::yt::YtClient` (Builder B). The app keeps
    /// it behind an `Arc`, so it does not need to be `Clone`.
    #[derive(Clone, Default)]
    pub struct YtClient {
        #[allow(dead_code)] // real client: reqwest + cookies
        cookies: Option<String>,
    }

    impl YtClient {
        pub fn new(cookies: Option<String>) -> Self {
            Self { cookies }
        }

        pub async fn search_tracks(&self, q: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
            Ok((0..limit).map(|i| canned_track(q, i)).collect())
        }

        pub async fn radio_for(&self, video_id: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
            Ok((0..limit)
                .map(|i| canned_track(&format!("radio:{video_id}"), i + 100))
                .collect())
        }
    }

    const CANNED_ARTISTS: [&str; 8] = [
        "Autechre",
        "Biosphere",
        "Global Communication",
        "The Orb",
        "Boards of Canada",
        "Aphex Twin",
        "Burial",
        "Four Tet",
    ];

    /// Deterministic stand-in track from a seed. Public for tests.
    pub fn canned_track(seed: &str, i: usize) -> Track {
        Track {
            video_id: canned_video_id(seed, i),
            title: if i == 0 {
                format!("{seed} (stand-in)")
            } else {
                format!("{seed} — take {}", i + 1)
            },
            artist: CANNED_ARTISTS[i % CANNED_ARTISTS.len()].into(),
            album: Some("Canned Results".into()),
            duration_secs: Some((150 + (i * 37) % 240) as u64),
            thumb_url: None,
        }
    }

    /// An 11-character deterministic id, YouTube-shaped.
    fn canned_video_id(seed: &str, i: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in seed.bytes().chain((i as u64).to_le_bytes()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        (0..11)
            .map(|_| {
                h ^= h << 13;
                h ^= h >> 7;
                h ^= h << 17;
                ALPHABET[(h % 64) as usize] as char
            })
            .collect()
    }
}

/// A skin file name without its archive extension, for showing.
pub fn skin_display_label(name: &str) -> &str {
    name.rsplit_once('.')
        .filter(|(_, ext)| matches!(ext.to_ascii_lowercase().as_str(), "wsz" | "wal" | "zip"))
        .map_or(name, |(stem, _)| stem)
}

/// True for `.wsz` / `.wal` / `.zip` skin archives.
pub fn is_skin_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        matches!(
            ext.to_string_lossy().to_ascii_lowercase().as_str(),
            "wsz" | "wal" | "zip"
        )
    })
}

/// Fits the fixed-size mini window to its wanted size. Compositors refuse
/// chatty resize requests, so a rejected ask retries at most once a
/// second (fastpotify's pattern).
fn fit_mini_window(ctx: &egui::Context, wanted: egui::Vec2) {
    let current = ctx.input(|input| {
        input
            .viewport()
            .inner_rect
            .map(|rect| rect.size())
            .unwrap_or(wanted)
    });
    if (current - wanted).abs().max_elem() < 1.0 {
        return;
    }
    let asked = egui::Id::new("ytamp-mini-fit");
    let now = ctx.input(|input| input.time);
    let last: Option<f64> = ctx.data(|data| data.get_temp(asked));
    if last.is_some_and(|last| now - last < 1.0) {
        return;
    }
    ctx.data_mut(|data| data.insert_temp(asked, now));
    ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(wanted));
    ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(wanted));
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(wanted));
}

/// Installs Inter as the proportional font when the system has it; egui's
/// defaults otherwise (docs/DESIGN.md: Inter is the shell's face).
pub fn install_fonts(ctx: &egui::Context) {
    let mut candidates: Vec<PathBuf> = [
        "/usr/share/fonts/inter/Inter-Regular.ttf",
        "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
        "/usr/share/fonts/inter/InterVariable.ttf",
        "/Library/Fonts/Inter-Regular.otf",
        "/Library/Fonts/InterVariable.ttf",
        "C:\\Windows\\Fonts\\Inter-Regular.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".local/share/fonts/Inter-Regular.ttf"));
        candidates.push(home.join(".local/share/fonts/InterVariable.ttf"));
        candidates.push(home.join(".fonts/Inter-Regular.ttf"));
    }
    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("Inter".into(), egui::FontData::from_owned(bytes).into());
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "Inter".into());
        ctx.set_fonts(fonts);
        log::info!("typography: Inter from {}", path.display());
        return;
    }
    log::debug!("typography: Inter not found on this system; egui defaults");
}

/// What landed from a spawned search or radio fetch.
#[derive(Debug)]
pub enum SearchOutcome {
    Results {
        query: String,
        tracks: Vec<Track>,
    },
    Radio {
        video_id: String,
        tracks: Vec<Track>,
    },
    Failed(String),
}

/// Search box state + the results the central panel lists.
#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub committed: String,
    pub searching: bool,
    pub results: Vec<Track>,
    pub selected: Option<usize>,
    /// Where the keyboard navigator last landed, for scroll-into-view.
    pub scroll_to: Option<usize>,
    /// The search TextEdit's widget id, for Ctrl+F.
    pub id: Option<egui::Id>,
}

/// Skin + mini player state behind the Winamp window.
pub struct MiniState {
    pub skin: Option<Skin>,
    pub winamp: WinampState,
    pub textures: standins::SkinTextures,
}

impl Default for MiniState {
    fn default() -> Self {
        Self {
            skin: None,
            winamp: WinampState::default(),
            textures: standins::SkinTextures,
        }
    }
}

/// Which page the main window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Search,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

/// An auto-expiring banner.
#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub until: Instant,
}

/// The application: everything any window of ytamp draws from, surviving
/// window switches (main <-> mini) inside the eframe shell in `main.rs`.
pub struct YtampApp {
    pub settings: Settings,
    settings_path: PathBuf,
    settings_dirty: bool,
    last_settings_save: Option<Instant>,

    /// Commands to the player engine; `None` when the engine is gone.
    pub cmd_tx: Option<mpsc::Sender<PlayerCommand>>,
    events: Option<broadcast::Receiver<PlayerEvent>>,
    /// Latest engine snapshots.
    pub state: PlaybackState,
    pub spectrum: SpectrumFrame,
    /// Shown as the "engine offline" chip when set.
    pub engine_note: Option<String>,

    /// Runtime for spawned searches (the engine owns its own).
    rt: tokio::runtime::Runtime,
    /// The YT client behind an `Arc` so spawned tasks can take a copy.
    pub yt: Arc<YtClient>,
    pub search: SearchState,
    search_outcome_tx: std::sync::mpsc::Sender<SearchOutcome>,
    search_rx: std::sync::mpsc::Receiver<SearchOutcome>,

    /// Skin loads requested from inside the Winamp UI (trait-safe deferral).
    skin_request_tx: std::sync::mpsc::Sender<PathBuf>,
    skin_request_rx: std::sync::mpsc::Receiver<PathBuf>,

    pub mini: MiniState,
    pub toasts: Vec<Toast>,

    /// The outer loop in `main` should reopen as the other window kind.
    pub switch_intent: bool,
    pub view: View,
    pub eq_open: bool,
    /// Position slider scrub while dragged, in seconds.
    pub scrub: Option<f64>,
    /// The skins library directory (injected for tests).
    pub skins_dir: PathBuf,
    /// Whether the queue side panel is open in the main window.
    pub show_queue: bool,
    /// The last window title sent to the viewport.
    window_title: String,
    /// Echoes from the mini player's host view (volume/EQ turns).
    echo_tx: std::sync::mpsc::Sender<HostEcho>,
    echo_rx: std::sync::mpsc::Receiver<HostEcho>,
}

impl YtampApp {
    /// Builds the app around loaded settings. Fails only if the tokio
    /// runtime cannot start (fatal and worth a hard error).
    pub fn new(
        settings: Settings,
        settings_path: PathBuf,
        skins_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;

        // Player engine, DESIGN.md contract shape.
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (event_tx, _) = broadcast::channel(512);
        let engine_note;
        let mut events = None;
        let mut cmd_handle = None;
        match PlayerEngine::spawn(cmd_rx, event_tx.clone()) {
            Ok(_engine) => {
                events = Some(event_tx.subscribe());
                cmd_handle = Some(cmd_tx.clone());
                engine_note = Some("offline (stand-in engine)".to_string());
            }
            Err(error) => {
                log::error!("player engine failed to spawn: {error}");
                engine_note = Some(format!("failed: {error}"));
            }
        }

        // Search pipeline + trait-deferred skin loads.
        let (search_outcome_tx, search_rx) = std::sync::mpsc::channel();
        let (skin_request_tx, skin_request_rx) = std::sync::mpsc::channel();
        // Mini-player host echoes (volume/EQ changed through HostView).
        let (echo_tx, echo_rx) = std::sync::mpsc::channel();

        let cookies = settings
            .cookie_path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .filter(|text| !text.trim().is_empty());
        // INTEGRATOR: if Builder B's YtClient::new wants a path or a parsed
        // jar instead of raw cookie text, adapt here only.
        let yt = Arc::new(YtClient::new(cookies));

        let mut app = Self {
            mini: MiniState {
                winamp: WinampState {
                    scale: settings.skin_scale.clamp(1, 4),
                    ..WinampState::default()
                },
                ..MiniState::default()
            },
            settings,
            settings_path,
            settings_dirty: false,
            last_settings_save: None,
            cmd_tx: cmd_handle,
            events,
            state: PlaybackState::default(),
            spectrum: SpectrumFrame::default(),
            engine_note,
            rt,
            yt,
            search: SearchState::default(),
            search_outcome_tx,
            search_rx,
            skin_request_tx,
            skin_request_rx,
            toasts: Vec::new(),
            switch_intent: false,
            view: View::default(),
            eq_open: false,
            scrub: None,
            skins_dir,
            show_queue: true,
            window_title: "ytamp".into(),
            echo_tx,
            echo_rx,
        };
        app.state.volume = app.settings.volume;

        // Wear the remembered skin.
        if let Some(name) = app.settings.skin.clone() {
            app.load_skin_by_name(&name);
        }

        // Bring the engine up to the persisted settings.
        let volume = app.settings.volume;
        app.send_cmd(PlayerCommand::SetVolume(volume));
        app.send_eq();
        Ok(app)
    }

    /// Per-window setup: theme, image loaders, restored intent. Called every
    /// time a window is (re)created around this long-lived state.
    pub fn attach(&mut self, ctx: &egui::Context) {
        install_theme(ctx);
        install_fonts(ctx);
        egui_extras::install_image_loaders(ctx);
        self.switch_intent = false;
        if self.settings.winamp_window {
            // The mini player sizes itself; undo any restored main-window
            // state and put the window back where it was.
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            if let Some([x, y]) = self.mini.winamp.restore_pos {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
            }
        }
    }

    /// Frame logic: drains every channel, expires banners, saves settings.
    /// Runs before the UI pass, and alone while the window is hidden.
    pub fn background_frame(&mut self, ctx: &egui::Context) {
        self.drain_events();
        self.drain_search();
        self.drain_skin_requests();
        self.drain_echoes();
        self.tick_toasts();
        self.save_if_due();

        // Keep the loop alive for the things that move on their own.
        if self.state.playing {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.search.searching {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    // ---- commands -----------------------------------------------------

    pub fn send_cmd(&mut self, command: PlayerCommand) {
        if let Some(tx) = &self.cmd_tx
            && let Err(error) = tx.try_send(command)
        {
            log::warn!("player command dropped: {error}");
        }
    }

    /// Pushes the current EQ settings to the engine.
    pub fn send_eq(&mut self) {
        let eq = self.settings.eq;
        self.send_cmd(PlayerCommand::SetEq {
            enabled: eq.enabled,
            gains_db: eq.gains_db,
            preamp_db: eq.preamp_db,
        });
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.settings.volume = volume.clamp(0.0, 1.0);
        self.mark_dirty();
        self.send_cmd(PlayerCommand::SetVolume(self.settings.volume));
    }

    // ---- window mode --------------------------------------------------

    /// Flip main <-> mini. The eframe shell notices `switch_intent` and
    /// recreates the window; the caller sends the Close viewport command.
    pub fn toggle_mini(&mut self) {
        if self.settings.winamp_window {
            self.mini.winamp.remember_position();
        }
        self.settings.winamp_window = !self.settings.winamp_window;
        self.mark_dirty();
        self.switch_intent = true;
    }

    // ---- skins ----------------------------------------------------------

    /// Installs a skin file: copies it into the library, then loads it.
    pub fn install_skin(&mut self, path: &Path) {
        if !path.is_file() {
            self.toast_error(format!("{} is not a file", path.display()));
            return;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            self.toast_error(format!("{} has no file name", path.display()));
            return;
        };
        let dest = self.skins_dir.join(&name);
        if dest != path {
            if let Err(error) =
                std::fs::create_dir_all(&self.skins_dir).and_then(|()| std::fs::copy(path, &dest))
            {
                self.toast_error(format!("could not copy skin into library: {error}"));
                return;
            }
        }
        self.load_skin_by_name(&name);
    }

    /// Loads a library skin by file name and selects it.
    pub fn load_skin_by_name(&mut self, name: &str) {
        let path = self.skins_dir.join(name);
        match Skin::load(&path) {
            Ok(skin) => {
                self.mini.skin = Some(skin.clone());
                self.settings.skin = Some(name.to_string());
                self.mark_dirty();
                self.toast(format!("Skin: {}", skin.name));
            }
            Err(error) => self.toast_error(format!("skin {name}: {error}")),
        }
    }

    /// Drops the skin back to the built-in look.
    pub fn clear_skin(&mut self) {
        self.mini.skin = None;
        self.settings.skin = None;
        self.mark_dirty();
        self.toast("Skin: built-in");
    }

    /// The skins library, file name and path, sorted for listing.
    pub fn list_skins(&self) -> Vec<(String, PathBuf)> {
        let mut skins: Vec<(String, PathBuf)> = std::fs::read_dir(&self.skins_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && is_skin_file(path))
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                Some((name, path))
            })
            .collect();
        skins.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        skins
    }

    // ---- cookies --------------------------------------------------------

    /// Points the YT client at a cookie file (or none) and rebuilds it.
    pub fn set_cookie_path(&mut self, path: Option<String>) {
        self.settings.cookie_path = path.clone();
        self.mark_dirty();
        let cookies = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .filter(|text| !text.trim().is_empty());
        let loaded = cookies.is_some();
        self.yt = Arc::new(YtClient::new(cookies));
        let message = match (&path, loaded) {
            (Some(_), true) => "Cookies loaded".to_string(),
            (Some(p), false) => format!("Cookie file empty or missing: {p}"),
            _ => "Cookies cleared".to_string(),
        };
        self.toast(message);
    }

    // ---- search ---------------------------------------------------------

    /// Commits the typed query and spawns the fetch.
    pub fn begin_search(&mut self) {
        let query = self.search.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.settings.remember_search(&query);
        self.mark_dirty();
        self.search.committed = query.clone();
        self.search.searching = true;
        self.search.selected = None;
        self.search.scroll_to = None;
        self.view = View::Search;
        let yt = Arc::clone(&self.yt);
        let tx = self.search_outcome_tx.clone();
        self.rt.spawn(async move {
            let outcome = match yt.search_tracks(&query, 30).await {
                Ok(tracks) => SearchOutcome::Results { query, tracks },
                Err(error) => SearchOutcome::Failed(format!("search failed: {error}")),
            };
            let _ = tx.send(outcome);
        });
    }

    /// Click on result `index`: replaces the queue and starts a radio fetch.
    pub fn play_result(&mut self, index: usize) {
        let Some(track) = self.search.results.get(index) else {
            return;
        };
        let video_id = track.video_id.clone();
        let tracks = self.search.results.clone();
        self.search.selected = Some(index);
        self.send_cmd(PlayerCommand::QueueReplace(tracks, Some(index)));
        self.spawn_radio(&video_id);
    }

    fn spawn_radio(&mut self, video_id: &str) {
        let yt = Arc::clone(&self.yt);
        let tx = self.search_outcome_tx.clone();
        let video_id = video_id.to_string();
        self.rt.spawn(async move {
            let outcome = match yt.radio_for(&video_id, 20).await {
                Ok(tracks) => SearchOutcome::Radio { video_id, tracks },
                Err(error) => SearchOutcome::Failed(format!("radio failed: {error}")),
            };
            let _ = tx.send(outcome);
        });
    }

    fn apply_search_outcome(&mut self, outcome: SearchOutcome) {
        match outcome {
            SearchOutcome::Results { query, tracks } => {
                if query != self.search.committed {
                    return; // a stale answer for an older query
                }
                self.search.searching = false;
                if tracks.is_empty() {
                    self.toast(format!("No results for “{query}”"));
                }
                self.search.results = tracks;
                self.search.selected = (!self.search.results.is_empty()).then_some(0);
                self.search.scroll_to = None;
            }
            SearchOutcome::Radio { video_id, tracks } => {
                // Skip anything the queue already holds.
                let mut known: Vec<String> = self
                    .state
                    .queue
                    .iter()
                    .map(|t| t.video_id.clone())
                    .collect();
                if self.search.results.iter().any(|t| t.video_id == video_id) {
                    known.push(video_id.clone());
                }
                let fresh: Vec<Track> = tracks
                    .into_iter()
                    .filter(|t| !known.contains(&t.video_id))
                    .collect();
                let added = fresh.len();
                if added > 0 {
                    self.send_cmd(PlayerCommand::QueueAppend(fresh));
                    self.toast(format!("Radio: +{added} tracks"));
                }
            }
            SearchOutcome::Failed(error) => {
                self.search.searching = false;
                self.toast_error(error);
            }
        }
    }

    // ---- the Winamp host snapshot ---------------------------------------

    /// An owned [`WinampHost`] view of the app for the duration of one
    /// frame: the mini player's `winamp_ui` takes `&mut dyn WinampHost`
    /// while the skin state is borrowed separately, so the host carries
    /// copies (a [`PlaybackState`] per frame is cheap) and routes commands
    /// back through the live channels.
    pub fn host_snapshot(&self) -> HostView {
        HostView {
            commands: self.cmd_tx.clone(),
            state: self.state.clone(),
            spectrum: self.spectrum.clone(),
            eq: self.settings.eq,
            skin_requests: self.skin_request_tx.clone(),
            echoes: Some(self.echo_tx.clone()),
        }
    }

    // ---- banners --------------------------------------------------------

    pub fn toast(&mut self, text: impl Into<String>) {
        self.push_toast(ToastKind::Info, text);
    }

    pub fn toast_error(&mut self, text: impl Into<String>) {
        self.push_toast(ToastKind::Error, text);
    }

    fn push_toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast {
            kind,
            text: text.into(),
            until: Instant::now() + TOAST_LIFETIME,
        });
        // Keep the screen readable under a storm of events.
        while self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn tick_toasts(&mut self) {
        let now = Instant::now();
        self.toasts.retain(|toast| toast.until > now);
    }

    // ---- persistence ----------------------------------------------------

    pub fn mark_dirty(&mut self) {
        self.settings_dirty = true;
    }

    fn save_if_due(&mut self) {
        if !self.settings_dirty {
            return;
        }
        if self
            .last_settings_save
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(1500))
        {
            self.save_settings();
        }
    }

    pub fn save_settings(&mut self) {
        self.settings.save(&self.settings_path);
        self.settings_dirty = false;
        self.last_settings_save = Some(Instant::now());
    }

    // ---- drains -----------------------------------------------------------

    /// Drains the engine's broadcast channel into the app's snapshots.
    fn drain_events(&mut self) {
        let Some(events) = &mut self.events else {
            return;
        };
        // Collected first: applying a toast takes `&mut self`, which the
        // receiver borrow above cannot share.
        let mut incoming = Vec::new();
        let mut closed = false;
        loop {
            match events.try_recv() {
                Ok(event) => incoming.push(event),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    log::debug!("skipped {skipped} player events");
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    closed = true;
                    break;
                }
            }
        }
        for event in incoming {
            match event {
                PlayerEvent::State(state) => self.state = state,
                PlayerEvent::Spectrum(frame) => self.spectrum = frame,
                PlayerEvent::Error(text) => self.toast_error(text),
                PlayerEvent::Info(text) => self.toast(text),
            }
        }
        if closed {
            self.events = None;
            self.engine_note = Some("engine stopped".into());
            self.toast_error("The audio engine stopped; playback is unavailable.");
        }
    }

    fn drain_search(&mut self) {
        let mut outcomes = Vec::new();
        while let Ok(outcome) = self.search_rx.try_recv() {
            outcomes.push(outcome);
        }
        for outcome in outcomes {
            self.apply_search_outcome(outcome);
        }
    }

    fn drain_skin_requests(&mut self) {
        let mut paths = Vec::new();
        while let Ok(path) = self.skin_request_rx.try_recv() {
            paths.push(path);
        }
        for path in paths {
            self.install_skin(&path);
        }
    }

    fn drain_echoes(&mut self) {
        while let Ok(echo) = self.echo_rx.try_recv() {
            match echo {
                HostEcho::Volume(volume) => {
                    self.settings.volume = volume.clamp(0.0, 1.0);
                    self.mark_dirty();
                }
                HostEcho::Eq(eq) => {
                    self.settings.eq = eq;
                    self.mark_dirty();
                }
            }
        }
    }

    // ---- the frame ------------------------------------------------------

    /// One frame of drawing for whichever window mode is open. The eframe
    /// `Shell` in `main.rs` calls this with the root `Ui`.
    pub fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.global_keys(&ctx);
        self.handle_dropped_skins(&ctx);
        self.sync_window_title(&ctx);
        if self.settings.winamp_window {
            self.frame_mini(ui);
        } else {
            crate::ui::show(self, ui);
        }
        self.draw_toasts(&ctx);
    }

    /// The mini player's frame: remember where the window is, fit it to
    /// its wanted size, draw through the stand-in renderer, then fold its
    /// deferred intents back into the app.
    fn frame_mini(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(rect) = ctx.input(|input| input.viewport().outer_rect) {
            self.mini.winamp.last_pos = Some([rect.min.x, rect.min.y]);
        }
        self.mini.winamp.eq_open = self.eq_open;
        self.mini.winamp.eq_scratch = self.settings.eq;
        let wanted = standins::mini_player::desired_window_size(&self.mini.winamp);
        fit_mini_window(&ctx, wanted);

        let builtin;
        let skin = match &self.mini.skin {
            Some(skin) => skin,
            None => {
                builtin = Skin::builtin();
                &builtin
            }
        };
        let mut host = self.host_snapshot();
        winamp_ui(
            ui,
            &mut self.mini.winamp,
            skin,
            &mut host,
            &mut self.mini.textures,
        );

        if self.mini.winamp.wants_exit {
            self.mini.winamp.wants_exit = false;
            self.toggle_mini();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.mini.winamp.wants_eq {
            self.mini.winamp.wants_eq = false;
            self.eq_open = !self.eq_open;
        }
        let scale = self.mini.winamp.scale.clamp(1, 4);
        if scale != self.settings.skin_scale {
            self.mini.winamp.scale = scale;
            self.settings.skin_scale = scale;
            self.mark_dirty();
        }
    }

    /// Ctrl+M flips main <-> mini in either mode; the shell's window loop
    /// reopens as the other kind.
    fn global_keys(&mut self, ctx: &egui::Context) {
        let mut toggle = false;
        ctx.input(|input| {
            for event in &input.events {
                let egui::Event::Key {
                    key: egui::Key::M,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    continue;
                };
                if modifiers.ctrl && !modifiers.shift {
                    toggle = true;
                }
            }
        });
        if toggle {
            self.toggle_mini();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Installs skins dropped on the window from the desktop.
    fn handle_dropped_skins(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        for path in dropped {
            if is_skin_file(&path) {
                self.install_skin(&path);
            }
        }
    }

    /// Keeps the running track in the window and taskbar title.
    fn sync_window_title(&mut self, ctx: &egui::Context) {
        let title = match (&self.state.playing, &self.state.track) {
            (true, Some(track)) => format!("{} — ytamp", track.display()),
            _ => "ytamp".to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// The banner stack, bottom-center; click one to dismiss it.
    fn draw_toasts(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let mut dismiss = None;
        egui::Area::new(egui::Id::new("ytamp-toasts"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -18.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.vertical(|ui| {
                    for (index, toast) in self.toasts.iter().enumerate() {
                        let (mark, stroke) = if toast.kind == ToastKind::Error {
                            (
                                "⚠",
                                egui::Stroke::new(1.0, egui::Color32::from_rgb(0xE8, 0x6A, 0x5A)),
                            )
                        } else {
                            ("ⓘ", egui::Stroke::NONE)
                        };
                        let response = egui::Frame::window(ui.style())
                            .fill(egui::Color32::from_rgb(0x1E, 0x1B, 0x2A))
                            .stroke(stroke)
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.set_min_width(360.0);
                                ui.horizontal(|ui| {
                                    ui.label(mark);
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(&toast.text).weak())
                                            .wrap(),
                                    );
                                });
                            })
                            .response;
                        if response.clicked() {
                            dismiss = Some(index);
                        }
                    }
                });
            });
        if let Some(index) = dismiss {
            self.toasts.remove(index);
        }
    }
}

/// The frozen host trait implementation over the whole app.
impl WinampHost for YtampApp {
    fn cmd(&mut self, c: PlayerCommand) {
        self.send_cmd(c);
    }

    fn state(&self) -> &PlaybackState {
        &self.state
    }

    fn spectrum(&self) -> SpectrumFrame {
        self.spectrum.clone()
    }

    fn eq(&self) -> EqSettings {
        self.settings.eq
    }

    fn load_skin_file(&mut self, path: &Path) {
        self.install_skin(path);
    }
}

/// What the mini player changed through its host view this frame; the
/// shell folds these back into settings so the knobs persist.
#[derive(Debug)]
enum HostEcho {
    Volume(f32),
    Eq(EqSettings),
}

/// One-frame owned host view; see [`YtampApp::host_snapshot`].
pub struct HostView {
    commands: Option<mpsc::Sender<PlayerCommand>>,
    state: PlaybackState,
    spectrum: SpectrumFrame,
    eq: EqSettings,
    skin_requests: std::sync::mpsc::Sender<PathBuf>,
    echoes: Option<std::sync::mpsc::Sender<HostEcho>>,
}

impl WinampHost for HostView {
    fn cmd(&mut self, c: PlayerCommand) {
        // Volume and EQ turns also echo to the app, which persists them;
        // everything still goes to the engine.
        if let Some(echoes) = &self.echoes {
            match &c {
                PlayerCommand::SetVolume(volume) => {
                    let _ = echoes.send(HostEcho::Volume(*volume));
                }
                PlayerCommand::SetEq {
                    enabled,
                    gains_db,
                    preamp_db,
                } => {
                    let _ = echoes.send(HostEcho::Eq(EqSettings {
                        enabled: *enabled,
                        gains_db: *gains_db,
                        preamp_db: *preamp_db,
                    }));
                }
                _ => {}
            }
        }
        if let Some(tx) = &self.commands {
            let _ = tx.try_send(c);
        }
    }

    fn state(&self) -> &PlaybackState {
        &self.state
    }

    fn spectrum(&self) -> SpectrumFrame {
        self.spectrum.clone()
    }

    fn eq(&self) -> EqSettings {
        self.eq
    }

    fn load_skin_file(&mut self, path: &Path) {
        let _ = self.skin_requests.send(path.to_path_buf());
    }
}

/// Midnight theme: dark backgrounds, purple accents, generous spacing.
pub fn install_theme(ctx: &egui::Context) {
    let accent = egui::Color32::from_rgb(0x8B, 0x5C, 0xF6);
    let accent_soft = egui::Color32::from_rgb(0x5B, 0x40, 0x99);
    let accent_bright = egui::Color32::from_rgb(0xA7, 0x8B, 0xFA);
    let window = egui::Color32::from_rgb(0x12, 0x11, 0x18);
    let panel = egui::Color32::from_rgb(0x18, 0x16, 0x21);
    let card = egui::Color32::from_rgb(0x1E, 0x1B, 0x2A);
    let text = egui::Color32::from_rgb(0xE7, 0xE5, 0xF0);
    let dim = egui::Color32::from_rgb(0x9C, 0x98, 0xAD);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = window;
    visuals.faint_bg_color = card;
    visuals.extreme_bg_color = egui::Color32::from_rgb(0x0C, 0x0B, 0x10);
    visuals.code_bg_color = card;
    visuals.hyperlink_color = accent_bright;
    visuals.selection.bg_fill = accent;
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    visuals.widgets.noninteractive.bg_fill = panel;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, dim);
    visuals.widgets.inactive.bg_fill = card;
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text);
    visuals.widgets.hovered.bg_fill = accent_soft;
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    visuals.widgets.open.bg_fill = accent_soft;
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, text);

    // Written for both themes and the selection pinned to dark: the app is
    // a night thing.
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        ctx.set_visuals_of(theme, visuals.clone());
    }
    ctx.set_theme(egui::ThemePreference::Dark);
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> (tempfile::TempDir, YtampApp) {
        let dir = tempfile::tempdir().unwrap();
        let app = YtampApp::new(
            Settings::default(),
            dir.path().join("settings.json"),
            dir.path().join("skins"),
        )
        .unwrap();
        (dir, app)
    }

    #[test]
    fn host_view_routes_commands_and_defers_skin_loads() {
        let (_dir, app) = test_app();
        let mut host = app.host_snapshot();
        host.cmd(PlayerCommand::SetVolume(0.25));
        assert!(!host.state().playing);
        assert!(host.state().queue.is_empty());
        assert_eq!(host.eq(), EqSettings::default());

        // Skin requests defer back through the app's channel.
        host.load_skin_file(Path::new("/tmp/whatever.wsz"));
        let path = app
            .skin_request_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(path, PathBuf::from("/tmp/whatever.wsz"));
    }

    #[test]
    fn toggling_mini_flips_mode_and_arms_the_switch() {
        let (_dir, mut app) = test_app();
        assert!(!app.settings.winamp_window);
        app.toggle_mini();
        assert!(app.settings.winamp_window);
        assert!(app.switch_intent);
        app.toggle_mini();
        assert!(!app.settings.winamp_window);
    }

    #[test]
    fn the_engine_simulates_a_queue() {
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        PlayerEngine::spawn(cmd_rx, event_tx).unwrap();

        let first = standins::canned_track("sim", 0);
        cmd_tx
            .try_send(PlayerCommand::QueueReplace(
                vec![first.clone(), standins::canned_track("sim", 1)],
                Some(0),
            ))
            .unwrap();

        let mut playing_state = None;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(PlayerEvent::State(state)) = event_rx.try_recv()
                && state.playing
                && state.track.is_some()
            {
                playing_state = Some(state);
                break;
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let state = playing_state.expect("engine reported playing state");
        assert_eq!(state.track.as_ref().unwrap().video_id, first.video_id);
        assert_eq!(state.queue.len(), 2);
        assert_eq!(state.queue_index, Some(0));
    }

    #[test]
    fn canned_search_and_radio_are_deterministic_and_distinct() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let yt = YtClient::new(None);
        let a = rt.block_on(yt.search_tracks("lofi", 5)).unwrap();
        let b = rt.block_on(yt.search_tracks("lofi", 5)).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 5);
        assert!(a.iter().all(|t| t.video_id.len() == 11));

        let radio = rt.block_on(yt.radio_for(&a[0].video_id, 20)).unwrap();
        assert_eq!(radio.len(), 20);
        assert!(radio.iter().all(|t| t.video_id != a[0].video_id));
    }

    #[test]
    fn installing_a_skin_copies_it_into_the_library_and_selects_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = YtampApp::new(
            Settings::default(),
            dir.path().join("settings.json"),
            dir.path().join("skins"),
        )
        .unwrap();

        let source = dir.path().join("My-Skin.wsz");
        std::fs::write(&source, b"PK\x03\x04 fake zip bytes").unwrap();

        app.install_skin(&source);
        assert!(app.mini.skin.is_some());
        assert_eq!(app.mini.skin.as_ref().unwrap().name, "My-Skin");
        assert_eq!(app.settings.skin.as_deref(), Some("My-Skin.wsz"));
        assert!(dir.path().join("skins/My-Skin.wsz").is_file());
        assert!(app.toasts.iter().any(|t| t.text.contains("My-Skin")));

        // A broken archive is reported, never fatal.
        let bad = dir.path().join("bad.wsz");
        std::fs::write(&bad, b"not a zip").unwrap();
        app.install_skin(&bad);
        assert!(app.toasts.iter().any(|t| t.kind == ToastKind::Error));
        assert_eq!(app.settings.skin.as_deref(), Some("My-Skin.wsz"));
    }

    #[test]
    fn radio_outcomes_append_only_fresh_tracks_end_to_end() {
        let (_dir, mut app) = test_app();
        let seed: Vec<Track> = (0..3).map(|i| standins::canned_track("q", i)).collect();
        app.search.results = seed.clone();
        app.play_result(0);
        // QueueReplace went out; drain the engine echo to see it.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && app.state.queue.is_empty() {
            app.drain_events();
            if app.state.queue.is_empty() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert_eq!(app.state.queue.len(), 3);
        assert_eq!(app.state.queue_index, Some(0));

        // Radio arrives: the three known ids must not be re-appended.
        let mut radio: Vec<Track> = seed.clone();
        radio.push(standins::canned_track("radio:x", 0));
        let video_id = seed[0].video_id.clone();
        app.apply_search_outcome(SearchOutcome::Radio {
            video_id,
            tracks: radio,
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && app.state.queue.len() < 4 {
            app.drain_events();
            if app.state.queue.len() < 4 {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert_eq!(app.state.queue.len(), 4);
        assert!(app.toasts.iter().any(|t| t.text.contains("+1")));
    }
}
