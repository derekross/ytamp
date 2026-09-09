//! ytamp — the application core: `YtampApp` owns settings, the player
//! engine handles, the search pipeline, the skin state, and the banners.
//! It implements [`WinampHost`], the frozen trait the skin engine draws
//! against (docs/DESIGN.md).
//!
//! Integrated: the real implementations from Builders A and B are wired
//! in below; only the window-position memory (`MiniPos`) and the engine
//! handle retention are glue added at integration time.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

use crate::model::{PlaybackState, PlayerCommand, PlayerEvent, SpectrumFrame, Track};
use crate::settings::{EqSettings, Settings};

// Real implementations (Builders A and B), per the frozen contracts.
pub use crate::player::PlayerEngine;
pub use crate::skin::{Skin, SkinError};
pub use crate::ui::winamp::winamp_ui;
pub use crate::winamp::{SkinTextures, WinampHost, WinampState};
pub use crate::yt::YtClient;

/// How long a toast/banner stays on screen.
pub const TOAST_LIFETIME: Duration = Duration::from_secs(4);

/// Test-only canned tracks (deterministic, YouTube-shaped ids).
#[cfg(test)]
pub mod canned {
    use crate::model::Track;

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

    /// Deterministic canned track from a seed.
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

/// The `m:ss` clock readout, the Winamp way (used by the main window
/// surfaces; the skinned mini player draws its own pixel clock).
pub fn fmt_time(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
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

/// Where to download a skin from, and what to call the file, for a Skin
/// Museum page link (`https://skins.webamp.org/skin/<md5>/<name>.wsz/`,
/// served from `r2.webampskins.org`) or a direct `.wsz`/`.zip` URL.
pub fn skin_download_for(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return None;
    }
    let mut parts = url.splitn(4, '/');
    let host = parts.nth(2).unwrap_or("");
    let path = parts.next().unwrap_or("");
    if host == "skins.webamp.org" || host == "webamp.org" {
        let mut parts = path.trim_end_matches('/').split('/');
        if parts.next() != Some("skin") {
            return None;
        }
        let hash = parts.next()?;
        if hash.len() != 32 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let name = parts
            .next()
            .map(|n| urlencoding::decode(n).map_or(n.to_string(), |d| d.into_owned()))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("{hash}.wsz"));
        let name = if is_skin_file(Path::new(&name)) {
            name
        } else {
            format!("{name}.wsz")
        };
        return Some((
            format!("https://r2.webampskins.org/skins/{hash}.wsz"),
            safe_file_name(&name),
        ));
    }
    let file = path.split(['?', '#']).next()?.rsplit('/').next()?;
    let file = urlencoding::decode(file).map_or(file.to_string(), |d| d.into_owned());
    is_skin_file(Path::new(&file)).then(|| (url.to_string(), safe_file_name(&file)))
}

/// A file name with nothing that could leave the skins directory.
fn safe_file_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '\0' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// How long a compositor gets to honour a resize before the mini window
/// is reopened at the wanted size instead.
const RESIZE_PATIENCE: f64 = 1.5;

/// Fits the fixed-size mini window to its wanted size. Compositors refuse
/// chatty resize requests, so a rejected ask retries at most once a
/// second (fastpotify's pattern). Returns true when the window has been
/// the wrong size for longer than [`RESIZE_PATIENCE`]: a tiling or
/// otherwise stubborn compositor, and the caller should reopen instead.
fn fit_mini_window(ctx: &egui::Context, wanted: egui::Vec2) -> bool {
    let current = ctx.input(|input| {
        input
            .viewport()
            .inner_rect
            .map(|rect| rect.size())
            .unwrap_or(wanted)
    });
    let asked = egui::Id::new("ytamp-mini-fit");
    let since = egui::Id::new("ytamp-mini-fit-since");
    if (current - wanted).abs().max_elem() < 1.0 {
        ctx.data_mut(|data| data.remove::<f64>(since));
        return false;
    }
    let now = ctx.input(|input| input.time);
    let first: f64 = ctx.data_mut(|data| *data.get_temp_mut_or(since, now));
    if now - first > RESIZE_PATIENCE {
        log::debug!("the compositor kept the mini window at {current:?}, wanted {wanted:?}");
        ctx.data_mut(|data| data.remove::<f64>(since));
        return true;
    }
    let last: Option<f64> = ctx.data(|data| data.get_temp(asked));
    if last.is_some_and(|last| now - last < 1.0) {
        return false;
    }
    ctx.data_mut(|data| data.insert_temp(asked, now));
    ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(wanted));
    ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(wanted));
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(wanted));
    false
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

/// Watches the Downloads folder for skins saved while the app runs, so
/// the Skin Museum's Download button is all it takes.
pub struct DownloadsWatch {
    dir: PathBuf,
    /// Files present at the last look, so only new ones are imported.
    seen: std::collections::HashSet<PathBuf>,
    last_look: Option<Instant>,
}

impl DownloadsWatch {
    /// How often the folder is read.
    const PERIOD: Duration = Duration::from_secs(3);
    /// A file changed this recently may still be being written.
    const SETTLE: Duration = Duration::from_secs(2);

    /// Starts watching `dir`; whatever is there now is not imported.
    pub fn new(dir: PathBuf) -> Self {
        let mut watch = Self {
            dir,
            seen: std::collections::HashSet::new(),
            last_look: None,
        };
        watch.seen = watch.skins_present().into_iter().map(|(p, _)| p).collect();
        watch
    }

    fn skins_present(&self) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
        std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_skin_file(path) && path.is_file())
            .map(|path| {
                let modified = path.metadata().and_then(|m| m.modified()).ok();
                (path, modified)
            })
            .collect()
    }

    /// New, settled skin files since the last look, at most once a period.
    pub fn poll(&mut self) -> Vec<PathBuf> {
        if self.last_look.is_some_and(|at| at.elapsed() < Self::PERIOD) {
            return Vec::new();
        }
        self.last_look = Some(Instant::now());
        let now = std::time::SystemTime::now();
        let mut fresh = Vec::new();
        for (path, modified) in self.skins_present() {
            if self.seen.contains(&path) {
                continue;
            }
            let settled = modified
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age >= Self::SETTLE);
            if settled {
                self.seen.insert(path.clone());
                fresh.push(path);
            }
        }
        fresh
    }
}

/// A skin change asked for from somewhere that cannot touch the app
/// directly: the skinned UI's host view, or a download that finished.
#[derive(Debug)]
pub enum SkinRequest {
    /// Install a file from disk (a drop).
    File(PathBuf),
    /// Wear a library skin by name, or the built-in one.
    Library(Option<String>),
    /// A download failed; the text is for a toast.
    Failed(String),
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
#[derive(Default)]
pub struct MiniState {
    pub skin: Option<Arc<Skin>>,
    pub winamp: WinampState,
    pub textures: SkinTextures,
    /// Where the mini window was when it last closed (integration glue:
    /// the skinned UI owns no window position).
    pub pos: MiniPos,
}

/// The mini window's position memory: where it is now, and where it
/// should come back next time it opens.
#[derive(Clone, Copy, Debug, Default)]
pub struct MiniPos {
    pub last: Option<[f32; 2]>,
    pub restore: Option<[f32; 2]>,
}

impl MiniPos {
    /// The last known position becomes the one to restore to.
    pub fn remember(&mut self) {
        if let Some(pos) = self.last {
            self.restore = Some(pos);
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
    /// The engine itself: kept alive for the app's lifetime (its Drop is
    /// the engine's kill switch).
    #[allow(dead_code)] // retention is the point; it is never read back
    engine_handle: Option<PlayerEngine>,
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

    /// Skin changes requested from inside the Winamp UI or by downloads.
    skin_request_tx: std::sync::mpsc::Sender<SkinRequest>,
    skin_request_rx: std::sync::mpsc::Receiver<SkinRequest>,
    /// The skin library as last listed, and when; the mini player's menu
    /// reads it every frame, so the directory is read once a second.
    skin_list: Vec<String>,
    skin_list_at: Option<Instant>,
    /// The Settings page's "import from URL" box.
    pub skin_url: String,
    /// The Downloads folder watch, while the setting is on.
    downloads: Option<DownloadsWatch>,

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
    /// Set by the skinned UI's eject button (via `WinampHost::leave_mini_player`);
    /// the frame folds it into a mode switch after the draw.
    leave_mini_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Set by the skinned UI's EQ toggle (via `WinampHost::toggle_eq_window`).
    eq_toggle_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Set by the skinned UI's playlist toggle (via
    /// `WinampHost::toggle_playlist_window`); folds into `mini.winamp`
    /// after the draw.
    playlist_toggle_requested: Arc<std::sync::atomic::AtomicBool>,
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

        // Player engine, DESIGN.md contract shape. The handle is kept:
        // dropping it would stop the engine (its Drop is the kill switch).
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (event_tx, _) = broadcast::channel(512);
        let engine_note;
        let mut events = None;
        let mut cmd_handle = None;
        let mut engine_handle = None;
        match PlayerEngine::spawn(cmd_rx, event_tx.clone()) {
            Ok(engine) => {
                engine_handle = Some(engine);
                events = Some(event_tx.subscribe());
                cmd_handle = Some(cmd_tx.clone());
                engine_note = None;
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
                winamp: {
                    // Struct-literal spread would fail: the marquee fields
                    // are private. Set the public scale after the default.
                    let mut winamp = WinampState::default();
                    winamp.scale = u32::from(settings.skin_scale.clamp(1, 4));
                    winamp
                },
                ..MiniState::default()
            },
            settings,
            settings_path,
            settings_dirty: false,
            last_settings_save: None,
            cmd_tx: cmd_handle,
            events,
            engine_handle,
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
            skin_list: Vec::new(),
            skin_list_at: None,
            skin_url: String::new(),
            downloads: None,
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
            leave_mini_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            eq_toggle_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            playlist_toggle_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        app.state.volume = app.settings.volume;

        // Wear the remembered skin.
        if let Some(name) = app.settings.skin.clone() {
            app.load_skin_by_name(&name);
        }
        app.sync_downloads_watch();

        // Bring the engine up to the persisted settings.
        let volume = app.settings.volume;
        app.send_cmd(PlayerCommand::SetVolume(volume));
        app.send_eq();
        Ok(app)
    }

    /// Per-window setup: theme, image loaders, restored intent. Called every
    /// time a window is (re)created around this long-lived state.
    pub fn attach(&mut self, ctx: &egui::Context) {
        // Textures belong to the window that uploaded them; a new window
        // has a new painter, so the skin and the playlist's text are
        // uploaded again on their first frame.
        self.mini.textures.clear();
        self.mini.winamp.playlist_text.clear();
        install_theme(ctx);
        install_fonts(ctx);
        egui_extras::install_image_loaders(ctx);
        self.switch_intent = false;
        if self.settings.winamp_window {
            // The mini player sizes itself; undo any restored main-window
            // state and put the window back where it was.
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            if let Some([x, y]) = self.mini.pos.restore {
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
        self.poll_downloads();
        self.tick_toasts();
        self.save_if_due();
        if self
            .skin_list_at
            .is_none_or(|at| at.elapsed() > Duration::from_secs(1))
        {
            self.skin_list = self
                .list_skins()
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            self.skin_list_at = Some(Instant::now());
        }

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
        if self.downloads.is_some() {
            ctx.request_repaint_after(DownloadsWatch::PERIOD);
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
            self.mini.pos.remember();
        }
        self.settings.winamp_window = !self.settings.winamp_window;
        self.mark_dirty();
        self.switch_intent = true;
    }

    // ---- skins ----------------------------------------------------------

    /// Starts or stops the Downloads watch to match the setting.
    pub fn sync_downloads_watch(&mut self) {
        match (self.settings.watch_downloads, self.downloads.is_some()) {
            (true, false) => {
                self.downloads = crate::settings::downloads_dir().map(DownloadsWatch::new);
            }
            (false, true) => self.downloads = None,
            _ => {}
        }
    }

    /// Imports skins that landed in Downloads since the last look.
    fn poll_downloads(&mut self) {
        let fresh = match self.downloads.as_mut() {
            Some(watch) => watch.poll(),
            None => return,
        };
        for path in fresh {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.toast(format!("Found {} in Downloads", skin_display_label(&name)));
            self.install_skin(&path);
        }
    }

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
        if dest != path
            && let Err(error) =
                std::fs::create_dir_all(&self.skins_dir).and_then(|()| std::fs::copy(path, &dest))
        {
            self.toast_error(format!("could not copy skin into library: {error}"));
            return;
        }
        self.skin_list_at = None;
        self.load_skin_by_name(&name);
    }

    /// Imports a skin from a URL: a Skin Museum page
    /// (`skins.webamp.org/skin/<hash>/<name>.wsz/`) or a direct `.wsz`
    /// link. The download runs in the background and lands as a
    /// [`SkinRequest`].
    pub fn import_skin_url(&mut self, url: &str) {
        let Some((download, file_name)) = skin_download_for(url) else {
            self.toast_error("Not a Skin Museum link or a .wsz URL");
            return;
        };
        self.toast(format!("Downloading {}…", skin_display_label(&file_name)));
        let dest = self.skins_dir.join(&file_name);
        let skins_dir = self.skins_dir.clone();
        let tx = self.skin_request_tx.clone();
        self.rt.spawn(async move {
            let fetched = async {
                let response = reqwest::Client::new()
                    .get(&download)
                    .timeout(Duration::from_secs(60))
                    .send()
                    .await?
                    .error_for_status()?;
                let bytes = response.bytes().await?;
                std::fs::create_dir_all(&skins_dir)?;
                std::fs::write(&dest, &bytes)?;
                anyhow::Ok(dest)
            }
            .await;
            let _ = tx.send(match fetched {
                Ok(path) => SkinRequest::File(path),
                Err(error) => SkinRequest::Failed(format!("skin download failed: {error:#}")),
            });
        });
    }

    /// Loads a library skin by file name and selects it.
    pub fn load_skin_by_name(&mut self, name: &str) {
        let path = self.skins_dir.join(name);
        match Skin::load(&path) {
            Ok(skin) => {
                self.toast(format!("Skin: {}", skin.name));
                self.mini.skin = Some(Arc::new(skin));
                self.settings.skin = Some(name.to_string());
                self.mark_dirty();
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
        skins.sort_by_key(|a| a.0.to_lowercase());
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
            skin_library: self.skin_list.clone(),
            worn_skin: self.settings.skin.clone(),
            echoes: Some(self.echo_tx.clone()),
            leave_mini: Some(self.leave_mini_requested.clone()),
            toggle_eq: Some(self.eq_toggle_requested.clone()),
            toggle_playlist: Some(self.playlist_toggle_requested.clone()),
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
        let mut requests = Vec::new();
        while let Ok(request) = self.skin_request_rx.try_recv() {
            requests.push(request);
        }
        for request in requests {
            match request {
                SkinRequest::File(path) => self.install_skin(&path),
                SkinRequest::Library(Some(name)) => self.load_skin_by_name(&name),
                SkinRequest::Library(None) => self.clear_skin(),
                SkinRequest::Failed(text) => self.toast_error(text),
            }
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
        self.handle_pasted_links(&ctx);
        self.sync_window_title(&ctx);
        if self.settings.winamp_window {
            self.frame_mini(ui);
        } else {
            crate::ui::show(self, ui);
        }
        self.draw_toasts(&ctx);
    }

    /// The mini player's frame: remember where the window is, fit it to
    /// its wanted size, draw through the skinned renderer, then fold the
    /// host's deferred intents back into the app.
    fn frame_mini(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(rect) = ctx.input(|input| input.viewport().outer_rect) {
            self.mini.pos.last = Some([rect.min.x, rect.min.y]);
        }
        // The app owns which side windows are open; the skinned state is
        // kept in step before the draw.
        self.mini.winamp.eq_open = self.eq_open;
        let wanted = crate::ui::winamp::window_size(&self.mini.winamp);
        if fit_mini_window(&ctx, wanted) {
            // Reopen at the right size: the shell's loop makes a new mini
            // window when the intent is set while the mode stays mini.
            self.mini.pos.remember();
            self.switch_intent = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // The skinless look is the engine's generated built-in skin.
        let builtin;
        let skin: &Skin = match &self.mini.skin {
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

        use std::sync::atomic::Ordering;
        if self.leave_mini_requested.swap(false, Ordering::Relaxed) {
            self.toggle_mini();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.eq_toggle_requested.swap(false, Ordering::Relaxed) {
            self.eq_open = !self.eq_open;
        }
        // The playlist's open flag lives in the skinned state, which the
        // skin UI flips itself; the host call is only a notification.
        self.playlist_toggle_requested
            .store(false, Ordering::Relaxed);
        let scale = self.mini.winamp.scale.clamp(1, 4);
        if scale != u32::from(self.settings.skin_scale) {
            self.mini.winamp.scale = scale;
            self.settings.skin_scale = scale as u8;
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

    /// A Skin Museum link or `.wsz` URL pasted anywhere but a text box
    /// imports the skin.
    fn handle_pasted_links(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let pasted: Vec<String> = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Paste(text) => Some(text.trim().to_string()),
                    _ => None,
                })
                .collect()
        });
        for text in pasted {
            if skin_download_for(&text).is_some() {
                self.import_skin_url(&text);
            }
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
        // The skinned UI forwards its own drops through the host view.
        if self.settings.winamp_window {
            return;
        }
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

    fn skin_library(&self) -> Vec<String> {
        self.skin_list.clone()
    }

    fn worn_skin(&self) -> Option<String> {
        self.settings.skin.clone()
    }

    fn wear_skin(&mut self, name: Option<&str>) {
        match name {
            Some(name) => self.load_skin_by_name(name),
            None => self.clear_skin(),
        }
    }

    fn toggle_eq_window(&mut self) {
        self.eq_open = !self.eq_open;
    }

    fn toggle_playlist_window(&mut self) {
        // `WinampState::playlist_open` is flipped by the skin UI itself.
    }

    fn leave_mini_player(&mut self) {
        // The eject button leaves the mini player.
        self.toggle_mini();
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
    skin_requests: std::sync::mpsc::Sender<SkinRequest>,
    skin_library: Vec<String>,
    worn_skin: Option<String>,
    echoes: Option<std::sync::mpsc::Sender<HostEcho>>,
    /// Deferred intents the app folds in after the draw (borrows force it:
    /// `winamp_ui` holds `&mut mini.winamp` while the host runs).
    leave_mini: Option<Arc<std::sync::atomic::AtomicBool>>,
    toggle_eq: Option<Arc<std::sync::atomic::AtomicBool>>,
    toggle_playlist: Option<Arc<std::sync::atomic::AtomicBool>>,
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
        if is_skin_file(path) {
            let _ = self
                .skin_requests
                .send(SkinRequest::File(path.to_path_buf()));
        }
    }

    fn skin_library(&self) -> Vec<String> {
        self.skin_library.clone()
    }

    fn worn_skin(&self) -> Option<String> {
        self.worn_skin.clone()
    }

    fn wear_skin(&mut self, name: Option<&str>) {
        let _ = self
            .skin_requests
            .send(SkinRequest::Library(name.map(str::to_string)));
    }

    fn toggle_eq_window(&mut self) {
        if let Some(flag) = &self.toggle_eq {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn toggle_playlist_window(&mut self) {
        if let Some(flag) = &self.toggle_playlist {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn leave_mini_player(&mut self) {
        if let Some(flag) = &self.leave_mini {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
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
        let request = app
            .skin_request_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(
            matches!(request, SkinRequest::File(path) if path == Path::new("/tmp/whatever.wsz"))
        );
        host.wear_skin(Some("Zaxon.wsz"));
        let request = app
            .skin_request_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(matches!(request, SkinRequest::Library(Some(name)) if name == "Zaxon.wsz"));
    }

    #[test]
    fn the_downloads_watch_reports_only_new_settled_skins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.wsz"), b"old").unwrap();
        let mut watch = DownloadsWatch::new(dir.path().to_path_buf());
        assert!(
            watch.poll().is_empty(),
            "what was already there is not imported"
        );

        let fresh = dir.path().join("new.wsz");
        std::fs::write(&fresh, b"new").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        // Just written: still settling, and the period has not passed.
        watch.last_look = None;
        assert!(watch.poll().is_empty());
        // Backdate it and look again.
        let old = std::time::SystemTime::now() - Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&fresh)
            .unwrap()
            .set_modified(old)
            .unwrap();
        watch.last_look = None;
        assert_eq!(watch.poll(), vec![fresh]);
        watch.last_look = None;
        assert!(watch.poll().is_empty(), "reported once");
    }

    #[test]
    fn museum_links_and_direct_urls_become_downloads() {
        let (url, name) = skin_download_for(
            "https://skins.webamp.org/skin/edbd0697172ad1ad1546ecf6bc5e4239/Axon_amp.wsz/",
        )
        .unwrap();
        assert_eq!(
            url,
            "https://r2.webampskins.org/skins/edbd0697172ad1ad1546ecf6bc5e4239.wsz"
        );
        assert_eq!(name, "Axon_amp.wsz");
        let (url, name) = skin_download_for("https://example.com/dl/Some%20Skin.wsz?x=1").unwrap();
        assert_eq!(url, "https://example.com/dl/Some%20Skin.wsz?x=1");
        assert_eq!(name, "Some Skin.wsz");
        assert!(skin_download_for("https://skins.webamp.org/").is_none());
        assert!(skin_download_for("https://example.com/readme.txt").is_none());
        assert!(skin_download_for("not a url").is_none());
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
    fn installing_a_skin_copies_it_into_the_library_and_selects_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = YtampApp::new(
            Settings::default(),
            dir.path().join("settings.json"),
            dir.path().join("skins"),
        )
        .unwrap();

        // A real skin from the skin engine's testdata; renamed to prove the
        // file name, not the skin's contents, drives the library entry.
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/base-2.91.wsz");
        let source = dir.path().join("My-Skin.wsz");
        std::fs::copy(&fixture, &source).unwrap();

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
        let seed: Vec<Track> = (0..3).map(|i| canned::canned_track("q", i)).collect();
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
        radio.push(canned::canned_track("radio:x", 0));
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
