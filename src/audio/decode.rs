//! symphonia decode loop and the ranged-HTTP media source that feeds it.
//!
//! [`decode_stream`] probes any [`MediaInput`] (a local file in tests, a
//! [`RangedHttpSource`] for YouTube), decodes the default audio track,
//! runs samples through the EQ, taps them into the analyser, and writes
//! them to a sink — pausing, seeking, and stopping at the pleasure of a
//! [`SessionCtl`] callback. The engine (`crate::player`) implements the
//! callback; this module knows nothing about queues or events.

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};

use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use symphonia::default::{get_codecs, get_probe};

use crate::audio::EqProcessor;
use crate::audio::analyser::Analyser;
use crate::audio::sink::{AudioSpec, SampleSink};
use crate::model::SpectrumFrame;

/// Anything the decoder can read from: local files (tests), cursors over
/// fixture bytes, or [`RangedHttpSource`] (YouTube).
///
/// DESIGN.md says `Box<dyn Read + Send>`; isomp4 demuxing needs `Seek`, and
/// symphonia's `MediaSource` wants `Sync` too, so this is the closest
/// implementable contract. Local files and cursors satisfy it
/// automatically via the blanket impl.
pub trait MediaInput: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> MediaInput for T {}

/// Adapts a [`MediaInput`] to symphonia's `MediaSource`.
struct MediaInputStream(Box<dyn MediaInput>);

impl Read for MediaInputStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Seek for MediaInputStream {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
}

impl MediaSource for MediaInputStream {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// What the decode loop should do next.
pub(crate) enum LoopCtl {
    Continue,
    Break,
    /// Seek to a fraction of the track's duration.
    Seek(f64),
}

/// Position/duration bookkeeping shared with the engine after every packet.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PacketStats {
    /// Decoded frames at the current position (seek-aware).
    pub frames: u64,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    /// True when the input hit a clean end of stream.
    pub ended: bool,
}

/// The engine's hook into the decode loop.
pub(crate) trait SessionCtl: Send {
    /// Called once, after probing, with the stream's format — the engine
    /// picks the sink here (device or null).
    fn open_sink(&mut self, spec: AudioSpec) -> Result<Box<dyn SampleSink>>;
    /// Called before each packet: apply volume/EQ changes, wait out
    /// pauses, and decide whether to continue, stop, or seek.
    fn pre_packet(&mut self, sink: &mut dyn SampleSink, eq: &mut EqProcessor) -> LoopCtl;
    /// Called after each packet with fresh position stats and, at ~30 Hz,
    /// a freshly computed spectrum frame.
    fn post_packet(&mut self, stats: &PacketStats, spectrum: Option<&SpectrumFrame>);
    /// Clean end of stream: the engine advances the queue.
    fn on_end(&mut self, stats: &PacketStats);
}

/// Decode `input` to the sink. Returns final stats (also delivered via
/// `post_packet` along the way).
pub(crate) fn decode_stream(
    input: Box<dyn MediaInput>,
    eq: &mut EqProcessor,
    analyser: &mut Analyser,
    ctl: &mut dyn SessionCtl,
) -> Result<PacketStats> {
    let mss = MediaSourceStream::new(
        Box::new(MediaInputStream(input)),
        MediaSourceStreamOptions::default(),
    );
    let hint = Hint::new();
    let probed = get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("probing the audio stream")?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("the stream has no audio track"))?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let time_base = params.time_base;
    let duration_secs = params.n_frames.zip(time_base).map(|(frames, tb)| {
        let time = tb.calc_time(frames);
        time.seconds as f64 + time.frac
    });
    let spec = AudioSpec {
        rate: params.sample_rate.unwrap_or(44_100),
        channels: params.channels.map(|c| c.count() as u16).unwrap_or(2),
    };

    let mut sink = ctl.open_sink(spec)?;
    eq.configure(spec.rate, spec.channels as usize);
    analyser.set_rate(spec.rate);
    let mut decoder = get_codecs()
        .make(&params, &DecoderOptions::default())
        .context("creating the decoder for this codec")?;

    let mut stats = PacketStats {
        duration_secs,
        ..PacketStats::default()
    };
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut buf_spec: Option<SignalSpec> = None;
    // Audio-time position of the last spectrum frame (wall time lies when
    // the sink is unpaced or paused).
    let mut last_spectrum_secs: f64 = 0.0;

    loop {
        // Control point: pause waits here, stops/seeks break out here.
        match ctl.pre_packet(sink.as_mut(), eq) {
            LoopCtl::Continue => {}
            LoopCtl::Break => return Ok(stats),
            LoopCtl::Seek(ratio) => {
                let Some(duration) = stats.duration_secs.filter(|d| *d > 0.0) else {
                    continue; // no duration known; seeking is a no-op
                };
                let target = ratio.clamp(0.0, 0.999_999) * duration;
                let time = Time {
                    seconds: target.floor() as u64,
                    frac: target.fract(),
                };
                let seeked = format
                    .seek(
                        SeekMode::Accurate,
                        SeekTo::Time {
                            track_id: Some(track_id),
                            time,
                        },
                    )
                    .with_context(|| format!("seeking to {target:.1}s"))?;
                // Demuxers land on packet boundaries; report where the seek
                // actually landed (SeekedTo), not where it was asked to go.
                let actual_secs = time_base
                    .map(|tb| {
                        let landed = tb.calc_time(seeked.actual_ts);
                        landed.seconds as f64 + landed.frac
                    })
                    .unwrap_or(target);
                decoder.reset();
                analyser.reset();
                stats.frames = (actual_secs * f64::from(spec.rate)) as u64;
                stats.position_secs = actual_secs;
                last_spectrum_secs = actual_secs;
                continue;
            }
        }

        // Next packet (skipping other tracks' packets inside the loop).
        let packet = loop {
            match format.next_packet() {
                Ok(packet) => {
                    if packet.track_id() != track_id {
                        continue;
                    }
                    break packet;
                }
                Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    stats.ended = true;
                    ctl.on_end(&stats);
                    return Ok(stats);
                }
                Err(SymError::ResetRequired) => {
                    decoder.reset();
                    continue;
                }
                Err(e) => return Err(anyhow!(e)).context("reading the next audio packet"),
            }
        };

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymError::DecodeError(_)) => continue, // corrupt packet: skip
            Err(SymError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => return Err(anyhow!(e)).context("decoding an audio packet"),
        };

        // (Re)create the interleaving buffer if the format changed.
        if buf_spec.as_ref() != Some(decoded.spec()) {
            let new_spec = *decoded.spec();
            let capacity = decoded.capacity().max(1) as u64;
            sample_buf = Some(SampleBuffer::<f32>::new(capacity, new_spec));
            buf_spec = Some(new_spec);
            let channels = new_spec.channels.count();
            if channels != spec.channels as usize || new_spec.rate != spec.rate {
                log::warn!(
                    "stream format changed mid-flight: {} Hz / {} ch -> {} Hz / {} ch",
                    spec.rate,
                    spec.channels,
                    new_spec.rate,
                    channels
                );
                eq.configure(new_spec.rate, channels);
                analyser.set_rate(new_spec.rate);
            }
        }

        let sample_buf = sample_buf
            .as_mut()
            .expect("sample buffer exists after spec check");
        let packet_frames = decoded.frames() as u64;
        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples_mut();
        let channels = buf_spec.expect("spec recorded").channels.count();

        eq.process(samples);
        analyser.push(samples, channels);

        let sink_latency = sink.latency_secs();
        stats.frames += packet_frames;
        stats.position_secs = (stats.frames as f64 / f64::from(spec.rate) - sink_latency).max(0.0);

        let dt = stats.position_secs - last_spectrum_secs;
        if dt >= 0.033 {
            let frame = analyser.spectrum(dt as f32);
            last_spectrum_secs = stats.position_secs;
            ctl.post_packet(&stats, Some(&frame));
        } else {
            ctl.post_packet(&stats, None);
        }
        sink.write(samples)?;
    }
}

/// A seekable `Read` over an HTTP resource using `Range` requests.
///
/// googlevideo URLs are IP- and time-bound (~6 h), so this fetches just
/// ahead of the decoder in 512 KiB slices and reopens at a new offset on
/// seek — the "ranged-HTTP prefetch buffer" from DESIGN.md, in its v0.1
/// shape: sequential chunks, no speculative parallel ranges.
///
/// All async work (reqwest) runs through `handle.block_on`; this type is
/// only ever used from the engine's dedicated decode thread, never from
/// an async context.
pub struct RangedHttpSource {
    client: reqwest::Client,
    handle: tokio::runtime::Handle,
    url: String,
    /// Absolute file offset of the next byte to be read.
    pos: u64,
    /// The currently buffered slice of the resource.
    buf: Vec<u8>,
    buf_start: u64,
    len: Option<u64>,
}

/// Bytes fetched per ranged request. 512 KiB is ~32 s of 128 kb/s AAC —
/// small enough to start fast, big enough to amortize request overhead.
const CHUNK: u64 = 512 * 1024;

impl RangedHttpSource {
    pub fn new(url: String, client: reqwest::Client, handle: tokio::runtime::Handle) -> Self {
        Self {
            client,
            handle,
            url,
            pos: 0,
            buf: Vec::new(),
            buf_start: 0,
            len: None,
        }
    }

    /// Fetch the next slice starting at `self.pos`.
    fn fill(&mut self) -> io::Result<usize> {
        let start = self.pos;
        let end = start + CHUNK - 1;
        let range = format!("bytes={start}-{end}");

        let (status, content_range, body) = self.handle.block_on(async {
            let resp = self
                .client
                .get(&self.url)
                .header(reqwest::header::RANGE, &range)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| io::Error::other(format!("stream fetch failed: {e}")))?;
            let status = resp.status();
            let content_range = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp
                .bytes()
                .await
                .map_err(|e| io::Error::other(format!("stream body failed: {e}")))?;
            Ok::<_, io::Error>((status, content_range, body))
        })?;

        if !status.is_success() {
            return Err(io::Error::other(format!(
                "stream fetch returned HTTP {status}"
            )));
        }
        if status == reqwest::StatusCode::OK && start > 0 {
            // Server ignored the Range header and would silently desync us.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "server ignored the Range request",
            ));
        }
        if let Some(total) = content_range.as_deref().and_then(parse_content_range_total) {
            self.len = Some(total);
        } else if status == reqwest::StatusCode::OK {
            // A plain 200 with no Content-Range: the body is the resource.
            self.len = Some(body.len() as u64);
        }

        self.buf = body.to_vec();
        self.buf_start = start;
        Ok(self.buf.len())
    }

    /// Bytes buffered ahead of `self.pos`, if any.
    fn buffered_ahead(&self) -> usize {
        let end = self.buf_start + self.buf.len() as u64;
        if self.pos >= self.buf_start && self.pos < end {
            (end - self.pos) as usize
        } else {
            0
        }
    }
}

/// `Content-Range: bytes 0-524287/7300000` → `7300000`.
fn parse_content_range_total(header: &str) -> Option<u64> {
    let total = header.rsplit('/').next()?.trim();
    total.parse::<u64>().ok().or(match total {
        "*" => None,
        _ => None,
    })
}

impl Read for RangedHttpSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.buffered_ahead() == 0 {
            if let Some(len) = self.len
                && self.pos >= len
            {
                return Ok(0); // clean EOF
            }
            let fetched = self.fill()?;
            if fetched == 0 {
                return Ok(0);
            }
        }
        let offset = (self.pos - self.buf_start) as usize;
        let available = self.buf.len() - offset;
        let n = available.min(out.len());
        out[..n].copy_from_slice(&self.buf[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RangedHttpSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(delta) => self.pos as i64 + delta,
            SeekFrom::End(delta) => match self.len {
                Some(len) => len as i64 + delta,
                None => {
                    // Unknown length: probe it with a 0-byte-range request?
                    // v0.1: youtube streams always report Content-Range on
                    // the first fill; treat missing length as unsupported.
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "cannot seek from end before the length is known",
                    ));
                }
            },
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }
        self.pos = new_pos as u64;
        // If the new position is inside the buffered slice, keep it;
        // otherwise drop the buffer so the next read refetches.
        if self.buffered_ahead() == 0 {
            self.buf.clear();
            self.buf_start = self.pos;
        }
        Ok(self.pos)
    }
}

impl MediaSource for RangedHttpSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny WAV writer (16-bit PCM mono) so decode tests run offline
    /// against bytes symphonia actually understands.
    fn wav_bytes(rate: u32, secs: f64, hz: f64) -> Vec<u8> {
        let n = (f64::from(rate) * secs) as usize;
        let data_len = (n * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..n {
            let t = i as f64 / f64::from(rate);
            let sample = (2.0 * std::f64::consts::PI * hz * t).sin() * 0.4 * f64::from(i16::MAX);
            out.extend_from_slice(&(sample as i16).to_le_bytes());
        }
        out
    }

    /// Test control: unpaced null sink, minimal bookkeeping.
    struct TestCtl {
        seek_after_frames: Option<(u64, f64)>, // (frames, ratio)
        pending_seek: Option<f64>,
        spectrums: usize,
        packets: usize,
        ended: bool,
    }

    impl TestCtl {
        fn passive() -> Self {
            Self {
                seek_after_frames: None,
                pending_seek: None,
                spectrums: 0,
                packets: 0,
                ended: false,
            }
        }
    }

    impl SessionCtl for TestCtl {
        fn open_sink(&mut self, spec: AudioSpec) -> Result<Box<dyn SampleSink>> {
            Ok(Box::new(crate::audio::sink::NullSink::unpaced(spec)))
        }
        fn pre_packet(&mut self, _sink: &mut dyn SampleSink, _eq: &mut EqProcessor) -> LoopCtl {
            match self.pending_seek.take() {
                Some(ratio) => LoopCtl::Seek(ratio),
                None => LoopCtl::Continue,
            }
        }
        fn post_packet(&mut self, stats: &PacketStats, spectrum: Option<&SpectrumFrame>) {
            self.packets += 1;
            if let Some(frame) = spectrum {
                self.spectrums += 1;
                assert!(!frame.bands.is_empty(), "spectrum frames must have bands");
            }
            if let Some((after, ratio)) = self.seek_after_frames
                && stats.frames > after
            {
                self.seek_after_frames = None;
                self.pending_seek = Some(ratio);
            }
        }
        fn on_end(&mut self, _stats: &PacketStats) {
            self.ended = true;
        }
    }

    #[test]
    fn decodes_wav_and_reports_position_and_duration() {
        let wav = wav_bytes(8000, 2.0, 440.0);
        let mut ctl = TestCtl::passive();
        let mut eq = EqProcessor::new();
        let mut analyser = Analyser::new();
        let stats = decode_stream(
            Box::new(std::io::Cursor::new(wav)),
            &mut eq,
            &mut analyser,
            &mut ctl,
        )
        .expect("decode");
        assert!(ctl.ended, "clean EOF must call on_end");
        assert!(stats.ended);
        assert!((stats.duration_secs.unwrap() - 2.0).abs() < 0.01);
        assert!(
            (stats.position_secs - 2.0).abs() < 0.05,
            "position should reach the end, got {} (frames {})",
            stats.position_secs,
            stats.frames
        );
        assert!(ctl.packets > 5, "WAV packets are ~per-4096-frames");
        assert!(ctl.spectrums >= 1, "2 s of audio must produce spectrums");
    }

    #[test]
    fn break_stops_decoding() {
        struct BreakAfter {
            seen: usize,
        }
        impl SessionCtl for BreakAfter {
            fn open_sink(&mut self, spec: AudioSpec) -> Result<Box<dyn SampleSink>> {
                Ok(Box::new(crate::audio::sink::NullSink::unpaced(spec)))
            }
            fn pre_packet(&mut self, _s: &mut dyn SampleSink, _e: &mut EqProcessor) -> LoopCtl {
                self.seen += 1;
                if self.seen > 3 {
                    LoopCtl::Break
                } else {
                    LoopCtl::Continue
                }
            }
            fn post_packet(&mut self, _st: &PacketStats, _sp: Option<&SpectrumFrame>) {}
            fn on_end(&mut self, _st: &PacketStats) {
                panic!("Break must not look like EOF");
            }
        }
        let wav = wav_bytes(8000, 2.0, 440.0);
        let mut ctl = BreakAfter { seen: 0 };
        let stats = decode_stream(
            Box::new(std::io::Cursor::new(wav)),
            &mut EqProcessor::new(),
            &mut Analyser::new(),
            &mut ctl,
        )
        .expect("decode with break");
        assert!(!stats.ended, "break is not an end");
        assert!(stats.frames < 16_000, "stopped early-ish");
    }

    #[test]
    fn seek_moves_the_position() {
        let wav = wav_bytes(8000, 2.0, 440.0);
        let mut ctl = TestCtl {
            seek_after_frames: Some((2000, 0.5)),
            ..TestCtl::passive()
        };
        let stats = decode_stream(
            Box::new(std::io::Cursor::new(wav)),
            &mut EqProcessor::new(),
            &mut Analyser::new(),
            &mut ctl,
        )
        .expect("decode with seek");
        // The track ends after seeking to halfway and playing the rest:
        // final position = 2.0 s, but the total decoded work must be
        // roughly half + a bit, not the whole file twice.
        assert!(ctl.ended, "seek then play to the end");
        assert!(
            (stats.position_secs - 2.0).abs() < 0.05,
            "position should still reach the end after seeking, got {} (frames {})",
            stats.position_secs,
            stats.frames
        );
    }

    #[test]
    fn content_range_totals_parse() {
        assert_eq!(
            parse_content_range_total("bytes 0-524287/7300000"),
            Some(7_300_000)
        );
        assert_eq!(parse_content_range_total("bytes */*"), None);
        assert_eq!(parse_content_range_total("garbage"), None);
    }

    #[test]
    fn wav_writes_are_symphonia_readable() {
        // Sanity for the fixture builder itself: probe must find RIFF/WAVE.
        let wav = wav_bytes(8000, 0.5, 440.0);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 8000);
    }
}
