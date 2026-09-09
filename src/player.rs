//! Builder B: player engine — owns the audio pipeline, answers
//! `PlayerCommand`, emits `PlayerEvent`.
//!
//! Threading: one owned tokio runtime drives the command loop and the
//! session supervisor; each playing track gets a dedicated OS thread that
//! resolves the stream, then decodes it through
//! [`crate::audio::decode::decode_stream`]. Sessions are invalidated by a
//! generation counter, so Next/Stop/queue-replace simply bump the counter
//! and the old thread exits at its next packet.
//!
//! Audio output follows the `audio-alsa` feature (see `src/audio/mod.rs`):
//! with it, rodio/cpal feed the device; without it, a realtime-paced null
//! sink keeps position, seek, spectrum, and auto-advance honest on boxes
//! with no ALSA headers and no sound card.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use futures_util::future::BoxFuture;
use tokio::runtime::Runtime;
use tokio::sync::broadcast::Sender as BroadcastSender;
use tokio::sync::{mpsc, watch};

use crate::audio::analyser::Analyser;
use crate::audio::decode::{LoopCtl, MediaInput, PacketStats, SessionCtl, decode_stream};
use crate::audio::sink::{AudioSpec, NullSink, SampleSink};
use crate::audio::{EqProcessor, EqSpec};
use crate::model::{PlaybackState, PlayerCommand, PlayerEvent, SpectrumFrame, Track};
use crate::yt::YtClient;

/// Fetches a decodable stream for a track: the byte source plus an optional
/// duration hint (seconds, from the resolver's `approxDurationMs`).
pub type TrackFetcher = Arc<
    dyn Fn(Track) -> BoxFuture<'static, Result<(Box<dyn MediaInput>, Option<u64>)>> + Send + Sync,
>;

/// How often the engine broadcasts a full `PlaybackState` while playing
/// (spectrum frames go out far more often).
const STATE_PERIOD: Duration = Duration::from_millis(250);
/// Poll interval while parked in a pause.
const PAUSE_POLL: Duration = Duration::from_millis(50);

/// The player engine (DESIGN.md contract).
///
/// `spawn` must be called from synchronous code (eframe's setup, a test's
/// main) — it builds its own tokio runtime and refuses to nest one. The
/// command channel is tokio's; from sync UI code use
/// `tokio::sync::mpsc::Sender::blocking_send`.
pub struct PlayerEngine {
    inner: Arc<EngineInner>,
    threads: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

/// Everything shared between the command loop, the supervisor, and decode
/// threads. `core` is the single source of truth for state.
struct EngineInner {
    core: Mutex<CoreState>,
    events: BroadcastSender<PlayerEvent>,
    /// Broadcasts the latest session generation to the supervisor.
    gen_tx: watch::Sender<u64>,
    /// Global kill switch for decode threads (set on Drop).
    shutdown: AtomicBool,
    fetcher: TrackFetcher,
    runtime: Runtime,
}

#[derive(Default)]
struct CoreState {
    queue: Vec<Track>,
    queue_index: Option<usize>,
    /// The session a decode thread should be playing: (generation, track).
    current: Option<(u64, Track)>,
    generation: u64,
    paused: bool,
    volume: f32,
    eq: EqSpec,
    position_secs: f64,
    duration_secs: Option<f64>,
    seek: Option<f64>,
}

impl PlayerEngine {
    /// Anonymous YouTube client, per the frozen contract.
    pub fn spawn(
        commands: mpsc::Receiver<PlayerCommand>,
        events: BroadcastSender<PlayerEvent>,
    ) -> Result<Self> {
        Self::spawn_with(commands, events, YtClient::new(None))
    }

    /// Same, with cookies armed (Premium itag 141 lane when the user has
    /// them). Builder C: construct `YtClient::new(cookie_text)` and pass it
    /// here.
    pub fn spawn_with(
        commands: mpsc::Receiver<PlayerCommand>,
        events: BroadcastSender<PlayerEvent>,
        client: YtClient,
    ) -> Result<Self> {
        let runtime = build_runtime()?;
        let handle = runtime.handle().clone();
        let fetcher = youtube_fetcher(client, handle);
        Self::spawn_inner(commands, events, fetcher, runtime)
    }

    /// Fully injectable stream source — how the offline tests feed local
    /// WAV bytes through the real engine.
    pub fn spawn_with_fetcher(
        commands: mpsc::Receiver<PlayerCommand>,
        events: BroadcastSender<PlayerEvent>,
        fetcher: TrackFetcher,
    ) -> Result<Self> {
        Self::spawn_inner(commands, events, fetcher, build_runtime()?)
    }

    fn spawn_inner(
        commands: mpsc::Receiver<PlayerCommand>,
        events: BroadcastSender<PlayerEvent>,
        fetcher: TrackFetcher,
        runtime: Runtime,
    ) -> Result<Self> {
        let (gen_tx, gen_rx) = watch::channel(0u64);
        let inner = Arc::new(EngineInner {
            core: Mutex::new(CoreState::default()),
            events,
            gen_tx,
            shutdown: AtomicBool::new(false),
            fetcher,
            runtime,
        });

        // Command loop.
        let cmd_inner = inner.clone();
        inner.runtime.spawn(async move {
            let mut commands = commands;
            while let Some(cmd) = commands.recv().await {
                handle_command(&cmd_inner, cmd);
            }
            // All senders dropped: the app is going away. Stop playback.
            stop_everything(&cmd_inner);
        });

        // Session supervisor.
        let sup_inner = inner.clone();
        let threads: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sup_threads = threads.clone();
        inner.runtime.spawn(async move {
            let mut rx = gen_rx;
            let mut spawned: u64 = 0;
            loop {
                let latest = *rx.borrow_and_update();
                if latest > spawned {
                    spawned = latest;
                    let session = lock_core(&sup_inner).current.clone();
                    if let Some((session_gen, track)) = session
                        && session_gen == latest
                    {
                        let inner = sup_inner.clone();
                        let handle = std::thread::Builder::new()
                            .name(format!("ytamp-decode-{}", track.video_id))
                            .spawn(move || run_session(inner, track, session_gen))
                            .expect("spawn decode thread");
                        if let Ok(mut list) = sup_threads.lock() {
                            list.retain(|h| !h.is_finished());
                            list.push(handle);
                        }
                    }
                }
                if rx.changed().await.is_err() {
                    break; // engine dropped
                }
            }
        });

        // Let the UI draw something sane immediately.
        emit_state(&inner);

        Ok(Self { inner, threads })
    }

    /// Stop playback and wind the engine down. Idempotent. The engine also
    /// does this on drop, but dropping the command senders first is the
    /// clean shutdown path for the app.
    pub fn shutdown(&self) {
        stop_everything(&self.inner);
    }

    /// A clone of the event sender, so embedders can subscribe extra
    /// listeners (Builder C keeps its own anyway).
    pub fn events(&self) -> BroadcastSender<PlayerEvent> {
        self.inner.events.clone()
    }
}

impl Drop for PlayerEngine {
    fn drop(&mut self) {
        stop_everything(&self.inner);
        // Give in-flight decode threads a moment to notice the kill switch
        // before the runtime goes away underneath them.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let busy = self
                .threads
                .lock()
                .map(|list| list.iter().any(|h| !h.is_finished()))
                .unwrap_or(false);
            if !busy {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn build_runtime() -> Result<Runtime> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(anyhow!(
            "PlayerEngine::spawn builds its own tokio runtime; call it from \
             synchronous code (eframe setup / test main), not from an async task"
        ));
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("ytamp-engine")
        .enable_all()
        .build()
        .context("building the player engine runtime")
}

/// The real fetcher: resolve via InnerTube, then stream ranged HTTP.
/// `handle` must be the engine runtime's handle — the future runs via
/// `block_on` from the decode thread.
fn youtube_fetcher(client: YtClient, handle: tokio::runtime::Handle) -> TrackFetcher {
    Arc::new(move |track: Track| {
        let client = client.clone();
        let handle = handle.clone();
        Box::pin(async move {
            let stream = crate::yt::resolve_stream(&client, &track.video_id)
                .await
                .with_context(|| format!("resolving a stream for {}", track.display()))?;
            let source = crate::audio::decode::RangedHttpSource::open(
                stream.url,
                client.http().clone(),
                handle,
            )
            .await
            .with_context(|| format!("opening the stream for {}", track.display()))?;
            Ok((
                Box::new(source) as Box<dyn MediaInput>,
                stream.duration_secs,
            ))
        })
    })
}

// ---------------------------------------------------------------------------
// Command handling
// ---------------------------------------------------------------------------

fn handle_command(inner: &Arc<EngineInner>, cmd: PlayerCommand) {
    let mut core = lock_core(inner);
    let mut start: Option<usize> = None;
    match cmd {
        PlayerCommand::PlayAt(i) => start = Some(i),
        PlayerCommand::QueueReplace(tracks, index) => {
            core.queue = tracks;
            match index {
                Some(i) => start = Some(i),
                None => stop_current(&mut core),
            }
        }
        PlayerCommand::QueueAppend(tracks) => {
            core.queue.extend(tracks);
        }
        PlayerCommand::Next => {
            let next = core.queue_index.map_or(0, |i| i.saturating_add(1));
            if next < core.queue.len() {
                start = Some(next);
            } else {
                stop_current(&mut core);
            }
        }
        PlayerCommand::Prev => {
            let prev = core.queue_index.map_or(0, |i| i.saturating_sub(1));
            if prev < core.queue.len() {
                start = Some(prev);
            }
        }
        PlayerCommand::PlayPause => core.paused = !core.paused,
        PlayerCommand::Pause => core.paused = true,
        PlayerCommand::Resume => core.paused = false,
        PlayerCommand::Stop => stop_current(&mut core),
        PlayerCommand::SeekRatio(ratio) => {
            if core.current.is_some() {
                core.seek = Some(ratio.clamp(0.0, 1.0));
            }
        }
        PlayerCommand::SetVolume(v) => core.volume = v.clamp(0.0, 1.0),
        PlayerCommand::SetEq {
            enabled,
            gains_db,
            preamp_db,
        } => {
            core.eq = EqSpec {
                enabled,
                gains_db,
                preamp_db,
            };
        }
    }
    let mut gen_changed = false;
    if let Some(i) = start.filter(|&i| i < core.queue.len()) {
        start_session(&mut core, i);
        gen_changed = true;
    }
    let generation = core.generation;
    drop(core);
    if gen_changed {
        let _ = inner.gen_tx.send(generation);
    }
    emit_state(inner);
}

fn start_session(core: &mut CoreState, index: usize) {
    core.generation += 1;
    core.queue_index = Some(index);
    core.current = core
        .queue
        .get(index)
        .cloned()
        .map(|track| (core.generation, track));
    core.paused = false;
    core.position_secs = 0.0;
    core.duration_secs = None;
    core.seek = None;
}

fn stop_current(core: &mut CoreState) {
    core.current = None;
    core.paused = false;
    core.generation += 1;
    core.position_secs = 0.0;
    core.duration_secs = None;
    core.seek = None;
}

fn stop_everything(inner: &Arc<EngineInner>) {
    inner.shutdown.store(true, Ordering::Relaxed);
    let generation = {
        let mut core = lock_core(inner);
        core.current = None;
        core.paused = false;
        core.generation += 1;
        core.seek = None;
        core.generation
    };
    let _ = inner.gen_tx.send(generation);
    emit_state(inner);
}

fn lock_core(inner: &Arc<EngineInner>) -> std::sync::MutexGuard<'_, CoreState> {
    inner
        .core
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn current_generation(core: &CoreState) -> Option<u64> {
    core.current.as_ref().map(|(g, _)| *g)
}

fn snapshot(core: &CoreState) -> PlaybackState {
    PlaybackState {
        playing: core.current.is_some() && !core.paused,
        track: core.current.as_ref().map(|(_, track)| track.clone()),
        position_secs: core.position_secs,
        duration_secs: core.duration_secs,
        volume: core.volume,
        queue: core.queue.clone(),
        queue_index: core.queue_index,
    }
}

fn emit(inner: &Arc<EngineInner>, event: PlayerEvent) {
    let _ = inner.events.send(event); // no subscribers is fine
}

fn emit_state(inner: &Arc<EngineInner>) {
    let state = snapshot(&lock_core(inner));
    emit(inner, PlayerEvent::State(state));
}

// ---------------------------------------------------------------------------
// Decode sessions
// ---------------------------------------------------------------------------

fn run_session(inner: Arc<EngineInner>, track: Track, generation: u64) {
    if inner.shutdown.load(Ordering::Relaxed) {
        return;
    }

    // Resolve the stream (network) on this dedicated thread via the
    // engine's runtime.
    let fetched = inner
        .runtime
        .handle()
        .block_on((inner.fetcher)(track.clone()));
    let (input, duration_hint) = match fetched {
        Ok(ok) => ok,
        Err(e) => {
            emit(
                &inner,
                PlayerEvent::Error(format!("couldn't open {}: {e:#}", track.display())),
            );
            invalidate_session(&inner, generation);
            return;
        }
    };

    // Seed the duration hint so SeekRatio works even before the decoder
    // reports its own.
    if let Some(hint) = duration_hint {
        let mut core = lock_core(&inner);
        if current_generation(&core) == Some(generation) && core.duration_secs.is_none() {
            core.duration_secs = Some(hint as f64);
        }
    }

    let mut ctl = EngineCtl {
        inner: inner.clone(),
        generation,
        duration_hint,
        last_state: Instant::now() - STATE_PERIOD * 2, // force an early State
        sink_paused: false,
    };
    let mut eq = EqProcessor::new();
    let mut analyser = Analyser::new();

    match decode_stream(input, &mut eq, &mut analyser, &mut ctl) {
        Ok(stats) => {
            if stats.ended {
                advance_queue(&inner, generation);
            } // Break: superseded or stopped — the winner owns the state
        }
        Err(e) => {
            emit(
                &inner,
                PlayerEvent::Error(format!("playback failed: {e:#}")),
            );
            invalidate_session(&inner, generation);
        }
    }
}

/// End of track: move to the next queue entry, or stop at the end.
fn advance_queue(inner: &Arc<EngineInner>, generation: u64) {
    let next_gen = {
        let mut core = lock_core(inner);
        if current_generation(&core) != Some(generation) {
            return; // a newer session already took over
        }
        let next = core
            .queue_index
            .and_then(|i| i.checked_add(1))
            .filter(|&i| i < core.queue.len());
        match next {
            Some(i) => {
                start_session(&mut core, i);
                core.generation
            }
            None => {
                stop_current(&mut core);
                core.generation
            }
        }
    };
    let _ = inner.gen_tx.send(next_gen);
    emit_state(inner);
}

/// A session died badly: clear it (if still current) so the UI isn't stuck.
fn invalidate_session(inner: &Arc<EngineInner>, generation: u64) {
    let latest = {
        let mut core = lock_core(inner);
        if current_generation(&core) == Some(generation) {
            stop_current(&mut core);
        }
        core.generation
    };
    let _ = inner.gen_tx.send(latest);
    emit_state(inner);
}

/// The engine's `SessionCtl`: bridges the decode loop into core state and
/// the event stream.
struct EngineCtl {
    inner: Arc<EngineInner>,
    generation: u64,
    duration_hint: Option<u64>,
    last_state: Instant,
    sink_paused: bool,
}

impl SessionCtl for EngineCtl {
    fn open_sink(&mut self, spec: AudioSpec) -> Result<Box<dyn SampleSink>> {
        #[cfg(feature = "audio-alsa")]
        {
            match crate::audio::sink::RodioSink::open(spec) {
                Ok(sink) => return Ok(Box::new(sink)),
                Err(e) => {
                    let note = format!("no audio device ({e:#}); playing silently");
                    log::warn!("{note}");
                    emit(&self.inner, PlayerEvent::Info(note));
                }
            }
        }
        #[cfg(not(feature = "audio-alsa"))]
        log::debug!(
            "audio-alsa feature off: pacing a null sink at {} Hz",
            spec.rate
        );
        Ok(Box::new(NullSink::paced(spec)))
    }

    fn pre_packet(&mut self, sink: &mut dyn SampleSink, eq: &mut EqProcessor) -> LoopCtl {
        loop {
            let (act, paused_now, volume) = {
                let mut core = lock_core(&self.inner);
                if self.inner.shutdown.load(Ordering::Relaxed)
                    || current_generation(&core) != Some(self.generation)
                {
                    (LoopCtl::Break, core.paused, core.volume)
                } else if let Some(ratio) = core.seek.take() {
                    (LoopCtl::Seek(ratio), core.paused, core.volume)
                } else {
                    eq.set_spec(core.eq);
                    (LoopCtl::Continue, core.paused, core.volume)
                }
            };
            match act {
                LoopCtl::Break => return LoopCtl::Break,
                LoopCtl::Seek(_) => return act,
                LoopCtl::Continue => {}
            }
            // Sink pause/play transitions mirror core.paused.
            if paused_now != self.sink_paused {
                self.sink_paused = paused_now;
                if paused_now {
                    sink.pause();
                } else {
                    sink.play();
                }
            }
            if !paused_now {
                sink.set_volume(volume);
                return LoopCtl::Continue;
            }
            std::thread::sleep(PAUSE_POLL);
        }
    }

    fn post_packet(&mut self, stats: &PacketStats, spectrum: Option<&SpectrumFrame>) {
        if let Some(frame) = spectrum {
            emit(&self.inner, PlayerEvent::Spectrum(frame.clone()));
        }
        let now = Instant::now();
        if now.duration_since(self.last_state) < STATE_PERIOD {
            return;
        }
        self.last_state = now;
        let mut core = lock_core(&self.inner);
        if current_generation(&core) != Some(self.generation) {
            return;
        }
        core.position_secs = stats.position_secs;
        if stats.duration_secs.is_some() {
            core.duration_secs = stats.duration_secs;
        } else if core.duration_secs.is_none() {
            core.duration_secs = self.duration_hint.map(|d| d as f64);
        }
        let state = snapshot(&core);
        drop(core);
        emit(&self.inner, PlayerEvent::State(state));
    }

    fn on_end(&mut self, stats: &PacketStats) {
        // decode_stream returns right after; final handling (queue advance)
        // happens in run_session. Record the final position first.
        let mut core = lock_core(&self.inner);
        if current_generation(&core) == Some(self.generation) {
            core.position_secs = stats.position_secs;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    /// WAV bytes: 16-bit PCM mono, `secs` of 440 Hz at 8 kHz.
    fn wav(rate: u32, secs: f64) -> Vec<u8> {
        let n = (f64::from(rate) * secs) as usize;
        let mut out = Vec::with_capacity(44 + n * 2);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + n as u32 * 2).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&((n * 2) as u32).to_le_bytes());
        for i in 0..n {
            let t = i as f64 / f64::from(rate);
            let sample = (2.0 * std::f64::consts::PI * 440.0 * t).sin() * 0.4 * f64::from(i16::MAX);
            out.extend_from_slice(&(sample as i16).to_le_bytes());
        }
        out
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.to_string(),
            title: format!("Track {id}"),
            artist: "Tester".into(),
            album: None,
            duration_secs: Some(2),
            thumb_url: None,
        }
    }

    /// A fetcher serving 2 s of WAV per track, entirely offline.
    fn wav_fetcher() -> TrackFetcher {
        let bytes = std::sync::Arc::new(wav(8000, 2.0));
        Arc::new(move |_track: Track| {
            let bytes = bytes.clone();
            Box::pin(async move {
                Ok((
                    Box::new(std::io::Cursor::new((*bytes).clone())) as Box<dyn MediaInput>,
                    Some(2u64),
                ))
            })
        })
    }

    fn spawn_engine() -> (
        PlayerEngine,
        mpsc::Sender<PlayerCommand>,
        broadcast::Receiver<PlayerEvent>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (evt_tx, evt_rx) = broadcast::channel(256);
        let engine =
            PlayerEngine::spawn_with_fetcher(cmd_rx, evt_tx, wav_fetcher()).expect("engine");
        (engine, cmd_tx, evt_rx)
    }

    fn send(cmd_tx: &mpsc::Sender<PlayerCommand>, cmd: PlayerCommand) {
        cmd_tx.blocking_send(cmd).expect("command channel");
    }

    /// Collect events until `pred` matches one (or timeout).
    fn wait_for_event<T: FnMut(&PlayerEvent) -> bool>(
        rx: &mut broadcast::Receiver<PlayerEvent>,
        mut pred: T,
        timeout: Duration,
    ) -> Option<PlayerEvent> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(event) => {
                    if pred(&event) {
                        return Some(event);
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Closed) => return None,
            }
        }
        None
    }

    #[test]
    fn engine_plays_a_queue_end_to_end() {
        let (engine, cmd_tx, mut evt_rx) = spawn_engine();
        send(
            &cmd_tx,
            PlayerCommand::QueueReplace(vec![track("aaaaaaaaaaa"), track("bbbbbbbbbbb")], Some(0)),
        );

        let started = wait_for_event(
            &mut evt_rx,
            |e| matches!(e, PlayerEvent::State(s) if s.playing && s.track.is_some()),
            Duration::from_secs(5),
        );
        assert!(started.is_some(), "must reach playing state");

        let spectrum = wait_for_event(
            &mut evt_rx,
            |e| matches!(e, PlayerEvent::Spectrum(_)),
            Duration::from_secs(2),
        );
        assert!(
            spectrum.is_some(),
            "spectrum frames must flow while playing"
        );

        // Track 1 ends -> track 2 auto-plays (2 s paced each).
        let second = wait_for_event(
            &mut evt_rx,
            |e| {
                matches!(e, PlayerEvent::State(s) if s
                .track.as_ref().is_some_and(|t| t.video_id == "bbbbbbbbbbb"))
            },
            Duration::from_secs(6),
        );
        assert!(second.is_some(), "auto-advance to the second track");

        // Queue ends -> stops playing.
        let stopped = wait_for_event(
            &mut evt_rx,
            |e| matches!(e, PlayerEvent::State(s) if !s.playing),
            Duration::from_secs(6),
        );
        assert!(stopped.is_some(), "must stop after the queue drains");

        send(&cmd_tx, PlayerCommand::Stop);
        engine.shutdown();
    }

    #[test]
    fn pause_freezes_position_and_resume_continues() {
        let (engine, cmd_tx, mut evt_rx) = spawn_engine();
        send(
            &cmd_tx,
            PlayerCommand::QueueReplace(vec![track("ccccccccccc")], Some(0)),
        );
        wait_for_event(
            &mut evt_rx,
            |e| matches!(e, PlayerEvent::State(s) if s.playing && s.position_secs > 0.3),
            Duration::from_secs(5),
        )
        .expect("playing and progressing");

        send(&cmd_tx, PlayerCommand::Pause);
        let frozen_at = wait_for_event(
            &mut evt_rx,
            |e| matches!(e, PlayerEvent::State(s) if !s.playing),
            Duration::from_secs(2),
        )
        .expect("paused state");
        let position = match frozen_at {
            PlayerEvent::State(s) => s.position_secs,
            _ => unreachable!(),
        };

        // While paused, no State events with a moving position arrive.
        std::thread::sleep(Duration::from_millis(400));
        let mut moved = false;
        while let Ok(event) = evt_rx.try_recv() {
            if let PlayerEvent::State(s) = event
                && (s.position_secs - position).abs() > 0.15
            {
                moved = true;
            }
        }
        assert!(!moved, "position must freeze while paused");

        send(&cmd_tx, PlayerCommand::Resume);
        let resumed = wait_for_event(
            &mut evt_rx,
            |e| matches!(e, PlayerEvent::State(s) if s.playing && s.position_secs > position + 0.2),
            Duration::from_secs(4),
        );
        assert!(
            resumed.is_some(),
            "resume must continue from ~{position:.2}s, not restart"
        );

        send(&cmd_tx, PlayerCommand::Stop);
        engine.shutdown();
    }

    #[test]
    fn volume_and_eq_commands_are_answered_with_state() {
        let (engine, cmd_tx, mut evt_rx) = spawn_engine();
        send(&cmd_tx, PlayerCommand::SetVolume(0.4));
        let vol = wait_for_event(
            &mut evt_rx,
            |e| matches!(e, PlayerEvent::State(s) if (s.volume - 0.4).abs() < 1e-6),
            Duration::from_secs(2),
        );
        assert!(vol.is_some(), "volume state must reflect the command");

        send(
            &cmd_tx,
            PlayerCommand::SetEq {
                enabled: true,
                gains_db: [6.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                preamp_db: 0.0,
            },
        );
        // No direct state field for EQ (Builder A's EqSettings owns the UI
        // side); just make sure the command doesn't wedge the engine.
        send(
            &cmd_tx,
            PlayerCommand::QueueReplace(vec![track("ddddddddddd")], Some(0)),
        );
        let playing = wait_for_event(
            &mut evt_rx,
            |e| matches!(e, PlayerEvent::State(s) if s.playing),
            Duration::from_secs(5),
        );
        assert!(playing.is_some(), "engine still plays after EQ changes");

        send(&cmd_tx, PlayerCommand::Stop);
        engine.shutdown();
    }
}
