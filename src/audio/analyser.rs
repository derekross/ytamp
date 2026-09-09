//! Spectrum analyser tap: post-EQ mono signal in, `SpectrumFrame`s out.
//!
//! ~20 log-spaced bands (Winamp-ish), FFT size 2048 with a Hann window.
//! Bands rise instantly and fall about one screen-height per second, the
//! classic analyser look. Frame cadence is controlled by the caller (the
//! decode loop asks for a frame every ~33 ms, meeting DESIGN.md's 30 Hz).

use rustfft::{Fft, FftPlanner, num_complex::Complex};

use crate::model::SpectrumFrame;

/// FFT size in samples. 2048 at 44.1 kHz resolves down to ~21 Hz per bin —
/// plenty for a 60 Hz lowest band.
pub const FFT_SIZE: usize = 2048;
/// Bands in each emitted frame (DESIGN.md: "~20 log bands").
pub const BAND_COUNT: usize = 20;
/// Lowest and highest band edges, in hertz.
const LOW_HZ: f64 = 50.0;
const HIGH_HZ: f64 = 16_000.0;
/// dB range mapped onto 0..=1 (a full-scale sine sits near the top).
const DYNAMIC_RANGE_DB: f64 = 54.0;
/// Fall speed, in normalized units per second (~1 screen per second).
const FALL_PER_SEC: f32 = 1.1;

/// Rolling FFT analyser over recent mono samples.
pub struct Analyser {
    fft: std::sync::Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// Monotonic sample history; `pos` is the next write index (ring).
    ring: Vec<f32>,
    pos: usize,
    filled: usize,
    /// Band centre frequencies for the current sample rate.
    band_edges: Vec<usize>,
    smoothed: Vec<f32>,
    rate: u32,
}

impl Analyser {
    /// A silent analyser until [`Analyser::set_rate`] configures the bands.
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        Self {
            fft,
            window: hann_window(FFT_SIZE),
            ring: vec![0.0; FFT_SIZE],
            pos: 0,
            filled: 0,
            band_edges: Vec::new(),
            smoothed: vec![0.0; BAND_COUNT],
            rate: 0,
        }
    }

    /// (Re)compute band edges for the stream's sample rate.
    pub fn set_rate(&mut self, rate: u32) {
        if rate == self.rate || rate == 0 {
            return;
        }
        self.rate = rate;
        let nyquist = f64::from(rate) / 2.0;
        let top = HIGH_HZ.min(nyquist * 0.95).max(LOW_HZ * 4.0);
        let bin_hz = f64::from(rate) / FFT_SIZE as f64;
        self.band_edges = (0..=BAND_COUNT)
            .map(|i| {
                let hz = LOW_HZ * (top / LOW_HZ).powf(i as f64 / BAND_COUNT as f64);
                ((hz / bin_hz).round() as usize).clamp(1, FFT_SIZE / 2)
            })
            .collect();
        // Edges must be strictly increasing to give every band width.
        for i in 1..self.band_edges.len() {
            if self.band_edges[i] <= self.band_edges[i - 1] {
                self.band_edges[i] = self.band_edges[i - 1] + 1;
            }
        }
    }

    /// Mix interleaved samples down to mono and append to the history ring.
    pub fn push(&mut self, interleaved: &[f32], channels: usize) {
        let channels = channels.max(1);
        for frame in interleaved.chunks_exact(channels) {
            let mono: f32 = frame.iter().sum::<f32>() / channels as f32;
            self.ring[self.pos] = mono;
            self.pos = (self.pos + 1) % FFT_SIZE;
            if self.filled < FFT_SIZE {
                self.filled += 1;
            }
        }
    }

    /// Forget history (after a seek or track change) so the bars drop
    /// instead of smearing old content.
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.filled = 0;
        self.pos = 0;
    }

    /// Compute one frame from the current history. `dt_secs` advances the
    /// attack/decay smoothing.
    pub fn spectrum(&mut self, dt_secs: f32) -> SpectrumFrame {
        let mut frame = SpectrumFrame {
            bands: vec![0.0; BAND_COUNT],
        };
        if self.band_edges.is_empty() || self.filled < FFT_SIZE {
            // Nothing to show yet: just run the fall so old bars decay.
            for v in &mut self.smoothed {
                *v = (*v - FALL_PER_SEC * dt_secs).max(0.0);
            }
            frame.bands.clone_from(&self.smoothed);
            return frame;
        }

        // Windowed, ordered-so-the-ring-is-linear copy of the history.
        let mut input: Vec<Complex<f32>> = (0..FFT_SIZE)
            .map(|i| {
                let sample = self.ring[(self.pos + i) % FFT_SIZE];
                Complex::new(sample * self.window[i], 0.0)
            })
            .collect();
        self.fft.process(&mut input);

        // Magnitude in dB, normalized to 0..=1. A full-scale sine through
        // the Hann window peaks near FFT_SIZE/4 (=-12 dB from N/2), hence
        // the range divisor. 10*log10(power) == 20*log10(amplitude).
        let mags: Vec<f32> = input[..FFT_SIZE / 2]
            .iter()
            .map(|c| {
                let power = c.re * c.re + c.im * c.im;
                let db = 10.0 * power.log10();
                (db / DYNAMIC_RANGE_DB as f32).clamp(0.0, 1.0)
            })
            .collect();

        for band in 0..BAND_COUNT {
            let (lo, hi) = (self.band_edges[band], self.band_edges[band + 1]);
            // Winamp-style bars: the peak bin in the band, not its average.
            let peak = mags[lo..hi].iter().copied().fold(0.0f32, f32::max);
            let smoothed = &mut self.smoothed[band];
            // Instant attack, measured fall.
            *smoothed = if peak >= *smoothed {
                peak
            } else {
                (*smoothed - FALL_PER_SEC * dt_secs).max(peak).max(0.0)
            };
            frame.bands[band] = *smoothed;
        }
        frame
    }
}

impl Default for Analyser {
    fn default() -> Self {
        Self::new()
    }
}

/// Hann window of `len` points.
fn hann_window(len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| {
            (std::f64::consts::PI * i as f64 / (len as f64 - 1.0))
                .sin()
                .powi(2) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `secs` of a sine into the analyser and return the last frame.
    fn analyse(hz: f64, rate: u32, secs: f32) -> SpectrumFrame {
        let mut a = Analyser::new();
        a.set_rate(rate);
        let total = (rate as f64 * secs as f64) as usize;
        let chunk = 1024;
        let mut frame = SpectrumFrame::default();
        let mut t = 0usize;
        while t < total {
            let end = (t + chunk).min(total);
            let samples: Vec<f32> = (t..end)
                .map(|i| {
                    (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin() as f32
                        * 0.9
                })
                .collect();
            a.push(&samples, 1);
            frame = a.spectrum(1.0 / 30.0);
            t = end;
        }
        frame
    }

    fn band_of(hz: f64, rate: u32) -> usize {
        // Find which band a frequency lands in, mirroring set_rate's edges.
        let nyquist = f64::from(rate) / 2.0;
        let top = HIGH_HZ.min(nyquist * 0.95).max(LOW_HZ * 4.0);
        let bin_hz = f64::from(rate) / FFT_SIZE as f64;
        let mut edges: Vec<usize> = (0..=BAND_COUNT)
            .map(|i| {
                let edge_hz = LOW_HZ * (top / LOW_HZ).powf(i as f64 / BAND_COUNT as f64);
                ((edge_hz / bin_hz).round() as usize).clamp(1, FFT_SIZE / 2)
            })
            .collect();
        for i in 1..edges.len() {
            if edges[i] <= edges[i - 1] {
                edges[i] = edges[i - 1] + 1;
            }
        }
        let bin = ((hz / bin_hz).round() as usize).clamp(1, FFT_SIZE / 2 - 1);
        edges
            .windows(2)
            .position(|w| bin >= w[0] && bin < w[1])
            .unwrap_or(BAND_COUNT - 1)
    }

    #[test]
    fn sine_peaks_in_its_own_band() {
        let rate = 44_100;
        let frame = analyse(440.0, rate, 0.2);
        let peak_band = frame
            .bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(
            peak_band,
            band_of(440.0, rate),
            "440 Hz sine should peak in band {}, got {peak_band} ({:?})",
            band_of(440.0, rate),
            frame.bands
        );
        assert!(
            frame.bands[peak_band] > 0.6,
            "full-scale sine should light its band up, got {}",
            frame.bands[peak_band]
        );
    }

    #[test]
    fn silence_is_dark_and_bars_fall() {
        let rate = 44_100;
        let mut a = Analyser::new();
        a.set_rate(rate);
        let loud: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / f64::from(rate)).sin() as f32
            })
            .collect();
        a.push(&loud, 1);
        let lit = a.spectrum(1.0 / 30.0);
        let peak = lit.bands.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak > 0.5, "loud sine lights up ({peak})");

        // Then silence: bars must decay to (near) zero within ~1.5 s.
        let silent = vec![0.0f32; FFT_SIZE];
        for _ in 0..45 {
            a.push(&silent, 1);
            a.spectrum(1.0 / 30.0);
        }
        let after = a.spectrum(1.0 / 30.0);
        let peak = after.bands.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak < 0.05, "bars should fall to dark, still {peak}");
    }

    #[test]
    fn needs_a_rate_and_a_window_before_lighting() {
        let mut a = Analyser::new();
        a.push(&[0.5; FFT_SIZE], 1);
        let frame = a.spectrum(0.033);
        assert!(
            frame.bands.iter().all(|v| *v == 0.0),
            "no rate configured -> no bands"
        );
        a.set_rate(48_000);
        let frame = a.spectrum(0.033);
        assert!(
            frame.bands.iter().all(|v| *v == 0.0),
            "not enough history yet -> no bands"
        );
    }
}
