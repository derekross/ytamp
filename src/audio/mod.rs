//! Builder B: audio decode/output engine (symphonia + rodio + EQ + analyser).
//!
//! Pipeline: `YtClient` resolves a stream URL → [`decode::RangedHttpSource`]
//! feeds byte ranges over HTTP → symphonia decodes AAC (or any enabled
//! codec) → [`EqProcessor`] runs the ten-band biquad chain → [`analyser`]
//! taps the post-EQ signal for spectrum frames → [`sink`] writes the device
//! (rodio/cpal when the `audio-alsa` feature is on, a realtime-paced null
//! sink otherwise).
//!
//! # Audio output and the `audio-alsa` feature
//!
//! Device output needs the ALSA development headers on Linux
//! (`libasound2-dev`), which not every build box has. Output is therefore
//! optional:
//!
//! ```text
//! cargo build                       # no device output; paced null sink
//! sudo apt install libasound2-dev pkg-config
//! cargo build --features audio-alsa # real output
//! ```
//!
//! Without the feature the whole engine still runs — decode, EQ, analyser,
//! position, seek — against [`sink::NullSink`], so UI development, tests,
//! and headless boxes lose nothing but sound.

pub mod analyser;
pub mod decode;
pub mod sink;

pub use sink::AudioSpec;

/// Winamp's ten band centre frequencies, in hertz (frozen in DESIGN.md and
/// `PlayerCommand::SetEq`'s doc comment).
pub const EQ_BANDS: [f64; 10] = [
    60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0,
];

/// How far a band goes either way, in decibels (Winamp's slider range).
pub const EQ_RANGE_DB: f64 = 12.0;

/// Equalizer settings snapshot, taken from `PlayerCommand::SetEq`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqSpec {
    pub enabled: bool,
    pub gains_db: [f64; 10],
    pub preamp_db: f64,
}

impl Default for EqSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            gains_db: [0.0; 10],
            preamp_db: 0.0,
        }
    }
}

impl EqSpec {
    fn clamped(mut self) -> Self {
        self.preamp_db = self.preamp_db.clamp(-EQ_RANGE_DB, EQ_RANGE_DB);
        for band in &mut self.gains_db {
            *band = band.clamp(-EQ_RANGE_DB, EQ_RANGE_DB);
        }
        self
    }
}

/// Ten-band peaking EQ over interleaved f32 samples.
///
/// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT
/// license. Differences from the original: the sample rate is a parameter
/// (YouTube serves both 44.1 kHz and 48 kHz AAC) and the settings snapshot
/// arrives from `PlayerCommand::SetEq` instead of a shared mutex.
///
/// The chain is rebuilt only after the settings or the stream format
/// change; a packet in between costs one `PartialEq` compare.
pub struct EqProcessor {
    spec: EqSpec,
    applied: EqSpec,
    rate: u32,
    channels: usize,
    /// One chain per channel; bands at flat are left out of the chain.
    chains: Vec<Vec<Biquad>>,
    /// Preamp as a linear gain; 1.0 when the EQ is off.
    gain: f64,
}

impl EqProcessor {
    pub fn new() -> Self {
        Self {
            spec: EqSpec::default(),
            applied: EqSpec::default(),
            rate: 0,
            channels: 0,
            chains: Vec::new(),
            gain: 1.0,
        }
    }

    /// Queue new settings; applied lazily before the next packet.
    pub fn set_spec(&mut self, spec: EqSpec) {
        self.spec = spec.clamped();
    }

    /// Note the stream format; forces a rebuild when it changes.
    pub fn configure(&mut self, rate: u32, channels: usize) {
        if rate != self.rate || channels != self.channels {
            self.rate = rate;
            self.channels = channels;
            self.applied = EqSpec {
                enabled: !self.spec.enabled,
                ..self.spec
            }; // force rebuild on next packet
        }
    }

    fn rebuild_if_dirty(&mut self) {
        if self.applied == self.spec && !self.chains.is_empty() {
            return;
        }
        self.applied = self.spec;
        self.chains = if self.spec.enabled {
            vec![chain(&self.spec.gains_db, self.rate); self.channels.max(1)]
        } else {
            vec![Vec::new(); self.channels.max(1)]
        };
        self.gain = if self.spec.enabled {
            10f64.powf(self.spec.preamp_db / 20.0)
        } else {
            1.0
        };
    }

    /// Filter one buffer of interleaved samples in place.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        self.rebuild_if_dirty();
        if self.chains.is_empty() || self.chains.iter().all(|c| c.is_empty()) {
            if self.gain != 1.0 {
                for sample in interleaved.iter_mut() {
                    *sample = (*sample as f64 * self.gain) as f32;
                }
            }
            return;
        }
        let channels = self.channels.max(1);
        for samples in interleaved.chunks_exact_mut(channels) {
            for (channel, sample) in samples.iter_mut().enumerate() {
                let mut value = *sample as f64 * self.gain;
                if let Some(chain) = self.chains.get_mut(channel) {
                    for filter in chain {
                        value = filter.run(value);
                    }
                }
                *sample = value as f32;
            }
        }
    }
}

impl Default for EqProcessor {
    fn default() -> Self {
        Self::new()
    }
}

/// Bands closer to flat than this are skipped rather than run for nothing.
const FLAT_DB: f64 = 0.05;
/// How far the solved gains may go past the sliders while making the
/// combined response meet them; a guard, not a target.
const SOLVED_LIMIT: f64 = 36.0;

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
    fn peaking(hz: f64, width: f64, gain_db: f64, rate: u32) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = std::f64::consts::TAU * hz / f64::from(rate);
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
    fn gain_db_at(&self, hz: f64, rate: u32) -> f64 {
        let w = std::f64::consts::TAU * hz / f64::from(rate);
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

/// Band widths in octaves, based on the midpoint between adjacent bands.
///
/// The bands are unevenly spaced. A shared width makes the high bands
/// overlap and overboost presets such as Full Treble. The outer bands
/// extend three octaves below 60 Hz and one octave above 16 kHz, matching
/// Winamp.
fn band_widths() -> [f64; 10] {
    let octaves: Vec<f64> = EQ_BANDS.map(|hz| hz.log2()).to_vec();
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
fn chain(bands_db: &[f64; 10], rate: u32) -> Vec<Biquad> {
    let widths = band_widths();
    let target = *bands_db;
    if target.iter().all(|gain| gain.abs() <= FLAT_DB) {
        return Vec::new();
    }
    // How much each band moves every centre, per decibel it is given.
    let mut unit = [[0.0; 10]; 10];
    for (j, (hz, width)) in EQ_BANDS.iter().zip(widths).enumerate() {
        let filter = Biquad::peaking(*hz, width, 1.0, rate);
        for (i, centre) in EQ_BANDS.iter().enumerate() {
            unit[i][j] = filter.gain_db_at(*centre, rate);
        }
    }
    let mut gains = target;
    for _ in 0..6 {
        let filters: Vec<(usize, Biquad)> = filters_for(&gains, &widths, rate);
        let mut residual = [0.0; 10];
        for (i, centre) in EQ_BANDS.iter().enumerate() {
            let played: f64 = filters
                .iter()
                .map(|(_, filter)| filter.gain_db_at(*centre, rate))
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
    filters_for(&gains, &widths, rate)
        .into_iter()
        .map(|(_, filter)| filter)
        .collect()
}

/// The bands worth running, with the filter for each.
fn filters_for(gains: &[f64; 10], widths: &[f64; 10], rate: u32) -> Vec<(usize, Biquad)> {
    EQ_BANDS
        .iter()
        .zip(widths)
        .zip(gains)
        .enumerate()
        .filter(|(_, (_, gain))| gain.abs() > FLAT_DB)
        .map(|(index, ((hz, width), gain))| (index, Biquad::peaking(*hz, *width, *gain, rate)))
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

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// One second of a sine at `hz`, amplitude 0.25.
    fn sine(hz: f64, rate: u32) -> Vec<f32> {
        (0..rate as usize)
            .map(|i| {
                (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin() as f32 * 0.25
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn flat_eq_is_identity() {
        let mut eq = EqProcessor::new();
        eq.configure(RATE, 2);
        let original = sine(1000.0, RATE);
        let mut samples = original.clone();
        // Enabled but all-zero gains must also be identity.
        eq.set_spec(EqSpec {
            enabled: true,
            gains_db: [0.0; 10],
            preamp_db: 0.0,
        });
        eq.process(&mut samples);
        for (a, b) in original.iter().zip(&samples) {
            assert!((a - b).abs() < 1e-9, "flat EQ changed a sample: {a} -> {b}");
        }
    }

    #[test]
    fn solved_chain_meets_sliders_at_band_centres() {
        let gains = [9.6, 0.0, -5.6, 0.0, 3.2, 0.0, 0.0, 8.0, 0.0, -3.3];
        let filters = chain(&gains, RATE);
        for (i, (hz, wanted)) in EQ_BANDS.iter().zip(gains).enumerate() {
            let played: f64 = filters.iter().map(|f| f.gain_db_at(*hz, RATE)).sum();
            assert!(
                (played - wanted).abs() < 0.05,
                "band {i} ({hz:.0} Hz): solved {played:+.2} dB, slider {wanted:+.2} dB"
            );
        }
    }

    #[test]
    fn one_kilohertz_boost_boosts_one_kilohertz() {
        let mut boost = EqProcessor::new();
        boost.configure(RATE, 1);
        boost.set_spec(EqSpec {
            enabled: true,
            gains_db: [0.0, 0.0, 0.0, 0.0, 12.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            preamp_db: 0.0,
        });
        let mut mid = sine(1000.0, RATE);
        boost.process(&mut mid);

        let mut flat = EqProcessor::new();
        flat.configure(RATE, 1);
        let reference = sine(1000.0, RATE);

        // Let the filters settle, then compare steady-state RMS.
        let settle = RATE as usize / 4;
        let boost_db = 20.0 * (rms(&mid[settle..]) / rms(&reference[settle..])).log10();
        assert!(
            (boost_db - 12.0).abs() < 1.5,
            "1 kHz boost measured {boost_db:+.2} dB, expected ~+12"
        );
    }

    #[test]
    fn one_kilohertz_boost_leaves_bass_alone() {
        let mut boost = EqProcessor::new();
        boost.configure(RATE, 1);
        boost.set_spec(EqSpec {
            enabled: true,
            gains_db: [0.0, 0.0, 0.0, 0.0, 12.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            preamp_db: 0.0,
        });
        let mut bass = sine(60.0, RATE);
        boost.process(&mut bass);

        let mut flat = EqProcessor::new();
        flat.configure(RATE, 1);
        let reference = sine(60.0, RATE);

        let settle = RATE as usize / 4;
        let delta_db = 20.0 * (rms(&bass[settle..]) / rms(&reference[settle..])).log10();
        assert!(
            delta_db.abs() < 1.5,
            "60 Hz moved {delta_db:+.2} dB under a 1 kHz boost, expected ~0"
        );
    }

    #[test]
    fn preamp_scales_and_disable_resets() {
        let mut eq = EqProcessor::new();
        eq.configure(RATE, 1);
        eq.set_spec(EqSpec {
            enabled: true,
            gains_db: [0.0; 10],
            preamp_db: 6.0,
        });
        let mut samples = sine(440.0, RATE);
        eq.process(&mut samples);
        let boosted = rms(&samples[RATE as usize / 4..]);

        let mut flat = EqProcessor::new();
        flat.configure(RATE, 1);
        let reference = sine(440.0, RATE);
        let base = rms(&reference[RATE as usize / 4..]);

        assert!(
            (20.0 * (boosted / base).log10() - 6.0).abs() < 0.1,
            "preamp must be a clean +6 dB"
        );

        eq.set_spec(EqSpec::default());
        let mut again = sine(440.0, RATE);
        eq.process(&mut again);
        for (a, b) in reference.iter().zip(&again) {
            assert!((a - b).abs() < 1e-9, "disabling EQ must restore identity");
        }
    }
}
