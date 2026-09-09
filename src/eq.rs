// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.

//! Winamp's ten-band equalizer, ported from fastpotify.
//!
//! Each Winamp band uses a second-order peaking filter. Bands and preamp
//! range from -12 to +12 dB. The builder that owns the audio pipeline calls
//! [`EqProcessor::set`] when the UI moves a slider and [`EqProcessor::process`]
//! on every buffer of interleaved stereo `f32` samples, in place:
//!
//! ```text
//! let mut eq = EqProcessor::new(48_000);
//! eq.set(true, [0.0; 10], 0.0);          // on, flat
//! ...per buffer:
//! eq.process(&mut interleaved_samples);  // in place, stereo pairs
//! ```
//!
//! This stage does not clip boosted samples; the output stage owns limiting.

/// The centre frequencies, Winamp's, in hertz.
pub const BANDS: [f64; 10] = [
    60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0,
];
/// How far a band goes either way, in decibels.
pub const RANGE_DB: f64 = 12.0;
/// Bands closer to flat than this are skipped rather than run for nothing.
const FLAT: f64 = 0.05;
/// How far the solved gains may go past the sliders while making the
/// combined response meet them; a guard, not a target.
const SOLVED_LIMIT: f64 = 36.0;

/// Equalizer settings: the switch, the ten band gains, and the preamp.
///
/// This is the shape shared across the UI, [`crate::winamp::WinampHost`],
/// and the player's audio pipeline; gains are decibels within ±[`RANGE_DB`].
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EqSettings {
    pub enabled: bool,
    /// Twelve decibels either way, like the preamp.
    pub gains_db: [f64; 10],
    pub preamp_db: f64,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            gains_db: [0.0; 10],
            preamp_db: 0.0,
        }
    }
}

impl EqSettings {
    /// The same settings kept within what the equalizer can do.
    pub fn clamped(mut self) -> Self {
        self.preamp_db = self.preamp_db.clamp(-RANGE_DB, RANGE_DB);
        for band in &mut self.gains_db {
            *band = band.clamp(-RANGE_DB, RANGE_DB);
        }
        self
    }

    /// Filter response used for playback and the curve display.
    pub fn curve(&self, sample_rate: u32) -> Curve {
        Curve {
            sample_rate,
            preamp_db: self.preamp_db,
            filters: chain(sample_rate, &self.gains_db),
        }
    }

    /// The response at one frequency, in decibels, with the switch on.
    pub fn response_db(&self, sample_rate: u32, hz: f64) -> f64 {
        self.curve(sample_rate).db_at(hz)
    }
}

/// Equalizer response by frequency.
pub struct Curve {
    sample_rate: u32,
    preamp_db: f64,
    filters: Vec<Biquad>,
}

impl Curve {
    pub fn db_at(&self, hz: f64) -> f64 {
        let mut db = self.preamp_db;
        for filter in &self.filters {
            db += filter.gain_db_at(self.sample_rate, hz);
        }
        db
    }
}

/// Band widths in octaves, based on the midpoint between adjacent bands.
///
/// The bands are unevenly spaced. A shared width makes the high bands overlap
/// and overboost presets such as Full Treble. The outer bands extend three
/// octaves below 60 Hz and one octave above 16 kHz, matching Winamp.
fn band_widths() -> [f64; 10] {
    let octaves: Vec<f64> = BANDS.iter().map(|hz| hz.log2()).collect();
    let mut widths = [0.0; 10];
    for (index, width) in widths.iter_mut().enumerate() {
        let below = index
            .checked_sub(1)
            .map_or(octaves[index] - 3.0, |i| octaves[i]);
        let above = octaves
            .get(index + 1)
            .copied()
            .unwrap_or(octaves[index] + 1.0);
        *width = (octaves[index] - below) / 2.0 + (above - octaves[index]) / 2.0;
    }
    widths
}

/// Builds filters whose combined response matches each slider at its centre.
/// Adjacent filters overlap, so their individual gains are solved together.
fn chain(sample_rate: u32, bands_db: &[f64; 10]) -> Vec<Biquad> {
    let widths = band_widths();
    let target = *bands_db;
    if target.iter().all(|gain| gain.abs() <= FLAT) {
        return Vec::new();
    }
    // How much each band moves every centre, per decibel it is given.
    let mut unit = [[0.0; 10]; 10];
    for (j, (hz, width)) in BANDS.iter().zip(widths).enumerate() {
        let filter = Biquad::peaking(sample_rate, *hz, width, 1.0);
        for (i, centre) in BANDS.iter().enumerate() {
            unit[i][j] = filter.gain_db_at(sample_rate, *centre);
        }
    }
    let mut gains = target;
    for _ in 0..6 {
        let filters: Vec<(usize, Biquad)> = filters_for(sample_rate, &gains, &widths);
        let mut residual = [0.0; 10];
        for (i, centre) in BANDS.iter().enumerate() {
            let played: f64 = filters
                .iter()
                .map(|(_, filter)| filter.gain_db_at(sample_rate, *centre))
                .sum();
            residual[i] = played - target[i];
        }
        if residual.iter().all(|r| r.abs() < 0.01) {
            break;
        }
        let correction = solve(unit, residual);
        for (gain, step) in gains.iter_mut().zip(correction) {
            *gain = (*gain - step).clamp(-SOLVED_LIMIT, SOLVED_LIMIT);
        }
    }
    filters_for(sample_rate, &gains, &widths)
        .into_iter()
        .map(|(_, filter)| filter)
        .collect()
}

/// The bands worth running, with the filter for each.
fn filters_for(sample_rate: u32, gains: &[f64; 10], widths: &[f64; 10]) -> Vec<(usize, Biquad)> {
    BANDS
        .iter()
        .zip(widths)
        .zip(gains)
        .enumerate()
        .filter(|(_, (_, gain))| gain.abs() > FLAT)
        .map(|(index, ((hz, width), gain))| {
            (index, Biquad::peaking(sample_rate, *hz, *width, *gain))
        })
        .collect()
}

/// Gaussian elimination with partial pivoting, for the ten-by-ten
/// interaction of the bands.
fn solve(mut matrix: [[f64; 10]; 10], mut rhs: [f64; 10]) -> [f64; 10] {
    let n = 10;
    for column in 0..n {
        let pivot = (column..n)
            .max_by(|a, b| {
                matrix[*a][column]
                    .abs()
                    .total_cmp(&matrix[*b][column].abs())
            })
            .unwrap_or(column);
        matrix.swap(column, pivot);
        rhs.swap(column, pivot);
        let lead = matrix[column][column];
        if lead.abs() < 1e-12 {
            continue;
        }
        let pivot_row = matrix[column];
        let pivot_rhs = rhs[column];
        for row in column + 1..n {
            let factor = matrix[row][column] / lead;
            if factor == 0.0 {
                continue;
            }
            for (cell, above) in matrix[row][column..].iter_mut().zip(&pivot_row[column..]) {
                *cell -= factor * above;
            }
            rhs[row] -= factor * pivot_rhs;
        }
    }
    let mut solution = [0.0; 10];
    for row in (0..n).rev() {
        let mut sum = rhs[row];
        for k in row + 1..n {
            sum -= matrix[row][k] * solution[k];
        }
        let lead = matrix[row][row];
        solution[row] = if lead.abs() < 1e-12 { 0.0 } else { sum / lead };
    }
    solution
}

/// A named set of band gains, as Winamp shipped them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub gains_db: [f64; 10],
}

/// How many of `PRESETS` are Winamp's own, in its order; what follows
/// are scenario presets of fastpotify's, shown behind a separator.
pub const WINAMP_PRESET_COUNT: usize = 18;

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Flat",
        gains_db: [0.0; 10],
    },
    Preset {
        name: "Classical",
        gains_db: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -7.2, -7.2, -7.2, -9.6],
    },
    Preset {
        name: "Club",
        gains_db: [0.0, 0.0, 8.0, 5.6, 5.6, 5.6, 3.2, 0.0, 0.0, 0.0],
    },
    Preset {
        name: "Dance",
        gains_db: [9.6, 7.2, 2.4, 0.0, 0.0, -5.6, -7.2, -7.2, 0.0, 0.0],
    },
    Preset {
        name: "Full Bass",
        gains_db: [-8.0, 9.6, 9.6, 5.6, 1.6, -4.0, -8.0, -10.4, -11.2, -11.2],
    },
    Preset {
        name: "Full Bass & Treble",
        gains_db: [7.2, 5.6, 0.0, -7.2, -4.8, 1.6, 8.0, 11.2, 12.0, 12.0],
    },
    Preset {
        name: "Full Treble",
        gains_db: [-9.6, -9.6, -9.6, -4.0, 2.4, 11.2, 12.0, 12.0, 12.0, 12.0],
    },
    Preset {
        name: "Laptop Speakers / Headphones",
        gains_db: [4.8, 11.2, 5.6, -3.2, -2.4, 1.6, 4.8, 9.6, 12.0, 12.0],
    },
    Preset {
        name: "Large Hall",
        gains_db: [10.4, 10.4, 5.6, 5.6, 0.0, -4.8, -4.8, -4.8, 0.0, 0.0],
    },
    Preset {
        name: "Live",
        gains_db: [-4.8, 0.0, 4.0, 5.6, 5.6, 5.6, 4.0, 2.4, 2.4, 2.4],
    },
    Preset {
        name: "Party",
        gains_db: [7.2, 7.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.2, 7.2],
    },
    Preset {
        name: "Pop",
        gains_db: [-1.6, 4.8, 7.2, 8.0, 5.6, 0.0, -2.4, -2.4, -1.6, -1.6],
    },
    Preset {
        name: "Reggae",
        gains_db: [0.0, 0.0, 0.0, -5.6, 0.0, 6.4, 6.4, 0.0, 0.0, 0.0],
    },
    Preset {
        name: "Rock",
        gains_db: [8.0, 4.8, -5.6, -8.0, -3.2, 4.0, 8.8, 11.2, 11.2, 11.2],
    },
    Preset {
        name: "Ska",
        gains_db: [-2.4, -4.8, -4.0, 0.0, 4.0, 5.6, 8.8, 9.6, 11.2, 9.6],
    },
    Preset {
        name: "Soft",
        gains_db: [4.8, 1.6, 0.0, -2.4, 0.0, 4.0, 8.0, 9.6, 11.2, 12.0],
    },
    Preset {
        name: "Soft Rock",
        gains_db: [4.0, 4.0, 2.4, 0.0, -4.0, -5.6, -3.2, 0.0, 2.4, 8.8],
    },
    Preset {
        name: "Techno",
        gains_db: [8.0, 5.6, 0.0, -5.6, -4.8, 0.0, 8.0, 9.6, 9.6, 8.8],
    },
    Preset {
        name: "Bass Booster",
        gains_db: [8.8, 7.2, 5.6, 3.2, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0],
    },
    Preset {
        name: "Bass Reducer",
        gains_db: [-8.8, -7.2, -5.6, -3.2, -0.8, 0.0, 0.0, 0.0, 0.0, 0.0],
    },
    Preset {
        name: "Treble Booster",
        gains_db: [0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 3.2, 5.6, 7.2, 8.8],
    },
    Preset {
        name: "Vocal Booster",
        gains_db: [-2.4, -4.8, -4.8, 1.6, 5.6, 5.6, 4.0, 1.6, 0.0, -2.4],
    },
    Preset {
        name: "Small Speakers",
        gains_db: [-8.0, -6.4, -4.0, -1.6, 1.6, 3.2, 4.8, 5.6, 5.6, 5.6],
    },
    Preset {
        name: "Spoken Word",
        gains_db: [-3.2, -0.8, 0.0, 0.8, 4.0, 5.6, 4.8, 2.4, 0.8, 0.0],
    },
    Preset {
        name: "Loudness",
        gains_db: [9.6, 6.4, 0.0, 0.0, -2.4, 0.0, -1.6, 0.0, 8.0, 1.6],
    },
    Preset {
        name: "Night Listening",
        gains_db: [-4.8, -3.2, -1.6, 0.8, 2.4, 3.2, 2.4, 0.8, -1.6, -3.2],
    },
];

/// A second-order section in direct form I, one per band and channel.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    /// A peaking filter after the Audio EQ Cookbook, `width` octaves wide
    /// between its half-gain points and normalised by `a0`. The width is
    /// taken the cookbook's digital way, with its `w0 / sin(w0)` term: near
    /// the top of the band the plain analog Q would have come out three
    /// times too narrow, which is what rippled the treble.
    fn peaking(sample_rate: u32, hz: f64, width: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = std::f64::consts::TAU * hz / f64::from(sample_rate);
        let (sin, cos) = w0.sin_cos();
        let alpha = sin * ((std::f64::consts::LN_2 / 2.0) * width * w0 / sin).sinh();
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha / a) / a0,
            ..Self::default()
        }
    }

    /// The filter's gain at a frequency, in decibels, from its transfer
    /// function on the unit circle: what is played, including the bend
    /// the bilinear transform puts near the top of the band.
    fn gain_db_at(&self, sample_rate: u32, hz: f64) -> f64 {
        let w = std::f64::consts::TAU * hz / f64::from(sample_rate);
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();
        let numerator = (self.b0 + self.b1 * cos1 + self.b2 * cos2).powi(2)
            + (self.b1 * sin1 + self.b2 * sin2).powi(2);
        let denominator = (1.0 + self.a1 * cos1 + self.a2 * cos2).powi(2)
            + (self.a1 * sin1 + self.a2 * sin2).powi(2);
        10.0 * (numerator / denominator).log10()
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// The equalizer over the audio pipeline: settings handed in from the UI,
/// filters rebuilt only after they change.
///
/// The audio pipeline's contract: make one, call [`EqProcessor::set`] when
/// the settings change (cheap), and call [`EqProcessor::process`] on every
/// buffer of interleaved stereo `f32` samples, in place.
pub struct EqProcessor {
    sample_rate: u32,
    applied: EqSettings,
    /// One chain per channel; a band at flat is left out of the chain.
    chains: [Vec<Biquad>; 2],
    gain: f64,
}

impl EqProcessor {
    pub fn new(sample_rate: u32) -> Self {
        let mut processor = Self {
            sample_rate,
            applied: EqSettings::default(),
            chains: [Vec::new(), Vec::new()],
            gain: 1.0,
        };
        processor.rebuild();
        processor
    }

    /// The sample rate the filters are shaped for.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Takes the settings the UI offers; gains beyond the range are held
    /// to it. Filters are rebuilt lazily, on the next buffer.
    pub fn set(&mut self, enabled: bool, gains_db: [f64; 10], preamp_db: f64) {
        let wanted = EqSettings {
            enabled,
            gains_db,
            preamp_db,
        }
        .clamped();
        if wanted != self.applied {
            self.applied = wanted;
            self.rebuild();
        }
    }

    /// The settings currently applied.
    pub fn settings(&self) -> EqSettings {
        self.applied
    }

    fn rebuild(&mut self) {
        self.gain = 10f64.powf(self.applied.preamp_db / 20.0);
        if self.applied.enabled {
            self.chains = [
                chain(self.sample_rate, &self.applied.gains_db),
                chain(self.sample_rate, &self.applied.gains_db),
            ];
        } else {
            self.chains = [Vec::new(), Vec::new()];
        }
    }

    /// Runs interleaved stereo samples through the equalizer, in place.
    /// With the switch off nothing happens, not even the preamp, which is
    /// how Winamp's own switch behaved.
    pub fn process(&mut self, samples: &mut [f32]) {
        if !self.applied.enabled {
            return;
        }
        let flat = self.chains[0].is_empty() && self.chains[1].is_empty();
        if flat && self.gain == 1.0 {
            return;
        }
        // No ceiling here. These are floats with room to spare; whatever
        // limits the output belongs to the output stage, past the volume.
        for frame in samples.chunks_exact_mut(2) {
            for (sample, chain) in frame.iter_mut().zip(self.chains.iter_mut()) {
                let mut y = f64::from(*sample) * self.gain;
                for filter in chain.iter_mut() {
                    y = filter.run(y);
                }
                *sample = y as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn tone(hz: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / f64::from(RATE);
                let sample = 0.25 * (std::f64::consts::TAU * hz * t).sin();
                [sample as f32, sample as f32]
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt()
    }

    #[test]
    fn a_boosted_band_makes_its_tone_louder_and_leaves_others_alone() {
        let mut processor = EqProcessor::new(RATE);
        let mut gains = [0.0; 10];
        gains[4] = 12.0; // 1 kHz
        processor.set(true, gains, 0.0);
        let mut at_band = tone(1000.0, 8192);
        let before = rms(&at_band[4096..]);
        processor.process(&mut at_band);
        let after = rms(&at_band[4096..]);
        let gain_db = 20.0 * (after / before).log10();
        assert!((gain_db - 12.0).abs() < 1.0, "1 kHz gained {gain_db:.1} dB");

        let mut far = tone(60.0, 8192);
        let before = rms(&far[4096..]);
        processor.process(&mut far);
        let after = rms(&far[4096..]);
        let gain_db = 20.0 * (after / before).log10();
        assert!(gain_db.abs() < 1.0, "60 Hz moved {gain_db:.1} dB");
    }

    #[test]
    fn off_or_flat_changes_nothing_and_the_preamp_scales() {
        let mut processor = EqProcessor::new(RATE);
        let original = tone(440.0, 1024);
        let mut samples = original.clone();
        processor.process(&mut samples);
        assert_eq!(samples, original);

        processor.set(true, [0.0; 10], 0.0);
        let mut samples = original.clone();
        processor.process(&mut samples);
        assert_eq!(samples, original);

        processor.set(true, [0.0; 10], 6.0);
        let mut samples = original.clone();
        processor.process(&mut samples);
        let ratio = rms(&samples) / rms(&original);
        assert!((20.0 * ratio.log10() - 6.0).abs() < 0.1);

        processor.set(true, [0.0; 10], -6.0);
        let mut samples = original.clone();
        processor.process(&mut samples);
        let ratio = rms(&samples) / rms(&original);
        assert!((20.0 * ratio.log10() + 6.0).abs() < 0.1);

        // The switch off means no preamp either, as Winamp behaved.
        processor.set(false, [12.0; 10], 12.0);
        let mut samples = original.clone();
        processor.process(&mut samples);
        assert_eq!(samples, original);
    }

    #[test]
    fn an_odd_number_of_samples_does_not_panic() {
        let mut processor = EqProcessor::new(RATE);
        processor.set(true, eq_preset("Rock"), 0.0);
        let mut samples = vec![0.5f32; 101];
        processor.process(&mut samples);
        assert_eq!(samples.len(), 101);
    }

    fn eq_preset(name: &str) -> [f64; 10] {
        PRESETS.iter().find(|p| p.name == name).unwrap().gains_db
    }

    #[test]
    fn the_drawn_response_peaks_at_the_band_and_adds_the_preamp() {
        let mut settings = EqSettings {
            enabled: true,
            ..EqSettings::default()
        };
        settings.gains_db[7] = 6.0; // 12 kHz
        settings.preamp_db = -3.0;
        assert!((settings.response_db(RATE, 12_000.0) - 3.0).abs() < 0.1);
        assert!((settings.response_db(RATE, 100.0) + 3.0).abs() < 0.2);
        let clamped = EqSettings {
            preamp_db: 20.0,
            gains_db: [20.0; 10],
            ..settings
        }
        .clamped();
        assert_eq!(clamped.preamp_db, RANGE_DB);
        assert!(clamped.gains_db.iter().all(|band| *band == RANGE_DB));
    }

    /// Every preset, played, meets its sliders at every band's centre,
    /// which the top three bands, a fifth of an octave apart, did not
    /// before fastpotify solved the chain together.
    #[test]
    fn what_is_played_meets_the_sliders_at_every_band() {
        for preset in PRESETS {
            let settings = EqSettings {
                enabled: true,
                gains_db: preset.gains_db,
                ..EqSettings::default()
            };
            let curve = settings.curve(RATE);
            for (hz, wanted) in BANDS.iter().zip(preset.gains_db) {
                let got = curve.db_at(*hz);
                assert!(
                    (got - wanted).abs() < 0.3,
                    "{} at {hz} Hz plays {got:.1} dB for {wanted:.1}",
                    preset.name
                );
            }
        }
    }

    /// One slider moves its own band and leaves the neighbours' centres
    /// alone, even at the top where they sit close together.
    #[test]
    fn a_slider_leaves_its_neighbours_centres_alone() {
        let mut settings = EqSettings {
            enabled: true,
            ..EqSettings::default()
        };
        settings.gains_db[8] = 12.0; // 14 kHz
        let curve = settings.curve(RATE);
        assert!((curve.db_at(14_000.0) - 12.0).abs() < 0.3);
        assert!(
            curve.db_at(12_000.0).abs() < 0.3,
            "12 kHz moved {:.1}",
            curve.db_at(12_000.0)
        );
        assert!(
            curve.db_at(16_000.0).abs() < 0.3,
            "16 kHz moved {:.1}",
            curve.db_at(16_000.0)
        );
        assert!(curve.db_at(1_000.0).abs() < 0.1);
        // Between two boosted neighbours the response does not sag away.
        settings.gains_db[7] = 12.0; // 12 kHz too
        let curve = settings.curve(RATE);
        assert!(
            curve.db_at(13_000.0) > 9.0,
            "13 kHz sags to {:.1}",
            curve.db_at(13_000.0)
        );
    }

    /// The outer sliders reach past their centres: the sub-bass follows
    /// the 60 Hz slider and the air follows the 16 kHz one, as they did.
    #[test]
    fn the_outer_sliders_reach_the_ends() {
        let mut settings = EqSettings {
            enabled: true,
            ..EqSettings::default()
        };
        settings.gains_db[0] = 12.0;
        let curve = settings.curve(RATE);
        assert!(
            curve.db_at(30.0) > 6.0,
            "30 Hz gets {:.1}",
            curve.db_at(30.0)
        );
        assert!(curve.db_at(170.0).abs() < 0.3);
        let mut settings = EqSettings {
            enabled: true,
            ..EqSettings::default()
        };
        settings.gains_db[9] = 12.0;
        let curve = settings.curve(RATE);
        // A peaking filter is back at 0 dB by the sample rate's top, so
        // half the slider is what 19 kHz can hold.
        assert!(
            curve.db_at(19_000.0) > 5.0,
            "19 kHz gets {:.1}",
            curve.db_at(19_000.0)
        );
        assert!(curve.db_at(14_000.0).abs() < 0.3);
    }

    #[test]
    fn every_preset_stays_within_the_range() {
        assert_eq!(PRESETS[0].name, "Flat");
        assert_eq!(PRESETS[WINAMP_PRESET_COUNT - 1].name, "Techno");
        for preset in PRESETS {
            assert!(
                preset.gains_db.iter().all(|band| band.abs() <= RANGE_DB),
                "{} leaves the range",
                preset.name
            );
        }
    }

    #[test]
    fn the_filters_follow_the_sample_rate_they_are_given() {
        assert_eq!(EqProcessor::new(48_000).sample_rate(), 48_000);
        let mut settings = EqSettings {
            enabled: true,
            ..EqSettings::default()
        };
        settings.gains_db[4] = 12.0;
        for rate in [44_100u32, 48_000, 96_000] {
            let curve = settings.curve(rate);
            assert!(
                (curve.db_at(1000.0) - 12.0).abs() < 0.3,
                "at {rate} Hz the 1 kHz slider plays {:.1}",
                curve.db_at(1000.0)
            );
        }
    }
}
