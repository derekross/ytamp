# YT Music Native Client Landscape — Research Report 01

**Date:** 2026-09-08
**Question:** Does a "fastpotify for YouTube Music" already exist?
**Reference:** [crmne/fastpotify](https://github.com/crmne/fastpotify) — Spotify client in Rust + egui, no browser engine, 100–250 MB RAM, playback via librespot. Signature: Winamp mini player rendering classic `.wsz` skins from the [Winamp Skin Museum](https://skins.webamp.org/) at 1×–4× scale, spectrum analyser, 10-band EQ, MilkDrop via projectM, MPRIS, tray, gapless, library/playlists, Linux/macOS/Windows.

All data verified 2026-09-08 from GitHub API + repo READMEs + source inspection (meduza-music shallow-cloned to /tmp).

---

## TL;DR Verdict

**The gap is real.** No project combines a native (non-webview) Rust GUI with YTM streaming **plus** Winamp `.wsz` skin rendering, spectrum analyser, 10-band EQ, and MilkDrop/projectM — on any platform. The closest architectural match (meduza-music: Rust + egui + MPV, <40 MB RAM, gapless) is Linux-only, anonymous-only, has no EQ/visualizer/skins/MPRIS, ~15 stars, and has been dormant for a month. The closest *feature-library* match (limusic: 268★, full account sync, mini player, extremely active) is a Tauri webview app with no EQ, no visualizer, no Winamp skins. Winamp-skinned YTM playback exists only in browser extensions, web apps, Electron hobby projects, and one TUI. **Zero projects in the entire field ship MilkDrop/projectM. Only one ships a real-FFT visualizer at all (an Electron 0-star project).**

---

## Tier 1 — Serious, active clients

### 1. SimoHypers/limusic ⭐ 268 — the feature king
**https://github.com/SimoHypers/limusic**

| Attribute | Value |
|---|---|
| Stack | Tauri 2 + Rust backend + SvelteKit UI; **libmpv** playback |
| Auth | Full sign-in: in-app Google login or cookie-paste; multi-account with switching |
| YTM library | Playlists (full CRUD incl. cover art), liked songs, saved albums/artists, uploads, history (YTM day-buckets), subscribe |
| Platform | Linux (AppImage/.deb/.rpm/AUR), Windows (setup/msi), macOS Apple Silicon (.dmg) |
| License | GPL-3.0 |
| Created / last push | 2026-07-03 / **2026-09-08 (today)** |
| Release cadence | Blistering: v0.6.4→v0.7.0 in 10 days (8+ releases) |
| Extras | Gapless + loudness normalization, radio/automix, word-by-word synced lyrics (Boidu/LRCLIB/YTM/Netease), music videos, local files, mini player + theater mode, Last.fm, Discord RPC, MPRIS/SMTC, tray, Listen Together, 6 languages, self-updating |

**Verdict:** Most complete YTM desktop client in existence, but it is a **webview app** (Tauri), not native-widget. No EQ, no visualizer, no Winamp anything. Rebuilt from the playback engine of Android's Metrolist.

---

### 2. 2latemc/JustAnotherMusicClient ⭐ 267
**https://github.com/2latemc/JustAnotherMusicClient**

| Attribute | Value |
|---|---|
| Stack | Tauri + React + **TypeScript** (Rust only as shell); custom `AudioEngine.ts` |
| Auth | Account sign-in; session encrypted in OS keychain |
| YTM library | Library, playlists, liked songs, recommendations |
| Platform | Windows (primary), macOS, Linux (AppImage; Wayland EGL workaround needed) |
| License | Apache-2.0 |
| Created / last push | 2026-06-11 / 2026-08-18 |
| Extras | Multi-tab playback (separate queues/volume per tab), **draggable mini player**, synced lyrics, Discord RPC, Last.fm, Nix flake, auto-update, Reddit community |

**Verdict:** Polished, popular, has a mini player — but webview UI, TypeScript-first, no EQ/visualizer/skins.

---

### 3. akilaisadev/meduza-music ⭐ 15 — **the critical lead, verified in source**
**https://github.com/akilaisadev/meduza-music** — *"Fast, native, lightweight YouTube Music desktop player for Linux. Built with Rust, egui, and MPV."*

| Attribute | Value |
|---|---|
| Stack | **Rust + eframe/egui 0.26** (4-crate workspace: core/playback/viewmodel/ui); MPV via IPC socket; stream resolution via **system python3 + yt-dlp**; custom async InnerTube client; tokio |
| Auth | **Anonymous InnerTube only** — no login, no cookie paste; "💚 Library" is local-only saves |
| YTM library | Home feed (Jump back in / Heavy rotation / history shelves), search, categories; **no playlists/likes/history sync** |
| Platform | **Linux only** (AppImage; Flatpak-tagged). Runtime deps: mpv, python3+yt_dlp, gtk3 |
| License | MIT |
| Created / last push | 2026-07-06 / **2026-08-08 (dormant ~1 month)** |
| Release cadence | v1.0.0 (Jul 28), v1.2.0 (Aug 8), then silence |
| Claims | <40 MB RAM, <0.2 s startup, gapless via mpv pre-buffering (verified: `Seamless Gapless Advance` logic in playback_manager.rs), offline track caching, tray via tray-icon crate |

**Source-level findings (cloned):**
- Grep for `equalizer|visualizer|spectrum|winamp|milkdrop|projectm|biquad|fft` → **zero hits**
- Grep for `mpris` → zero hits (only `tray-icon` + GTK StatusNotifier)
- Stream resolver shells out to `python3 -m yt_dlp` with a concurrency limiter ("each python process is heavy")
- mpv launched with `--ytdl=no`; app resolves URLs itself

**Verdict:** Architecturally the closest thing to "fastpotify for YTM" (same GUI framework, similar RAM story, gapless) — but Linux-only, anonymous-only, feature-thin, single dev, stalled. It proves the concept; it is not the product.

---

### 4. FrogSnot/Sunder ⭐ 21 — the EQ outlier
**https://github.com/FrogSnot/Sunder** — *"A desktop YouTube music client that doesn't spy on you"*

| Attribute | Value |
|---|---|
| Stack | Tauri v2 + Svelte 5 UI; **rodio 0.19 + symphonia** (pure-Rust audio → ALSA/Pulse/PipeWire); yt-dlp extraction; SQLite + FTS5 |
| Auth | **No login by design** (privacy-first); local playlists + YTM playlist import by URL paste |
| YTM library | ❌ No account features; dual-source search (YTM + plain YT), local playlists, JSON export |
| Platform | Linux (AUR/.deb/AppImage), Windows, macOS |
| License | AGPL-3.0 |
| Created / last push | 2026-02-16 / 2026-08-29 |
| Extras | **10-band parametric EQ (persisted!)**, playback speed 0.25–3×, offline downloads w/ progress ring + offline-first playback, MPRIS, Windows media keys, tray, focus view, yt-dlp one-click updater (403 recovery), ~15 MB binary, ~40 MB idle |

**Verdict:** The **only** client in the field with a real EQ, and the only Tauri app with pure-Rust audio (no mpv). But: webview UI, no account, no visualizer, no skins. Its README is a candid field guide to YouTube's enforcement churn ("YouTube changes its enforcement every few weeks").

---

### 5. ccgauche/ytermusic ⭐ 697 — the TUI veteran
**https://github.com/ccgauche/ytermusic**

| Attribute | Value |
|---|---|
| Stack | Rust TUI; ~20 MB RAM |
| Auth | Cookie paste (headers.txt) + brand-account id |
| YTM library | Playlists + Supermix |
| Platform | Linux/macOS/Windows |
| License | Apache-2.0 |
| Created / last push | 2022-04-18 / 2026-05-13 |
| Extras | Offline cache, background downloader, search, theming, mouse support |

**Verdict:** Highest star count of the Rust ones, but terminal-only. No GUI, no EQ/visualizer/skins.

---

## Tier 2 — Small, new, or niche (relevant proof points)

### 6. chamchi0809/pocket-ytm ("Pocket Music") ⭐ 1
**https://github.com/chamchi0809/pocket-ytm** — **GPUI** (Zed's GPU framework) + rodio/CPAL audio + Python ytmusicapi bridge (NDJSON) + resident yt-dlp resolver + FFmpeg HTTP-range streaming. Truly zero-DOM. macOS Apple Silicon only. Login via dedicated Chrome cookie harvest or manual fetch-paste; library/likes/lyrics/radio/prefetch. Created 2026-08-18. **Proof that GPUI + rodio native YTM is buildable today.**

### 7. galyarderlabs/GMusic ⭐ 1
**https://github.com/galyarderlabs/GMusic** — Tauri + Rust, ad-free, Discord RPC + Last.fm. Created 2026-09-01, active. Too new to judge.

### 8. Brogolem35/eartube ⭐ 3
**https://github.com/Brogolem35/eartube** — **Iced** (pure Rust GUI) + rodio + yt-dlp. "Stupid simple"; README admits streaming is broken pending rodio/symphonia fixes; favorites caching only, GPL-3.0. Proof-of-concept for Iced+rodio.

### 9. GooseOb/music_plr ⭐ 3
**https://github.com/GooseOb/music_plr** — Iced, streams YT Music + SoundCloud. Small.

### 10. PiBOH/vivi-music ("Vivi Music Desktop Edition") ⭐ 6
**https://github.com/PiBOH/vivi-music** — Kotlin **Compose Multiplatform** (JVM, no webview) port of Android's Vivi Music. Material 3, any desktop OS, "no extra dependencies". Created 2026-08-13, active. Not Rust, not native-compiled, but the only cross-platform *non-webview* full client besides limusic in spirit.

### 11. ref42/CAPS ⭐ 30
**https://github.com/ref42/CAPS** — Dioxus desktop "dynamic island" always-on-top music widget; supports YTM/NetEase/Bilibili. Windows. Niche form factor.

### 12. TUI/CLI remainder
- **WakaTaira/ytmusic-tui** ⭐ 2 — ratatui + mpv, spotify_player-inspired (MIT, 2026-06).
- **lightzgls/mtui** ⭐ 3 — ratatui, YouTube streaming, tiny memory (active today).
- **merthanmerter/youcli**, **tnxnox/YTM-CLI** — terminal/CLI clients.
- **TymanWasTaken/ytmusic-rs** ⭐ 1 — native Rust client (ytmusic-api lib + GUI crate); **dead since Apr 2022**, never released.
- **2gn/tatar** ⭐ 7 — Tauri YTM client, **archived** Feb 2025.
- *harmony:* searched; PotatoTech/harmony (Rust TUI) is gone/unreachable; existing "Harmony-Music" projects are Flutter/Dart (anandnet, ZingyTomato), not Rust TUIs. Nothing relevant.

---

## The Winamp question — who has skins / mini player / visualizer / EQ?

This is the decisive differentiator set. Found via GitHub search "winamp youtube music":

| Project | What | Winamp skins | Visualizer | EQ | Native? |
|---|---|---|---|---|---|
| **gyp430/retro-ytm-bongo-cat** ⭐ 0 | Electron + Python Flask ytmusicapi sidecar + hidden YT IFrame player (Premium applies). Winamp-skinned UI, real-FFT visualizer (AnalyserNode) for local/yt-dlp streams, simulated for embedded audio, beat-reactive bongo cat | ✅ (HTML/CSS skin) | ✅ partial (local audio only) | ❌ | ❌ Electron |
| **dzulfikar08/rustune** ⭐ 33 | Rust **TUI**; YouTube search/stream via yt-dlp + local playback; loads classic Winamp 2.x `.wsz` skins + online gallery browse; MIT; active Sep 7 | ✅ (rendered in terminal) | ❌ | ❌ | ✅ but terminal |
| **SkinAmp** (skinamp.skin) | Webamp (Winamp 2 in browser) streaming YouTube; 100k+ skins from Skin Museum | ✅ | ✅ (Webamp scopes) | ❌ | ❌ web app |
| **internetblacksmith/youtube_winamp** ⭐ 1 | Chrome extension: pixel-perfect CSS Winamp 2.x skin + WSZ loader for YTM/Spotify/Amazon | ✅ | ❌ | ❌ | ❌ extension |
| **moffaty/goamp** ⭐ 0 | Tauri 2 + Webamp + YouTube | ✅ (Webamp in webview) | ✅ (Webamp) | ❌ | ❌ webview |
| **rajofearth/muxics** ⭐ 6 | Electron + React, Winamp-inspired unofficial YouTube client | ~inspired | ❌ | ❌ | ❌ |
| **mcftira/ytm-winamp** ⭐ 0 | Python bridge: YTM tracks/playlists into *actual classic Winamp* via yt-dlp + ffmpeg | ✅ (real Winamp!) | ✅ (Winamp's own) | ✅ (Winamp's own) | ❌ bridge hack |
| **halilkaandogan/kaaninhos-mp3** ⭐ 102 | JS retro Winamp-style player streaming from YouTube + real-time audio effects + mini-game | ~inspired | ~effects | ~effects | ❌ |
| **mikeypdev/wimpyamp** ⭐ 1 | Python retro desktop player, Winamp skins, local hi-res files only | ✅ | ✅ | ❌ | ❌ Python; no YTM |
| **fastpotify** (reference) | Rust + egui + librespot | ✅ .wsz 1×–4× | ✅ real spectrum | ✅ 10-band | ✅ |

**Across the entire native YTM client field (Tier 1+2):**
- Winamp skin support: **none** (rustune is YouTube-generic, terminal-only)
- Real-FFT visualizer: **none** (meduza has a decorative vinyl animation, not an analyser)
- 10-band EQ: **only Sunder** (Tauri webview) — and it works on its rodio pipeline
- MilkDrop/projectM: **zero, anywhere, in any YTM client of any architecture**
- MPRIS: limusic ✅, Sunder ✅, JAMC (SMTC on Windows) ✅, meduza ❌, ytermusic ❌
- Gapless: limusic ✅ (libmpv), meduza ✅ (mpv preload), Sunder ✅ (prefetch), JAMC ~, pocket-ytm ✅ (prefetch)
- Cross-platform native GUI: **nobody** — meduza (Linux-only), eartube (broken streaming), pocket-ytm (macOS-only), pocket/GMusic (webview)

---

## Features matrix (field vs fastpotify)

| | fastpotify (ref) | limusic | JustAnotherMC | meduza | Sunder | ytermusic | rustune | bongo-cat | pocket-ytm |
|---|---|---|---|---|---|---|---|---|---|
| GUI type | egui (native) | Tauri webview | Tauri webview | **egui (native)** | Tauri webview | TUI | TUI | Electron | **GPUI (native)** |
| Rust-first | ✅ | ✅ backend | ❌ TS | ✅ | ✅ backend | ✅ | ✅ | ❌ | ✅ |
| RAM | 100–250 MB | webview-tier | webview-tier | **<40 MB** | ~40 MB | ~20 MB | small | Electron-tier | low |
| Winamp .wsz 1×–4× | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (TUI) | ✅ (CSS) | ❌ |
| Spectrum analyser | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ⚠️ local-only | ❌ |
| 10-band EQ | ✅ | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ |
| MilkDrop/projectM | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| MPRIS | ✅ | ✅ | SMTC | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ |
| Tray | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | n/a | ✅ | ❌ |
| Gapless | ✅ | ✅ | ~ | ✅ | ✅ | ~ | ~ | ❌ | ✅ |
| YTM account library | n/a (Spotify) | ✅ full | ✅ | ❌ anon | ❌ none | ✅ cookies | ❌ | ✅ Premium | ✅ |
| Premium-quality streams | n/a | ad-free anon | ad-free anon | ad-free anon | ad-free anon | cookie-gated | ad-free anon | ✅ via IFrame | cookies |
| Platforms | L/M/W | L/W/M | W/M/L | **Linux only** | L/W/M | L/M/W | L/M | ? | **macOS-AS only** |
| License | (see repo) | GPL-3.0 | Apache-2.0 | MIT | AGPL-3.0 | Apache-2.0 | MIT | — | MIT |
| Stars / activity | — | 268 / daily | 267 / weeks | 15 / stalled | 21 / weekly | 697 / monthly | 33 / daily | 0 | 1 |

*(Electron baseline for context: th-ch/youtube-music — the 14k★ incumbent — has itself been renamed/moved to pear-devs/pear-desktop. Still Electron. Not part of the native field.)*

---

## Verdict & opportunity

### Does the gap exist? **Yes — on three axes simultaneously:**

1. **Architecture gap:** No cross-platform, native-GUI (egui/Iced/GPUI/rodio) YTM client exists. Each attempt covers exactly one OS (meduza→Linux, pocket-ytm→macOS, eartube→broken). Every popular client (limusic, JAMC, Sunder, GMusic) is a webview app.
2. **Retro-experience gap:** No native client has Winamp skins, a real spectrum analyser, an EQ (except webview Sunder), or MilkDrop/projectM. The Winamp-skin + YTM combination exists only in browsers, extensions, Electron, and one TUI. **Nobody renders .wsz skins in a native GPU window, and nobody in the YTM world has projectM.**
3. **Trust/weight gap:** The biggest players bundle a webview (100–200 MB-class RAM); the truly-light ones are TUIs or Linux-only.

### What a new project ("ytamp") would uniquely offer
- **The fastpotify experience on YTM:** egui (or GPUI) native GUI, rodio or mpv output, ~100 MB RAM class, MPRIS, tray, gapless — *cross-platform*, which literally nobody has done natively.
- **Winamp .wsz mini player at 1×–4× pulling from the Skin Museum** — first native implementation ever for YTM.
- **Real spectrum analyser + 10-band EQ + MilkDrop via projectM** — first in the entire YTM ecosystem (any architecture except the 0-star Electron bongo-cat's partial FFT).
- Differentiation is durable: fastpotify's skin-rendering code (Rust, .wsz parsing, projectM integration) is directly reusable; the YTM-specific hard part is only the stream-resolution + InnerTube layer.

### Risks / hard parts (evidenced by the field)
- **No librespot equivalent for YTM.** Everyone resolves streams via yt-dlp (Python) or InnerTube hacks; meduza and Sunder both shell out to system yt-dlp; Sunder ships a one-click updater because "YouTube changes enforcement every few weeks" (403s). A Rust-native resolver (ytextract-class crates) is perpetually breaking — plan for a yt-dlp sidecar or an updatable resolver module.
- **Premium audio quality requires cookies/login**; anonymous streams are ad-free but quality-limited and occasionally throttled (pocket-ytm README documents anonymous-playback limits).
- **ToS exposure:** every project in this field carries the same disclaimer; bongo-cat routes through the IFrame player specifically so Premium applies; Google tolerates but does not bless this ecosystem.
- **Maintenance treadmill:** InnerTube API shape changes; meduza's one-month stall and ytmusic-rs's 2022 death show the churn rate. limusic's near-daily releases show the required maintenance tempo.

### Bottom line
A native, cross-platform, Winamp-skinnable, visualizer-equipped "fastpotify for YouTube Music" **does not exist and would be first of its kind**. The closest neighbors to learn from: **meduza-music** (egui+MPV architecture, gapless, RAM story), **limusic** (InnerTube + account features + product polish), **Sunder** (EQ + rodio audio path + yt-dlp resilience), **rustune** (.wsz parsing in Rust), and **fastpotify** itself (the skin/visualizer/EQ stack to port).

---

## Appendix — full repo index

| Repo | Stars | Lang | Status |
|---|---|---|---|
| https://github.com/SimoHypers/limusic | 268 | Rust+Svelte | active daily |
| https://github.com/2latemc/JustAnotherMusicClient | 267 | TypeScript | active |
| https://github.com/ccgauche/ytermusic | 697 | Rust | slow-burn active |
| https://github.com/akilaisadev/meduza-music | 15 | Rust | stalled Aug 8 |
| https://github.com/FrogSnot/Sunder | 21 | Rust+Svelte | active |
| https://github.com/dzulfikar08/rustune | 33 | Rust | active |
| https://github.com/ref42/CAPS | 30 | Rust/Dioxus | active |
| https://github.com/PiBOH/vivi-music | 6 | Kotlin/CMP | active, new |
| https://github.com/halilkaandogan/kaaninhos-mp3 | 102 | JavaScript | — |
| https://github.com/mikeypdev/wimpyamp | 1 | Python | active-ish, no YTM |
| https://github.com/gyp430/retro-ytm-bongo-cat | 0 | JS+Python | hobby |
| https://github.com/rajofearth/muxics | 6 | TS/Electron | small |
| https://github.com/internetblacksmith/youtube_winamp | 1 | JS | extension |
| https://github.com/moffaty/goamp | 0 | TS/Tauri | tiny |
| https://github.com/mcftira/ytm-winamp | 0 | Python | bridge |
| https://github.com/chamchi0809/pocket-ytm | 1 | Rust/GPUI | new, macOS-AS |
| https://github.com/galyarderlabs/GMusic | 1 | Rust/Tauri | new |
| https://github.com/Brogolem35/eartube | 3 | Rust/Iced | experimental |
| https://github.com/GooseOb/music_plr | 3 | Rust/Iced | small |
| https://github.com/WakaTaira/ytmusic-tui | 2 | Rust/ratatui | small |
| https://github.com/lightzgls/mtui | 3 | Rust/ratatui | active |
| https://github.com/TymanWasTaken/ytmusic-rs | 1 | Rust | dead 2022 |
| https://github.com/2gn/tatar | 7 | Rust/Tauri | archived |
| https://github.com/11philip22/ytmusicapi-rs | — | Rust lib | API lib (building block) |
| https://skinamp.skin | — | web | Winamp+YouTube browser app |
