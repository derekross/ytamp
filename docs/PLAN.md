# ytamp — Plan

**Phase 0 — Research** ✅ 2026-09-08
- [x] Landscape: gap confirmed (docs/01-landscape.md)
- [x] API feasibility: layered strategy locked (docs/02-api-feasibility.md)
- [x] UI/skins architecture: fastpotify source studied; port, don't reinvent

**Phase 1 — Foundation** ✅ 2026-09-08 night
- [x] Repo + GitHub (CentauriAgent/ytamp, MIT)
- [x] DESIGN.md contracts frozen; model.rs skeleton
- [x] Test skins staged (base-2.91, TopazAmp, XMMS-Turquoise)

**Phase 2 — Parallel build (overnight)**
- [ ] Builder A: skin engine + winamp UI + EQ + vis (port from fastpotify)
- [ ] Builder B: YT layer + resolver + audio engine
- [ ] Builder C: app shell + main UI + wiring
- [ ] Integration: contracts reconciled, `cargo build` green

**Phase 3 — Quality gates**
- [ ] `cargo test` (skin fixtures, format selection, eq/vis units)
- [ ] `cargo clippy -D warnings`, `cargo fmt`
- [ ] CI green on GitHub

**Phase 4 — Ship**
- [ ] README/ATTRIBUTION final, LICENSE
- [ ] BUILD.md for Derek's laptop (Ubuntu 24, rustup, libasound2-dev, optional yt-dlp + cookie export)
- [ ] Issues filed for v0.2 scope (MPRIS, tray, MilkDrop, library sync)
- [ ] Push, morning delivery to Signal with instructions

## Decisions log

| # | Decision | Why |
|---|---|---|
| D1 | Port fastpotify skin engine instead of reimplementing | MIT licensed, battle-tested, 6.9k lines of subtle Winamp behaviors |
| D2 | egui/eframe 0.36 glow (same as fastpotify) | Proven for skin rendering; light binary; no webview |
| D3 | Own thin InnerTube client, not ytmapi-rs dependency | Full control over player/resolver; metadata endpoints stable; ytmapi-rs re-evaluable for v0.2 library sync |
| D4 | yt-dlp subprocess as sanctioned fallback | Externalizes the anti-bot arms race (youtui/Sunder/meduza pattern) |
| D5 | Cookies-first auth, anonymous search default | Research: durable, Premium unlocks itag 141 + no PO tokens |
| D6 | rodio+symphonia, AAC-first itags | Pure Rust, no system codec deps; symphonia aac+isomp4 covers 140/141 |
| D7 | Single crate (like fastpotify) | Simpler integration tonight; workspace split later if needed |
| D8 | beads CLI unavailable (not on crates.io) | File-based plan + GitHub issues instead; noted to Derek |
