# ytamp — Design

**Mission:** fastpotify's experience for YouTube Music: native Rust + egui, Winamp `.wsz` mini player, spectrum analyser, 10-band EQ.

**Research grounding:** `docs/01-landscape.md` (gap confirmed: nobody has native GUI + Winamp skins + analyser for YTM), `docs/02-api-feasibility.md` (layered audio strategy: InnerTube client chain + yt-dlp fallback).

## Architecture

```
┌──────────────────────────── egui app (eframe) ────────────────────────────┐
│  Main window (search / results / queue / now-playing / settings)          │
│  Winamp mini player + EQ windows (own viewports, .wsz-rendered)           │
│                                                                           │
│  App implements WinampHost ── issues PlayerCommand, receives PlayerEvent  │
├───────────────────────────────────────────────────────────────────────────┤
│  skin engine     src/skin/* + src/ui/winamp/*   (ported from fastpotify)  │
│  audio engine    src/audio/* + src/player.rs    (rodio + symphonia + EQ   │
│                  + FFT analyser tap)                                      │
│  YT layer        src/yt/*   (InnerTube metadata + player/stream resolver  │
│                  with client fallback chain + yt-dlp escape hatch)        │
└───────────────────────────────────────────────────────────────────────────┘
```

## Module ownership (parallel build)

| Area | Owner | Files |
|---|---|---|
| Skin engine + winamp UI + EQ DSP + vis | Builder A | `src/skin/{mod,zip,sprites,layout,config,font}.rs`, `src/ui/winamp/{mod,playlist,equalizer,pixel_text}.rs`, `src/eq.rs`, `src/vis.rs`, `src/winamp.rs` |
| YT data + resolver + audio playback | Builder B | `src/yt/{mod,innertube,resolver,search}.rs`, `src/audio/{mod,decode,sink,analyser}.rs`, `src/player.rs` |
| App shell + main UI + wiring | Builder C | `src/main.rs`, `src/app.rs`, `src/ui/{main,search,queue,settings}.rs`, `src/model.rs` (owns final shape) |

Integration: Centauri. Contracts below are **frozen** — if a builder must deviate, they keep the public names and add an extension rather than changing signatures.

## Contract: `src/model.rs` (written first; everyone codes against it)

```rust
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: Option<u64>,
    pub thumb_url: Option<String>,   // thumbnail; app loads via egui_extras
}

pub enum PlayerCommand {
    PlayAt(usize),              // jump to queue index
    QueueReplace(Vec<Track>, Option<usize>), // new queue, optional start index
    QueueAppend(Vec<Track>),
    Next, Prev, PlayPause, Pause, Resume, Stop,
    SeekRatio(f64),             // 0..=1
    SetVolume(f32),             // 0..=1
    SetEq { enabled: bool, gains_db: [f64; 10], preamp_db: f64 },
}

#[derive(Clone)]
pub struct PlaybackState {
    pub playing: bool,
    pub track: Option<Track>,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub volume: f32,
    pub queue: Vec<Track>,
    pub queue_index: Option<usize>,
}

#[derive(Clone)]
pub struct SpectrumFrame { pub bands: Vec<f32> }  // 0..=1, ~20 log bands, 30+ fps

#[derive(Clone)]
pub enum PlayerEvent {
    State(PlaybackState),
    Spectrum(SpectrumFrame),
    Error(String),              // human-readable; shown as toast/banner
    Info(String),
}
```

## Contract: player engine (Builder B)

```rust
pub struct PlayerEngine;        // spawns on tokio runtime inside eframe
impl PlayerEngine {
    /// commands: app -> engine. events: engine -> app (broadcast, app keeps latest).
    pub fn spawn(commands: mpsc::Receiver<PlayerCommand>, events: broadcast::Sender<PlayerEvent>) -> anyhow::Result<Self>;
}
```

- Owns: tokio runtime (or spawned onto one), rodio `OutputStream`+`Sink`, symphonia decoder fed by a ranged-HTTP prefetch buffer, EQ biquad chain, FFT analyser tap (`SpectrumEvent` at ≥30 Hz while playing).
- Decoder accepts a `Box<dyn Read + Send>` per track so tests can feed local files.
- EQ: 10 peaking biquads (60,170,310,600,1k,3k,6k,12k,14k,16k) + preamp, ported curve constants from fastpotify `src/eq.rs`.

## Contract: skin engine (Builder A)

```rust
// src/skin/mod.rs — ported nearly verbatim from fastpotify (MIT)
pub struct Skin { /* sheets, playlist style, viscolors, regions */ }
impl Skin {
    pub fn load(path: &Path) -> Result<Skin, SkinError>;
    pub fn from_archive(name: &str, bytes: &[u8]) -> Result<Skin, SkinError>;
}

// src/ui/winamp/mod.rs — renders main Winamp window; playlist + EQ windows
pub struct WinampState { /* scale, shade mode, window toggles, positions */ }
pub fn winamp_ui(ui: &mut egui::Ui, state: &mut WinampState, skin: &Skin, host: &mut dyn WinampHost, tex: &mut SkinTextures);

pub trait WinampHost {
    fn cmd(&mut self, c: PlayerCommand);
    fn state(&self) -> &PlaybackState;
    fn spectrum(&self) -> SpectrumFrame;
    fn eq(&self) -> EqSettings;                 // enabled, gains, preamp
    fn load_skin_file(&mut self, path: &Path);  // drop target handling
}
```

- Skin textures: builder A owns a `SkinTextures` struct that turns `Skin` bitmaps into egui texture handles (they know the sprite layout).
- Fastpotify's fastpotify source of truth: `~/clawd/projects/ytamp/reference/fastpotify` — port, don't reimplement. Replace its Spotify model references with `WinampHost`.
- MIT headers preserved on ported files: `// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.`

## Contract: YT layer (Builder B)

```rust
// src/yt/mod.rs
pub struct YtClient { /* reqwest + optional cookies */ }
impl YtClient {
    pub fn new(cookies: Option<String>) -> Self;
    pub async fn search_tracks(&self, q: &str, limit: usize) -> anyhow::Result<Vec<Track>>;
    pub async fn radio_for(&self, video_id: &str, limit: usize) -> anyhow::Result<Vec<Track>>; // Watch Next
}

// src/yt/resolver.rs
pub struct StreamUrl { pub url: String, pub itag: u32, pub mime: String, pub duration_secs: Option<u64> }
pub async fn resolve_stream(client: &YtClient, video_id: &str) -> anyhow::Result<StreamUrl>;
```

Resolver strategy (from research):
1. **Client chain:** `android_vr` → `ios` → `tv` (anonymous, no PO token today); `web_music` when cookies present (Premium: itag 141 AAC 256k).
2. **Itag preference:** 141 → 140 → 251 → 250 → 249 → 599 (audio-only; AAC first — symphonia covers it).
3. **Escape hatch:** if all clients fail (403/SABR-only), shell out to `yt-dlp -J --no-playlist <watch_url>`, parse `url` for the same itag preference. Errors are events, never panics.
4. **No native nsig in v0.1** — mobile/tv client URLs don't need it; yt-dlp covers the rest.

## App shell (Builder C)

- `main.rs`: env_logger, clap (`--skin <path>`, `--cookies <path>`, `--log-filter`), eframe launch, dark theme, Inter font default.
- `app.rs`: `YtampApp` implements `WinampHost`. Owns: engine handles (mpsc/broadcast), latest `PlaybackState`/`SpectrumFrame`, skin + `WinampState`, search box + results, queue panel, now-playing bar, settings (skin picker, cookie file, scale), drag-drop of `.wsz` files.
- Mini player: Ctrl+M toggles the Winamp window as a separate eframe viewport; EQ window toggle from the skin (like fastpotify).
- Search flow: text → `YtClient::search_tracks` (spawned) → results list; click/double-click → `QueueReplace(tracks, i)` → often `radio_for` appended after the clicked track.
- Player events drained each frame (`try_recv` loop); SpectrumFrame kept for `vis`.

## Config layout

```
~/.config/ytamp/
  settings.json     # volume, eq, skin choice, scale
  cookies.txt        # optional Netscape-format cookie jar (user-provided)
  skins/             # user skin library
  cache/             # cover-art cache
```

## Non-goals for v0.1 (issues filed instead)

MilkDrop/projectM, MPRIS, tray, playlist editing, library sync, uploads. MilkDrop is architected for (`--features milkdrop` later, mirroring fastpotify's child-process design).

## Verification plan

- `cargo test`: skin parser against `testdata/*.wsz` (three real skins incl. canonical `base-2.91.wsz`), eq curve sanity, vis band mapping, format-selection logic with fixture JSON, cookie header building.
- `cargo clippy -D warnings` + `cargo fmt` clean.
- Live smoke test on Derek's laptop in the morning (server has no audio device): search → queue → play → skin switch.
