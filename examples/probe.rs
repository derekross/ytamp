//! Live smoke test: resolve one track, play it through the real engine,
//! then seek. YouTube's API and delivery rules change without notice, so
//! this is the quickest way to see whether the native path still works.
//!
//! ```text
//! cargo run --example probe [VIDEO_ID]
//! cargo run --features audio-alsa --example probe   # with sound
//! ```

use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};
use ytamp::model::{PlayerCommand, PlayerEvent, Track};
use ytamp::player::PlayerEngine;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("ytamp=debug"))
        .init();
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "JhulBGMA7G4".into());

    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (evt_tx, mut evt_rx) = broadcast::channel(512);
    // YTAMP_COOKIES=/path/to/cookies.txt runs the probe signed in.
    let cookies =
        std::env::var_os("YTAMP_COOKIES").and_then(|path| std::fs::read_to_string(path).ok());
    let client = ytamp::yt::YtClient::new(cookies);
    println!("signed in: {}", client.has_cookies());
    let engine = PlayerEngine::spawn_with(cmd_rx, evt_tx, client).expect("engine");
    let track = Track {
        video_id: video_id.clone(),
        title: "probe".into(),
        artist: String::new(),
        album: None,
        duration_secs: None,
        thumb_url: None,
    };
    cmd_tx
        .blocking_send(PlayerCommand::QueueReplace(vec![track], Some(0)))
        .expect("engine accepts commands");

    let start = Instant::now();
    let mut seeked = false;
    let mut last_pos = -1.0;
    let mut outcome = "timed out";
    while start.elapsed() < Duration::from_secs(40) {
        match evt_rx.try_recv() {
            Ok(PlayerEvent::State(state)) => {
                if (state.position_secs - last_pos).abs() >= 1.0 {
                    println!(
                        "t+{:>5.1}s playing={} position={:.1}s duration={:?}",
                        start.elapsed().as_secs_f32(),
                        state.playing,
                        state.position_secs,
                        state.duration_secs.map(|d| d.round())
                    );
                    last_pos = state.position_secs;
                }
                if state.playing && state.position_secs > 3.0 && !seeked {
                    println!("-> SeekRatio(0.5)");
                    cmd_tx
                        .blocking_send(PlayerCommand::SeekRatio(0.5))
                        .expect("seek");
                    seeked = true;
                }
                if seeked
                    && let Some(duration) = state.duration_secs
                    && state.position_secs > duration * 0.5 + 3.0
                {
                    outcome = "ok: played, sought, and kept playing";
                    break;
                }
            }
            Ok(PlayerEvent::Error(error)) => {
                println!("ERROR: {error}");
                outcome = "failed";
                break;
            }
            Ok(PlayerEvent::Info(info)) => println!("info: {info}"),
            Ok(PlayerEvent::Spectrum(_)) => {}
            Err(broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {}
        }
    }
    println!("probe {video_id}: {outcome}");
    let _ = cmd_tx.blocking_send(PlayerCommand::Stop);
    engine.shutdown();
}
