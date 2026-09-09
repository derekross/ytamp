//! Audio output sinks.
//!
//! [`SampleSink`] is what the decode loop writes interleaved f32 frames
//! into. Two implementations:
//!
//! - [`NullSink`] — always compiled. Consumes samples and (optionally)
//!   paces writes to real time, so the whole engine — position, seek,
//!   spectrum, track advance — works with no audio hardware and no ALSA
//!   headers. This is what CI, tests, and `cargo build` without the
//!   `audio-alsa` feature use.
//! - [`RodioSink`] — behind `--features audio-alsa`. Real device output
//!   through rodio/cpal. Needs `libasound2-dev` + `pkg-config` on Linux
//!   (see `src/audio/mod.rs`).

#[cfg(feature = "audio-alsa")]
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::Result;

#[cfg(feature = "audio-alsa")]
use std::sync::{Arc, Mutex, OnceLock};

/// The stream format a sink was opened for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSpec {
    pub rate: u32,
    pub channels: u16,
}

impl AudioSpec {
    /// Samples per second across all channels.
    pub fn samples_per_sec(&self) -> u64 {
        u64::from(self.rate) * u64::from(self.channels)
    }
}

/// Where the decode loop writes its output.
pub trait SampleSink: Send {
    fn spec(&self) -> AudioSpec;
    /// Write one buffer of interleaved samples. May block to pace the
    /// decoder to real time.
    fn write(&mut self, samples: &[f32]) -> Result<()>;
    fn set_volume(&mut self, volume: f32);
    fn pause(&mut self);
    fn play(&mut self);
    /// Estimated buffered-but-unplayed duration, for position honesty.
    fn latency_secs(&self) -> f64 {
        0.0
    }
}

/// Consumes samples; optionally paces writes to real time.
///
/// With `realtime` pacing this makes a feature-less build behave like a
/// player: `write` sleeps so that N seconds of audio take N seconds, which
/// drives position updates, spectrum cadence, and auto-advance exactly as
/// a device sink would. Tests construct it unpaced (`NullSink::unpaced`)
/// so they finish instantly.
pub struct NullSink {
    spec: AudioSpec,
    realtime: bool,
    written: u64,
    started: Option<Instant>,
    volume: f32,
}

impl NullSink {
    /// Paced to real time — the engine default when device output is off.
    pub fn paced(spec: AudioSpec) -> Self {
        Self {
            spec,
            realtime: true,
            written: 0,
            started: None,
            volume: 1.0,
        }
    }

    /// Unpaced — drains as fast as the decoder produces. For tests.
    pub fn unpaced(spec: AudioSpec) -> Self {
        Self {
            spec,
            realtime: false,
            written: 0,
            started: None,
            volume: 1.0,
        }
    }

    /// Samples handed to this sink so far (all channels).
    pub fn written_samples(&self) -> u64 {
        self.written
    }
}

impl SampleSink for NullSink {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn write(&mut self, samples: &[f32]) -> Result<()> {
        self.written += samples.len() as u64;
        if self.realtime {
            let started = *self.started.get_or_insert_with(Instant::now);
            let target = started
                + Duration::from_secs_f64(self.written as f64 / self.spec.samples_per_sec() as f64);
            let now = Instant::now();
            if now < target {
                std::thread::sleep(target - now);
            }
        }
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    fn pause(&mut self) {
        // Pacing is derived from total written samples, so pausing simply
        // means the decode loop stops calling write(); nothing to do.
    }

    fn play(&mut self) {}

    fn latency_secs(&self) -> f64 {
        0.0
    }
}

/// A silent volume field is kept so the trait object is meaningful without
/// device output; it is used by nothing else.
impl NullSink {
    #[allow(dead_code)]
    fn volume(&self) -> f32 {
        self.volume
    }
}

#[cfg(feature = "audio-alsa")]
mod device {
    use super::*;

    use anyhow::Context as _;

    /// How much audio the ring may hold ahead of the device (seconds).
    const BACKLOG_SECS: f64 = 2.0;

    /// The process-wide output stream. Kept forever once opened; rebuilt
    /// if a track arrives with a different sample rate (rare: YouTube AAC
    /// is 44.1 kHz or 48 kHz).
    struct Device {
        _stream: rodio::OutputStream,
        handle: rodio::OutputStreamHandle,
        rate: u32,
    }

    static DEVICE: OnceLock<Mutex<Option<Device>>> = OnceLock::new();

    fn shared_ring(spec: AudioSpec) -> Arc<Mutex<VecDeque<f32>>> {
        Arc::new(Mutex::new(VecDeque::with_capacity(
            spec.samples_per_sec() as usize / 4,
        )))
    }

    /// A pull-source over the shared sample ring. Lives on rodio's audio
    /// thread: `next` must never block, so underruns emit silence.
    struct LiveSource {
        ring: Arc<Mutex<VecDeque<f32>>>,
        spec: AudioSpec,
    }

    impl Iterator for LiveSource {
        type Item = f32;
        fn next(&mut self) -> f32 {
            match self.ring.lock() {
                Ok(mut queue) => queue.pop_front().unwrap_or(0.0),
                Err(_) => 0.0,
            }
        }
    }

    impl rodio::Source for LiveSource {
        fn current_frame_len(&self) -> Option<usize> {
            None
        }
        fn channels(&self) -> u16 {
            self.spec.channels
        }
        fn sample_rate(&self) -> u32 {
            self.spec.rate
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    /// Device output through rodio/cpal (needs the `audio-alsa` feature).
    pub struct RodioSink {
        ring: Arc<Mutex<VecDeque<f32>>>,
        sink: rodio::Sink,
        spec: AudioSpec,
        volume: f32,
        backlog: usize,
    }

    impl RodioSink {
        /// Open (or reuse) the output stream for `spec` and start pulling.
        pub fn open(spec: AudioSpec) -> Result<Self> {
            let cell = DEVICE.get_or_init(|| Mutex::new(None));
            let mut guard = cell
                .lock()
                .map_err(|_| anyhow::anyhow!("audio device lock poisoned"))?;
            if guard.as_ref().is_none_or(|d| d.rate != spec.rate) {
                let (stream, handle) = rodio::OutputStream::try_default()
                    .context("opening the audio output stream (is a device present?)")?;
                *guard = Some(Device {
                    _stream: stream,
                    handle,
                    rate: spec.rate,
                });
            }
            let handle = guard.as_ref().expect("device just set").handle.clone();
            drop(guard);

            let ring = shared_ring(spec);
            let sink = rodio::Sink::try_new(&handle).context("creating the playback sink")?;
            sink.append(LiveSource {
                ring: ring.clone(),
                spec,
            });
            Ok(Self {
                backlog: (spec.samples_per_sec() as f64 * BACKLOG_SECS) as usize,
                ring,
                sink,
                spec,
                volume: 1.0,
            })
        }
    }

    impl SampleSink for RodioSink {
        fn spec(&self) -> AudioSpec {
            self.spec
        }

        fn write(&mut self, samples: &[f32]) -> Result<()> {
            // Backpressure: if the ring is well ahead of the device, wait
            // for it to drain a little rather than buffering unbounded.
            loop {
                let queued = self.ring.lock().map(|q| q.len()).unwrap_or(self.backlog);
                if queued <= self.backlog {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if let Ok(mut queue) = self.ring.lock() {
                queue.extend(samples.iter().copied());
            }
            Ok(())
        }

        fn set_volume(&mut self, volume: f32) {
            self.volume = volume.clamp(0.0, 1.0);
            self.sink.set_volume(self.volume);
        }

        fn pause(&mut self) {
            self.sink.pause();
        }

        fn play(&mut self) {
            self.sink.play();
        }

        fn latency_secs(&self) -> f64 {
            self.ring.lock().map(|q| q.len() as f64).unwrap_or(0.0)
                / self.spec.samples_per_sec() as f64
        }
    }

    impl Drop for RodioSink {
        fn drop(&mut self) {
            self.sink.stop();
        }
    }
}

#[cfg(feature = "audio-alsa")]
pub use device::RodioSink;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpaced_null_sink_drains_instantly() {
        let mut sink = NullSink::unpaced(AudioSpec {
            rate: 8000,
            channels: 1,
        });
        let started = Instant::now();
        // Ten seconds of audio.
        sink.write(&[0.0; 80_000]).expect("write");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(sink.written_samples(), 80_000);
    }

    #[test]
    fn paced_null_sink_sleeps_like_playback() {
        let mut sink = NullSink::paced(AudioSpec {
            rate: 8000,
            channels: 1,
        });
        let started = Instant::now();
        sink.write(&[0.0; 400]).expect("write"); // 50 ms
        sink.write(&[0.0; 400]).expect("write"); // another 50 ms
        assert!(
            started.elapsed() >= Duration::from_millis(90),
            "paced sink must hold real time, {} ms elapsed",
            started.elapsed().as_millis()
        );
    }

    #[cfg(feature = "audio-alsa")]
    #[test]
    fn rodio_sink_opens_when_a_device_exists() {
        // On a headless box this reports "no device" rather than panicking;
        // on Derek's laptop it should open. Either way it must not hang.
        let spec = AudioSpec {
            rate: 44_100,
            channels: 2,
        };
        match RodioSink::open(spec) {
            Ok(mut sink) => {
                sink.write(&[0.0; 128]).expect("write");
                sink.set_volume(0.5);
                sink.pause();
                sink.play();
            }
            Err(e) => {
                let text = format!("{e:#}");
                assert!(
                    text.to_lowercase().contains("device")
                        || text.to_lowercase().contains("stream"),
                    "unexpected error: {text}"
                );
            }
        }
    }

    #[test]
    fn spec_math() {
        let spec = AudioSpec {
            rate: 44_100,
            channels: 2,
        };
        assert_eq!(spec.samples_per_sec(), 88_200);
    }
}
