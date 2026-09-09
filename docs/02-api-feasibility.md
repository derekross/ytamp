# YT Music API Feasibility for a Pure-Rust Desktop Client ("fastpotify for YouTube Music")

**Date:** 2026-09-08
**Author:** Research agent (Centauri)
**Question:** Can a native Rust + egui app with no browser engine access YT Music data (search, library, playlists) and stream audio reliably in 2026 — and what does keeping it alive cost?

**Reference point:** [fastpotify](https://github.com/crmne/fastpotify) uses librespot (a clean-room Spotify protocol reimplementation). **There is no librespot for YouTube.** Everything below flows from that gap.

---

## 1. AUDIO STREAMING

### 1.1 How every FOSS client works: the InnerTube API

All surviving clients (yt-dlp, NewPipe, FreeTube, youtui, Invidious) talk to **InnerTube** (`youtubei`) — YouTube's private first-party API. Clients POST JSON to `https://www.youtube.com/youtubei/v1/<endpoint>` (or `music.youtube.com/youtubei/v1/...` for YT Music) with a client context block (`context.client.clientName`, `clientVersion`, etc.) that impersonates an official app. Keys are static public constants extracted from official players. Metadata endpoints (`search`, `browse`, `next`) are stable and have **no PO token requirement**. The trouble is confined to the **`player` endpoint** (format URLs) and **googlevideo.com delivery**.

- Verified: ytmapi-rs implements ~40 YT Music metadata endpoints on InnerTube and is actively maintained ([repo](https://github.com/nick42d/youtui), [crates.io](https://crates.io/crates/ytmapi-rs)).
- Verified: FreeTube's "Local API" is exactly this pattern (YouTube.js/InnerTube, with an Invidious proxy fallback) ([docs](https://docs.freetubeapp.io/usage/local-api/), [DeepWiki](https://deepwiki.com/FreeTubeApp/FreeTube/4.1-local-api-implementation)).

### 1.2 The four walls between you and the audio bytes (current state, Sept 2026)

1. **Client identity + PO Tokens (Proof of Origin).** A BotGuard/DroidGuard/iOSGuard attestation token bound to your session (Visitor ID) **and now to each video ID**. Required for GVS (googlevideo streaming) requests on most web-family clients; without it you get 403s or ~480p caps. **Not required for YouTube Premium subscribers** (verified: [yt-dlp PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide)).
2. **nsig / "n" parameter.** The player JS ships a transform function for the `n` URL parameter; without solving it, googlevideo throttles you. The player JS now **rotates structure ~2–3×/month** (verified: [VidPickr engineering post, May 2026](https://vidpickr.com/blog/youtube-anti-bot-evolution-2026); yt-dlp release notes show constant nsig/jsinterp churn).
3. **SABR (Server Adaptive Bit Rate).** YouTube's session-based replacement for plain HTTPS format URLs. Rolled out to the **web client starting Feb 2025**: `adaptiveFormats` no longer contains direct URLs, only a `serverAbrStreamingUrl`; segments arrive framed in **UMP** (protobuf parts: MEDIA_HEADER, MEDIA, MEDIA_END). FFmpeg cannot consume it; yt-dlp's native SABR downloader (PR #13515) was **still under test as of mid-2026** (verified: [issue #12482](https://github.com/yt-dlp/yt-dlp/issues/12482), [ffmpeg.download analysis](https://www.ffmpeg.download/blog/bypass-youtube-sabr-ffmpeg/)).
4. **IP locks.** Signed URLs carry `ip` + `expire` (~6 h); for licensed music (most Vevo/official content) the IP check is **strict** — the IP that minted the URL must fetch the bytes. Datacenter IPs get bot-walled almost immediately; residential IPs are fine (verified: [VidPickr](https://vidpickr.com/blog/youtube-anti-bot-evolution-2026)).

**Did SABR/UMID kill old extractors?** Partially yes — but selectively, per client. The web/tv clients increasingly return SABR-only, while **`tv`, `ios`, `android`, `android_vr` still return plain HTTPS format URLs in many cases**, which is why yt-dlp biases its default client chain there (verified: [#12482](https://github.com/yt-dlp/yt-dlp/issues/12482), [#13968](https://github.com/yt-dlp/yt-dlp/issues/13968) verbose logs, [ffmpeg.download](https://www.ffmpeg.download/blog/bypass-youtube-sabr-ffmpeg/)). Old extractors are dead not because SABR is unbreakable — PipePipe (NewPipe fork) **implemented the full SABR protocol in Java (~47 classes: UMP framing, session driver, attestation recovery)** — but because their maintainers stopped before the 2024–25 PO-token/SABR wave (verified: [PipePipe SABR docs](https://priveetee.github.io/Docs-PipePipe/extractor/sabr.html)).

### 1.3 PO token / client cheat-sheet (yt-dlp wiki, verified 2026-09-08)

| Client | PO token needed for | Notes |
|---|---|---|
| `web` | Subs, GVS | **SABR-only formats** |
| `web_safari` | GVS* | HLS formats exempt (*currently) |
| `mweb` | GVS | **yt-dlp's recommended** client with PO-token plugin |
| `tv` | none | DRM'd if no cookies; SABR-only sometimes |
| `tv_simply` | GVS | no account cookies |
| `web_embedded` | none | embeddable videos only |
| `web_music` | GVS | the YT Music client |
| `android` | GVS or Player | no account cookies |
| `android_vr` | **none** | no made-for-kids content |
| `ios` | GVS or Player | no account cookies |

Plus: **Premium subscribers skip GVS PO tokens entirely**; HLS live streams skip them (except ios). PO tokens are now **per-video** (new token each song) with ~12 h+ lifespans — manual extraction is dead; plugin providers are the way ([PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide)).

### 1.4 Rust crate landscape (verified via crates.io API, 2026-09-08)

| Crate | Last release | Status |
|---|---|---|
| `rustube` | 2022-10-16 | **dead** |
| `youtubei-rs` | 2022-07-05 | **dead** |
| `ytextract` | 2023-01-29 | **dead** |
| `rusty_ytdl` | 2024-08-10 | stale; pre-SABR, pre-per-video-PO-token |
| **`ytmapi-rs`** (nick42d/youtui) | **2026-08-24** (v0.3.3; cadence ~2–3 months) | **active — the only living pure-Rust YT Music stack** |
| **`bgutil-ytdlp-pot-provider`** (jim60105) | **2026-03-12** (v0.8.1) | active — **Rust** BotGuard PO-token generator (HTTP server or yt-dlp plugin) |

**youtui** ([repo](https://github.com/nick42d/youtui)) is direct prior art: a pure-Rust TUI YT Music player (Rodio/cpal + symphonia playback), cookie.txt *or* OAuth TV device auth, native InnerTube downloader with optional `po_token.txt`, and **yt-dlp shell-out as configurable fallback**. It proves the full stack works in Rust today — with the caveat that it leans on the fallback when YouTube-side changes bite.

### 1.5 What the audio path should look like for us

Player request (InnerTube `player`, YT Music `web_music` or `android_vr` client) → pick `itag 140/141/251` (audio-only) → solve `n` param (run the actual player JS in an embedded interpreter — e.g. a JS crate — robust to rotation, the approach yt-dlp's `--js-runtimes` uses) → stream HTTPS range requests from googlevideo → decode AAC/Opus with symphonia → rodio. If SABR-only responses appear: fall back to another client; last resort: SABR in Rust (protobuf schema exists — PipePipe/protobug prove it) or yt-dlp subprocess.

---

## 2. AUTH

- **Anonymous:** works for search/metadata; guest rate limit ~300 videos/hour (~1000 player requests/hour) ([yt-dlp Extractors wiki](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)).
- **Cookies:** the universal method. Export from an incognito-only YouTube session (so cookies never rotate), pass to InnerTube. Account rate limit ~2000 videos/hour. **Real ban risk** for heavy use — yt-dlp warns explicitly ([wiki](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)).
- **OAuth / TV device code:** YouTube **killed yt-dlp's OAuth device flow** ("logging in with OAuth no longer works with yt-dlp" — [Extractors wiki](https://github.com/yt-dlp/yt-dlp/wiki/Extractors)). **However**, youtui ships its own OAuth TV-device flow (user creates a Google Cloud OAuth client of type "TVs and Limited Input devices", `youtui setup-oauth`), documented as a working alternative to cookies and actively maintained as of Aug 2026 ([youtui README](https://github.com/nick42d/youtui)). So a smart-TV-style device pairing is still viable — you own the GCP client — but it's one policy change away from death. **Cookies are the durable path.**
- **Premium quality:** itag **141 = AAC 256 kb/s** (m4a), itag **774 = Opus 256 kb/s**; both require cookies + active Premium (verified: [gytmdl docs](https://pypi.org/project/gytmdl/); Reddit reports through Apr 2026 confirm 141 downloads working). Critically, **Premium subscribers don't need GVS PO tokens** ([PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide)) — so *Premium + cookies + audio-only* is the cleanest lane in the whole system: 256 kbps AAC with no BotGuard dance. Historical flakiness exists (#7972, #5502 — 141 intermittently missing), so treat it as best-effort. Free tier: itag 140 (AAC 128k) / 251 (Opus ~130k).
- **What Premium unlocks via InnerTube:** full library (songs/albums/artists/playlists), likes (partially — youtui's GetLikedSongs is unimplemented), uploads (ytmapi-rs has the full upload suite), history read/write, ratings, radio/watch playlists. Anonymous gets search + public playlists.

---

## 3. MAINTENANCE BURDEN

**How often does it break?**
- Player JS rotates **~2–3×/month**; each rotation can break regex-based nsig extraction ([VidPickr](https://vidpickr.com/blog/youtube-anti-bot-evolution-2026)). Running the real JS in an interpreter absorbs most of this.
- yt-dlp stable releases 2026.02.21 → 2026.06.09 (~quarterly) but ships YouTube fixes constantly; **nightly channel is the de-facto requirement** for YouTube reliability ([releases](https://github.com/yt-dlp/yt-dlp/releases), [ffmpeg.download](https://www.ffmpeg.download/blog/bypass-youtube-sabr-ffmpeg/)).
- PO-token enforcement moved three times in 2025 alone (#12482 Feb → #13968 Aug → #14744 Oct); SABR downloader still experimental mid-2026 (PR #13515).
- NewPipe breaks publicly and gets fixed by extractor bumps (Reddit reports Jan 2026; NewPipeExtractor v0.26.3 fixed playlist extraction) — same treadmill, slower cadence.

**The graveyard tells the real story:** four Rust YouTube extractor crates (rustube, youtubei-rs, ytextract, rusty_ytdl) all died within ~1–2 years of their last release — each casualty of exactly this treadmill. The two survivors split cleanly: **ytmapi-rs** lives because YT Music *metadata* endpoints are stable; **bgutil-ytdlp-pot-provider** lives because it has an active maintainer riding the BotGuard changes.

**Realistic commitment for our own audio extractor:** watch yt-dlp/NewPipeExtractor commits weekly (they're the canary flock), update client versions/UA ranges when yt-dlp bumps them (they auto-bump Chrome version ranges ~monthly), re-test the client fallback chain monthly, expect a scramble 2–4×/year when YouTube ships something structural (SABR expansions, new token bindings). Estimate **4–10 h/week sustained**, spiking to full-time days during incidents. Metadata layer: ~1–2 h/month.

**Mitigation that changes the math:** residential-IP desktop app + Premium cookies + `web_music`/`android_vr` audio-only + yt-dlp fallback = you're never solely responsible for the bleeding edge.

---

## 4. LEGAL / ToS POSTURE

- **ToS:** InnerTube use violates YouTube's ToS (unauthorized client). This is a contract/civil matter, not criminal circumvention — unless a court buys a DMCA §1201 "technical protection measure" theory.
- **The decisive precedent:** RIAA's Oct 23, 2020 DMCA takedown of youtube-dl was **reversed within 3 weeks** (Nov 16, 2020). EFF's counter-notice argued youtube-dl uses YouTube's own signature code "in the same manner as any browser" — not circumvention. GitHub reinstated, stood up a $1M developer defense fund, and now reviews §1201 claims with lawyers before takedowns (verified: [EFF](https://www.eff.org/deeplinks/2020/11/github-reinstates-youtube-dl-after-riaas-abuse-dmca), [TechCrunch](https://techcrunch.com/2020/11/16/github-defies-riaa-takedown-notice-restoring-youtube-dl-and-starting-1m-defense-fund/), [Wikipedia](https://en.wikipedia.org/wiki/Youtube-dl)). yt-dlp has never been taken down.
- **Project postures:** yt-dlp (download tool, unbothered since 2020), NewPipe (Android client, F-Droid, ~10 years), FreeTube (desktop client, Electron, "not endorsed by Google" disclaimer), Invidious (server proxy — this is the one that suffers, from **IP blocking**, not lawsuits). youtui carries a simple "not supported or endorsed by Google" line.
- **What keeps them alive:** (1) they distribute *code, not content*; (2) users bring their own credentials; (3) interoperability/reverse-engineering is broadly protected (the youtube-dl precedent, plus decades of precedent from DeCSS-era litigation onward); (4) YouTube's actual enforcement is technical (IP blocks, token churn) and account-level (bans), not legal.
- **Our risk shape:** a *streaming* player (transient playback, no retention) is materially softer than a downloader. Risks to note: account bans for cookie users (real, documented); theoretical civil suit (negligible given precedent); GCP OAuth client shutdown if we abuse the TV flow. Sensible posture: open-source the extraction code (mirror the yt-dlp model), no bundled keys/content beyond public InnerTube constants, disclaim affiliation, make download an opt-in afterthought, and never proxy user streams through our servers.

---

## FEASIBILITY VERDICT

**Feasible — but only as a layered hybrid. There is no librespot-grade "implement once, maintain never" path; the audio layer is a subscription to an arms race.**

### Recommended architecture

```
┌─────────────────────────── egui app ───────────────────────────┐
│ DATA LAYER   ytmapi-rs (dependency, or thin fork)              │  LOW risk
│              InnerTube music.youtube.com: search, library,     │  (metadata stable
│              playlists, uploads, history — cookie/OAuth TV     │   for years)
│
│ AUTH         cookie.txt (incognito export) primary;            │
│              OAuth TV device pairing (own GCP client) optional;│
│              anonymous fallback for search-only                │
│
│ AUDIO LAYER  own InnerTube player client with fallback chain:  │  MEDIUM-HIGH risk
│              web_music → android_vr → tv → ios                 │  (see mitigations)
│              itag 141 (Premium AAC 256k) → 140 → 251           │
│              nsig: execute real player JS in embedded JS       │
│              runtime (rotation-proof)                          │
│              PO token: bgutil Rust provider as opt-in          │
│              ESCAPE HATCH: shell out to yt-dlp binary          │  LOW risk
│              (youtui's proven pattern — transfers the          │  (maintenance
│              bleeding edge to the yt-dlp team)                 │   externalized)
│
│ PLAYBACK     rodio + cpal + symphonia (AAC/Opus decode)        │  LOW risk
└────────────────────────────────────────────────────────────────┘
```

### Risk ratings

| Component | Risk | Why |
|---|---|---|
| Metadata/data layer | **LOW** | InnerTube music endpoints stable for years; ytmapi-rs actively maintained; worst case: JSON parsing fixes |
| Audio via own InnerTube client | **MEDIUM-HIGH** | Client rotations ~monthly, per-video PO tokens, SABR creep — but Premium cookies skip GVS PO tokens, and residential desktop IPs dodge the bot wall |
| yt-dlp subprocess fallback | **LOW** | Battle-tested by youtui; nightly channel tracks YouTube for us |
| Premium 256 kbps AAC | **MEDIUM** | Works today (cookies + Premium, no PO token needed); historical flakiness means degrade-gracefully to 128k |
| OAuth TV pairing | **MEDIUM** | Works for youtui today; yt-dlp's variant was killed — treat as convenience, not foundation |
| Legal/ToS | **LOW-MEDIUM** | Strong precedent (RIAA retreat 2020), all peers alive; real exposure is account bans, not court. Stream-not-store default posture |

### Bottom line

Build the data layer on **ytmapi-rs**, own a **thin player client** with a client fallback chain and embedded-JS nsig solving, keep **yt-dlp as the sanctioned escape hatch**, and design auth around **cookies first**. Budget sustained maintenance (~4–10 h/week, spikes during YouTube incidents) or accept that the audio layer will rot like rustube did. The Premium path (256 kbps AAC, no PO token) is the killer feature that makes this worth doing for subscribers specifically.

---

## Source index (all verified 2026-09-08/09 unless noted)

- yt-dlp PO Token Guide — https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide
- yt-dlp Extractors wiki (YouTube section) — https://github.com/yt-dlp/yt-dlp/wiki/Extractors
- yt-dlp issue #12482 (SABR rollout, web client) — https://github.com/yt-dlp/yt-dlp/issues/12482
- yt-dlp issue #13968 (SABR forced despite cookies) — https://github.com/yt-dlp/yt-dlp/issues/13968
- yt-dlp issue #13260 (nsig failure + SABR) — https://github.com/yt-dlp/yt-dlp/issues/13260
- yt-dlp releases — https://github.com/yt-dlp/yt-dlp/releases
- SABR engineering analysis — https://www.ffmpeg.download/blog/bypass-youtube-sabr-ffmpeg/
- PipePipe SABR implementation docs — https://priveetee.github.io/Docs-PipePipe/extractor/sabr.html
- VidPickr anti-bot deep-dive (May 2026) — https://vidpickr.com/blog/youtube-anti-bot-evolution-2026
- youtui / ytmapi-rs — https://github.com/nick42d/youtui + crates.io API (versions, dates)
- bgutil-ytdlp-pot-provider (Rust) — https://github.com/jim60105/bgutil-ytdlp-pot-provider-rs + crates.io
- rustube / youtubei-rs / ytextract / rusty_ytdl — crates.io API (last-update dates)
- FreeTube Local API docs — https://docs.freetubeapp.io/usage/local-api/
- EFF on youtube-dl reinstatement — https://www.eff.org/deeplinks/2020/11/github-reinstates-youtube-dl-after-riaas-abuse-dmca
- TechCrunch ($1M defense fund) — https://techcrunch.com/2020/11/16/github-defies-riaa-takedown-notice-restoring-youtube-dl-and-starting-1m-defense-fund/
- youtube-dl Wikipedia (takedown history) — https://en.wikipedia.org/wiki/Youtube-dl
- gytmdl (Premium itags 141/774) — https://pypi.org/project/gytmdl/
- yt-dlp itag 141 issues — https://github.com/yt-dlp/yt-dlp/issues/7972, https://github.com/yt-dlp/yt-dlp/issues/5502

**Speculation flags:** (a) OAuth TV flow longevity — inferred from youtui's active maintenance, not independently tested; (b) SABR-in-Rust effort estimate — extrapolated from PipePipe's Java scope, not attempted; (c) maintenance hours — judgment call from incident cadence, not measured.
