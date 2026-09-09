# ytamp

**YouTube Music, native and skinned.** ⭐

A fast, lightweight YouTube Music player written in Rust with [egui](https://github.com/emilk/egui) — no browser engine — featuring a Winamp mini player that renders classic `.wsz` skins, a real spectrum analyser, and Winamp's ten-band equalizer.

```
RAM target: ~100 MB class   ·   startup: under a second   ·   browser engines: zero
```

Inspired by (and indebted to) [fastpotify](https://github.com/crmne/fastpotify), which proved this architecture for Spotify. ytamp brings the same experience to YouTube Music.

## Features

- **Winamp mini player** — drop any classic `.wsz` skin from the [Winamp Skin Museum](https://skins.webamp.org/) onto the window. Rendered natively at 1×–4× scale, shaped windows and all.
- **Spectrum analyser** — real FFT bands driven by the audio pipeline, drawn in your skin's `viscolor.txt` palette.
- **Ten-band equalizer** — Winamp's classic EQ curve and presets, in the main window and the skin.
- **YouTube Music** — search, radio, queues, likes-later. Anonymous out of the box; add cookies for your library and Premium-grade 256 kbps streams.
- **Native everywhere** — one Rust binary, Linux/macOS/Windows, MPRIS on Linux (roadmap).

## Status

🚧 Early alpha — the skin engine and audio pipeline are landing first. See [docs/PLAN.md](docs/PLAN.md).

## Building

Rust 1.95+ (rustup installs the pinned toolchain automatically), plus ALSA headers on Linux:

```bash
# Linux
sudo apt install -y libasound2-dev pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev
cargo run --release --features audio-alsa,login-webview
```

`audio-alsa` is the sound output (without it the player runs silently against a null sink, for machines with no ALSA headers). `login-webview` is the "Sign in with Google" window, which needs WebKitGTK on Linux.

Optional but recommended: `yt-dlp` on your `$PATH` — ytamp resolves streams natively and falls back to yt-dlp when YouTube's enforcement changes. See [BUILD.md](BUILD.md) for full instructions.

## Skins

Thousands of classic skins live at the [Winamp Skin Museum](https://skins.webamp.org/). Any of these puts one on:

- Click **Download** on a museum page: ytamp watches your Downloads folder and wears a new `.wsz` as it lands (Settings can turn this off).
- Paste a museum page link, or any `.wsz` URL, with Ctrl+V over either window, or into the box in Settings.
- Right-click the mini player's title bar (Ctrl+M opens it) to pick from your library, or drop skins in `~/.config/ytamp/skins/`.
- Drag a `.wsz` file onto either window. On Wayland desktops the toolkit delivers no drops; tick "Run under X11" in Settings (or pass `--x11`) to get them through XWayland.

Classic skins only — same as fastpotify.

## Attribution

- The Winamp skin engine (`.wsz` parsing, sprite rendering, pixel text, EQ and visualizer) is adapted from [crmne/fastpotify](https://github.com/crmne/fastpotify), MIT licensed, and so is the built-in skin (`assets/skins/builtin.wsz`, fastpotify's own). Thank you — this project stands on your work.
- Stream resolution follows the patterns of [yt-dlp](https://github.com/yt-dlp/yt-dlp), [youtui](https://github.com/nick42d/youtui), and [meduza-music](https://github.com/akilaisadev/meduza-music).

## Not affiliated with Google

ytamp is an unofficial player. It is not endorsed by or connected to YouTube or Google. Use your own account and credentials; nothing is proxied through anyone else's servers.

## License

MIT — see [LICENSE](LICENSE).
