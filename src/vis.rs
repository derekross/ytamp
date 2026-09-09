// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.

//! Winamp-style spectrum analyser data, ported from fastpotify.
//!
//! The FFT itself lives in Builder B's audio pipeline, which publishes
//! [`SpectrumFrame`](crate::model::SpectrumFrame): normalized 0..=1
//! magnitudes, log-spaced, ~20 bands. This module takes those bands and
//! turns them into the classic analyser's nineteen bars with Winamp's own
//! falloff and peak behaviour (from `classic_vis.cpp`, cross-checked
//! against Webamp's `VisPainter.ts`): bars jump to the sound and fall at a
//! fixed rate on a fixed clock, peaks hang above and pick up speed as they
//! drop.

use std::time::{Duration, Instant};

/// The analyser's height in skin pixels (the scope's too).
pub const ROWS: u8 = 16;
/// The bars, each three columns wide with one between.
pub const BARS: usize = 19;
/// The tallest a bar gets.
const MAX_HEIGHT: f32 = 15.0;
/// How far a bar falls each step, and how peaks pick up speed.
const FALLOFF: f32 = 12.0 / 16.0;
const PEAK_FALLOFF: f32 = 1.1;
/// How often the bars move. Winamp drew its analyser sixty times a
/// second on a timer of its own, so a fast or slow frame rate never
/// changed how quickly the bars fell; a frame that comes sooner than
/// this shows the bars where they were.
pub const STEP: Duration = Duration::from_micros(16_667);

/// What the visualiser shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VisMode {
    /// The spectrum analyser's bars.
    #[default]
    Bars,
    /// Nothing at all.
    Off,
}

/// One bar of the analyser, in rows from the bottom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bar {
    /// How tall the bar is, 0 to 15.
    pub height: u8,
    /// Where the peak mark sits, as the height of a bar whose top row it
    /// would be, 1 to 16; `None` while it is out of sight.
    pub peak: Option<u8>,
}

/// The spectrum analyser's memory: where each bar and its peak are.
#[derive(Debug)]
pub struct Analyser {
    /// Where each bar is, falling at a fixed rate towards the sound.
    falloff: [f32; BARS],
    /// Each peak, in 256ths of a row, and how fast it is dropping.
    peaks: [i32; BARS],
    peak_speed: [f32; BARS],
    /// When the bars last moved, and where they are.
    last_step: Option<Instant>,
    bars: [Bar; BARS],
}

impl Default for Analyser {
    fn default() -> Self {
        Self {
            falloff: [0.0; BARS],
            peaks: [0; BARS],
            peak_speed: [0.0; BARS],
            last_step: None,
            bars: [Bar::default(); BARS],
        }
    }
}

impl Analyser {
    /// One frame: `bands` are the analyser's normalized output (0..=1,
    /// log-spaced, any count; an empty or short frame reads as silence),
    /// `now` paces the steps. Returns the nineteen bars.
    pub fn step(&mut self, bands: &[f32], now: Instant) -> [Bar; BARS] {
        // Keep the step's own beat when frames come a little early or late,
        // and never owe more than one step after a long gap.
        let due = self.last_step.map_or(now, |last| last + STEP);
        if now + Duration::from_millis(1) < due {
            return self.bars;
        }
        self.last_step = Some(due.max(now - STEP));
        let columns = map_bands(bands);
        let mut bars = [Bar::default(); BARS];
        for (bar, slot) in bars.iter_mut().enumerate() {
            let sound = columns[bar];
            // Winamp kept the target as a whole number of rows.
            let target = (sound.min(1.0) * MAX_HEIGHT).trunc();
            let falloff = &mut self.falloff[bar];
            *falloff -= FALLOFF;
            if *falloff <= target {
                *falloff = target;
            }
            let peak = &mut self.peaks[bar];
            if *peak <= (*falloff * 256.0).round() as i32 {
                *peak = (*falloff * 256.0) as i32;
                self.peak_speed[bar] = 3.0;
            }
            let peak_row = *peak / 256;
            *peak -= self.peak_speed[bar].round() as i32;
            self.peak_speed[bar] *= PEAK_FALLOFF;
            if *peak <= 0 {
                *peak = 0;
            }
            slot.height = falloff.round() as u8;
            slot.peak = (peak_row >= 1).then_some((peak_row + 1) as u8);
        }
        self.bars = bars;
        bars
    }

    /// Whether every bar and peak has come to rest, so nothing moves until
    /// there is sound again.
    pub fn settled(&self) -> bool {
        self.falloff.iter().all(|f| *f <= 0.0) && self.peaks.iter().all(|p| *p == 0)
    }

    pub fn reset(&mut self) {
        self.falloff = [0.0; BARS];
        self.peaks = [0; BARS];
        self.peak_speed = [0.0; BARS];
        self.last_step = None;
        self.bars = [Bar::default(); BARS];
    }
}

/// Maps the analyser's bands onto the nineteen bars. Winamp summed its own
/// semitone-spaced columns four at a time; the input here is already
/// log-spaced, so the bars read it evenly: one band per bar when the counts
/// match, and a straight interpolation between the two nearest bands when
/// they do not.
fn map_bands(bands: &[f32]) -> [f32; BARS] {
    let mut out = [0.0; BARS];
    let bands: Vec<f32> = bands.iter().map(|b| b.clamp(0.0, 1.0)).collect();
    match bands.len() {
        0 => out,
        1 => {
            out.fill(bands[0]);
            out
        }
        count => {
            let span = (count - 1) as f32 / (BARS - 1) as f32;
            for (index, slot) in out.iter_mut().enumerate() {
                let at = index as f32 * span;
                let low = at.floor() as usize;
                let high = (low + 1).min(count - 1);
                let fraction = at - low as f32;
                *slot = bands[low] * (1.0 - fraction) + bands[high] * fraction;
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames one step apart, the way a 60 Hz loop delivers them.
    fn clock() -> impl FnMut() -> Instant {
        let mut at = Instant::now();
        move || {
            at += STEP;
            at
        }
    }

    #[test]
    fn the_bands_map_onto_the_bars() {
        // The count the analyser ships: one band per bar.
        let identity: Vec<f32> = (0..BARS).map(|i| i as f32 / BARS as f32).collect();
        assert_eq!(map_bands(&identity), identity[..].try_into().unwrap());
        // Twenty bands interpolate: the first and last bars keep their ends.
        let mut twenty = vec![0.0f32; 20];
        twenty[19] = 1.0;
        let bars = map_bands(&twenty);
        assert_eq!(bars[0], 0.0);
        assert!((bars[BARS - 1] - 1.0).abs() < 1e-6);
        // The bar just past the middle sits just past the middle band.
        assert!(bars[BARS / 2] > 0.4 && bars[BARS / 2] < 0.6);
        // Whatever wanders in is clamped, padded, and counted.
        let wild = map_bands(&[2.0, -1.0]);
        assert_eq!(wild[0], 1.0);
        assert_eq!(wild[1], 0.0);
        assert_eq!(map_bands(&[]), [0.0; BARS]);
        assert_eq!(map_bands(&[0.5]), [0.5; BARS]);
    }

    #[test]
    fn a_fast_frame_rate_leaves_the_bars_alone() {
        let mut analyser = Analyser::default();
        let loud: Vec<f32> = vec![0.9; BARS];
        let silence: Vec<f32> = vec![0.0; BARS];
        let start = Instant::now();
        let bars = analyser.step(&loud, start);
        // Frames a millisecond apart do not move the bars: they are shown
        // where they were, however often the window paints.
        for i in 1..12 {
            let again = analyser.step(&silence, start + Duration::from_millis(i));
            assert_eq!(
                again.iter().map(|b| b.height).collect::<Vec<_>>(),
                bars.iter().map(|b| b.height).collect::<Vec<_>>()
            );
        }
        let moved = analyser.step(&silence, start + STEP);
        assert!(
            moved
                .iter()
                .zip(bars.iter())
                .any(|(after, before)| after.height < before.height)
        );
    }

    #[test]
    fn bars_rise_with_sound_and_fall_without() {
        let mut analyser = Analyser::default();
        let mut tick = clock();
        let loud: Vec<f32> = (0..BARS)
            .map(|i| 0.3 + 0.6 * ((i % 7) as f32 / 7.0))
            .collect();
        let bars = analyser.step(&loud, tick());
        let tallest = bars.iter().map(|bar| bar.height).max().unwrap();
        assert!(tallest > 0, "no bar rose to the sound");
        assert!(tallest <= 15);
        assert!(!analyser.settled());

        let silence: Vec<f32> = vec![0.0; BARS];
        let after = analyser.step(&silence, tick());
        let lower = bars
            .iter()
            .zip(after.iter())
            .all(|(before, after)| after.height <= before.height);
        assert!(lower, "bars rose in silence");
        // The peak hangs above the bar it came from.
        let with_peak = after.iter().find(|bar| bar.peak.is_some()).unwrap();
        assert!(with_peak.peak.unwrap() > with_peak.height);
        for _ in 0..400 {
            analyser.step(&silence, tick());
        }
        assert!(analyser.settled());
        assert!(
            analyser
                .step(&silence, tick())
                .iter()
                .all(|bar| bar.height == 0 && bar.peak.is_none())
        );
    }

    /// A reused analyser that just saw a full frame must read a short or
    /// empty frame as silence, not as whatever came before.
    #[test]
    fn a_short_or_empty_input_reads_as_silence() {
        let mut analyser = Analyser::default();
        let loud: Vec<f32> = vec![0.9; BARS];
        let mut tick = clock();
        analyser.step(&loud, tick());
        let short = analyser.step(&[0.9, 0.9], tick());
        assert!(short.iter().all(|bar| bar.height > 0));
        analyser.reset();
        assert!(analyser.settled());
        let empty = analyser.step(&[], tick());
        assert!(empty.iter().all(|bar| bar.height == 0 && bar.peak.is_none()));
    }
}
