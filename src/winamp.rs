// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.

//! Winamp mini-player controller: display state shared by the skinned
//! windows, plus the [`WinampHost`] contract the app shell implements.
//!
//! Drawing lives in [`crate::ui::winamp`]; this module owns what the
//! drawing needs between frames: the marquee's position, slider previews,
//! the visualiser's memory, playlist scrolling and selection, and the
//! egui texture handles the skin's sheets are uploaded to.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::eq::EqSettings;
use crate::model::{PlaybackState, PlayerCommand, SpectrumFrame};
use crate::skin::{Sheet, Skin};
use crate::vis::Analyser;

/// The most screen pixels a skin pixel may take.
pub const MAX_SCALE: u32 = 4;
/// How many characters the marquee shows: thirty whole ones and the edge
/// of a thirty-first.
pub const MARQUEE_CHARS: usize = 31;
/// Text this long fits the marquee without scrolling.
const MARQUEE_FITS: usize = 30;
/// How long the marquee waits between one-character steps.
const MARQUEE_STEP: Duration = Duration::from_millis(220);
/// What separates the end of a scrolling title from its start again.
const MARQUEE_GAP: &str = "  ***  ";

/// The text with the gap it scrolls through, for drawing it as pixels.
pub fn marquee_strip(text: &str) -> String {
    format!("{text}{MARQUEE_GAP}")
}

/// What the skinned windows ask of the app that hosts them.
pub trait WinampHost {
    /// Sends a command to the player engine.
    fn cmd(&mut self, command: PlayerCommand);
    /// The latest playback snapshot.
    fn state(&self) -> &PlaybackState;
    /// The latest analyser frame; an empty one when nothing has arrived.
    fn spectrum(&self) -> SpectrumFrame;
    /// The equalizer as it is set.
    fn eq(&self) -> EqSettings;
    /// A `.wsz` file was dropped on the window.
    fn load_skin_file(&mut self, path: &Path);
    /// Toggles the window under the mouse, when the host keeps them
    /// separate: the equalizer or the playlist.
    fn toggle_eq_window(&mut self) {}
    fn toggle_playlist_window(&mut self) {}
    /// The eject button's meaning is the host's: leave the mini player.
    fn leave_mini_player(&mut self) {}
}

/// Display state for the skinned windows.
pub struct WinampState {
    /// Screen pixels per skin pixel.
    pub scale: u32,
    /// The main window rolled up to its title bar.
    pub shaded: bool,
    /// The equalizer window hangs under the main one.
    pub eq_open: bool,
    pub eq_shaded: bool,
    /// The playlist window hangs under whatever is above it.
    pub playlist_open: bool,
    pub playlist_shaded: bool,
    /// The playlist's height as last stretched, in skin pixels.
    pub playlist_height: u32,
    /// Count down instead of up; clicking the time toggles it.
    pub time_remaining: bool,
    /// The balance while its thumb is held, for the marquee to report.
    pub balance_preview: Option<f32>,
    /// The volume while its thumb is held.
    pub volume_preview: Option<f32>,
    /// The seek position while its thumb is held, as a fraction.
    pub seek_preview: Option<f64>,
    /// Shuffle and repeat, as their lamps show them.
    pub shuffle: bool,
    pub repeat: bool,
    marquee_text: String,
    marquee_offset: usize,
    marquee_moved: Option<Instant>,
    /// The visualiser's memory, fed from the host's spectrum frames.
    pub analyser: Analyser,
    /// What the visualiser shows; a click on the display cycles it.
    pub vis_mode: crate::vis::VisMode,
    /// The playlist window: its first visible row, the wheel's leftover,
    /// and the corner drag's leftover.
    pub playlist_scroll: usize,
    /// The rows selected; Ctrl-click adds and removes, SEL has the rest.
    pub playlist_selection: HashSet<usize>,
    pub playlist_wheel: f32,
    pub playlist_resize: f32,
    /// The playlist's rows drawn as pixels, kept once drawn.
    pub playlist_text: crate::ui::winamp::PixelText,
}

impl Default for WinampState {
    fn default() -> Self {
        Self {
            scale: 2,
            shaded: false,
            eq_open: false,
            eq_shaded: false,
            playlist_open: false,
            playlist_shaded: false,
            playlist_height: crate::skin::layout::PLAYLIST_MIN_HEIGHT,
            time_remaining: false,
            balance_preview: None,
            volume_preview: None,
            seek_preview: None,
            shuffle: false,
            repeat: false,
            marquee_text: String::new(),
            marquee_offset: 0,
            marquee_moved: None,
            analyser: Analyser::default(),
            vis_mode: crate::vis::VisMode::default(),
            playlist_scroll: 0,
            playlist_selection: HashSet::new(),
            playlist_wheel: 0.0,
            playlist_resize: 0.0,
            playlist_text: crate::ui::winamp::PixelText::default(),
        }
    }
}

impl WinampState {
    /// The stack's height in skin pixels: the main window, and the
    /// equalizer and the playlist under it, whichever are open.
    pub fn stack_height(&self) -> u32 {
        use crate::skin::layout;
        let mut height = if self.shaded {
            layout::SHADE_HEIGHT
        } else {
            layout::WINDOW_HEIGHT
        };
        if self.eq_open {
            height += if self.eq_shaded {
                layout::EQ_SHADE_HEIGHT
            } else {
                layout::EQ_HEIGHT
            };
        }
        if self.playlist_open {
            height += if self.playlist_shaded {
                layout::PLAYLIST_SHADE_HEIGHT
            } else {
                self.playlist_height
                    .clamp(layout::PLAYLIST_MIN_HEIGHT, layout::PLAYLIST_MAX_HEIGHT)
            };
        }
        height
    }

    /// The characters the marquee shows now: the text itself when it
    /// fits, otherwise a window onto it that moves a character at a time.
    pub fn marquee(&mut self, text: &str, now: Instant) -> (String, usize) {
        if text != self.marquee_text {
            self.marquee_text = text.to_string();
            self.marquee_offset = 0;
            self.marquee_moved = Some(now);
        }
        let mut chars: Vec<char> = self.marquee_text.chars().collect();
        if chars.len() <= MARQUEE_FITS {
            return (self.marquee_text.clone(), 0);
        }
        chars.extend(MARQUEE_GAP.chars());
        let moved = self.marquee_moved.get_or_insert(now);
        let steps =
            (now.saturating_duration_since(*moved).as_millis() / MARQUEE_STEP.as_millis()) as usize;
        if steps > 0 {
            self.marquee_offset = self.marquee_offset.wrapping_add(steps);
            *moved += MARQUEE_STEP * steps as u32;
        }
        let offset = self.marquee_offset;
        let shown = (0..MARQUEE_CHARS)
            .map(|index| chars[(offset + index) % chars.len()])
            .collect();
        (shown, offset)
    }

    /// Whether the marquee is on the move, and so wants frames.
    pub fn marquee_scrolling(&self) -> bool {
        self.marquee_text.chars().count() > MARQUEE_FITS
    }
}

/// The skin's sheets as egui textures, made on demand and remade whenever
/// the skin under them changes (its id does).
#[derive(Default)]
pub struct SkinTextures {
    skin_id: u64,
    handles: HashMap<Sheet, egui::TextureHandle>,
}

impl SkinTextures {
    /// Every sheet as a texture id, uploading whatever is missing.
    pub fn get(&mut self, ctx: &egui::Context, skin: &Skin) -> HashMap<Sheet, egui::TextureId> {
        if self.skin_id != skin.id {
            self.handles.clear();
            self.skin_id = skin.id;
        }
        for sheet in Sheet::ALL {
            if self.handles.contains_key(&sheet) {
                continue;
            }
            let bitmap = skin.sheet(sheet);
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [bitmap.width as usize, bitmap.height as usize],
                &bitmap.rgba,
            );
            let handle = ctx.load_texture(
                format!("ytamp-skin-{}", sheet.file_stem()),
                image,
                egui::TextureOptions::NEAREST,
            );
            self.handles.insert(sheet, handle);
        }
        self.handles
            .iter()
            .map(|(sheet, handle)| (*sheet, handle.id()))
            .collect()
    }

    /// Drops the textures, for when the window they belong to is gone.
    pub fn clear(&mut self) {
        self.handles.clear();
    }
}

/// A whole-number scale from what the host asks for.
pub fn clamp_scale(scale: u32) -> u32 {
    scale.clamp(1, MAX_SCALE)
}

/// The default skin when a host has loaded none.
pub fn default_skin() -> Arc<Skin> {
    Skin::builtin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_title_sits_still_and_a_long_one_scrolls() {
        let mut state = WinampState::default();
        let start = Instant::now();
        assert_eq!(state.marquee("ytamp", start).0, "ytamp");
        assert!(!state.marquee_scrolling());

        let long = "Radiohead — Everything In Its Right Place (4:11)";
        let (first, offset) = state.marquee(long, start);
        assert_eq!(first.chars().count(), MARQUEE_CHARS);
        assert_eq!(offset, 0);
        assert!(long.starts_with(&first));
        assert!(state.marquee_scrolling());
        // Not yet time to move.
        assert_eq!(
            state.marquee(long, start + Duration::from_millis(100)).0,
            first
        );
        let (later, stepped) = state.marquee(long, start + MARQUEE_STEP);
        assert!(long[1..].starts_with(&later));
        assert_eq!(stepped, 1);
        // Eventually the start comes round again, after the gap.
        let round = long.chars().count() + MARQUEE_GAP.len();
        let again = state.marquee(long, start + MARQUEE_STEP * round as u32).0;
        assert_eq!(again, first);
        // A new title starts from its beginning.
        let other = "Something else entirely, and just as long as before";
        assert!(other.starts_with(&state.marquee(other, start + Duration::from_secs(9)).0));
    }

    #[test]
    fn the_stack_adds_up_whichever_windows_are_open() {
        use crate::skin::layout;
        let mut state = WinampState::default();
        assert_eq!(state.stack_height(), layout::WINDOW_HEIGHT);
        state.shaded = true;
        assert_eq!(state.stack_height(), layout::SHADE_HEIGHT);
        state.shaded = false;
        state.eq_open = true;
        assert_eq!(
            state.stack_height(),
            layout::WINDOW_HEIGHT + layout::EQ_HEIGHT
        );
        state.eq_shaded = true;
        assert_eq!(
            state.stack_height(),
            layout::WINDOW_HEIGHT + layout::EQ_SHADE_HEIGHT
        );
        state.playlist_open = true;
        state.playlist_height = 1000;
        assert_eq!(
            state.stack_height(),
            layout::WINDOW_HEIGHT + layout::EQ_SHADE_HEIGHT + layout::PLAYLIST_MAX_HEIGHT
        );
        state.playlist_shaded = true;
        assert_eq!(
            state.stack_height(),
            layout::WINDOW_HEIGHT + layout::EQ_SHADE_HEIGHT + layout::PLAYLIST_SHADE_HEIGHT
        );
    }

    #[test]
    fn the_scale_stays_a_whole_number_in_range() {
        assert_eq!(clamp_scale(0), 1);
        assert_eq!(clamp_scale(3), 3);
        assert_eq!(clamp_scale(99), MAX_SCALE);
    }
}
