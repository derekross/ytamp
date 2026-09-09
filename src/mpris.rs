// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.

//! Linux desktop media controls (MPRIS): media keys, the shell's player
//! widget, `playerctl`.
//!
//! D-Bus runs on its own thread with a local executor and exchanges
//! bounded messages with the app, which stays the only owner of playback
//! decisions. A slow or absent session bus therefore cannot stall audio
//! or the window.

use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use mpris_server::{Metadata, PlaybackStatus, Player, Time, TrackId};
use tokio::sync::mpsc as tokio_mpsc;

use crate::model::{PlaybackState, Track};

const PLAYING_POSITION_INTERVAL: Duration = Duration::from_millis(1000);
const TRACK_OBJECT_PATH_PREFIX: &str = "/org/ytamp/Track/";

/// What the desktop asked of the player.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Seek relative to the position, in milliseconds.
    SeekBy(i64),
    /// Seek to an absolute position, in milliseconds.
    SetPosition(u64),
    /// Volume 0..=1.
    SetVolume(f64),
    /// Bring the window forward.
    Raise,
    Quit,
}

/// What the desktop is told, distilled from [`PlaybackState`].
#[derive(Clone, Debug, PartialEq)]
pub struct MediaState {
    pub status: PlaybackStatus,
    pub track: Option<Track>,
    pub position_ms: u64,
    pub volume: f64,
}

impl MediaState {
    pub fn of(state: &PlaybackState) -> Self {
        Self {
            status: if state.playing {
                PlaybackStatus::Playing
            } else if state.track.is_some() {
                PlaybackStatus::Paused
            } else {
                PlaybackStatus::Stopped
            },
            track: state.track.clone(),
            position_ms: (state.position_secs.max(0.0) * 1000.0) as u64,
            volume: f64::from(state.volume),
        }
    }
}

enum Update {
    State(MediaState),
    Seeked(u64),
}

pub struct MediaService {
    updates: tokio_mpsc::UnboundedSender<Update>,
    commands: Receiver<MediaCommand>,
    published: Option<MediaState>,
    last_position_update: Instant,
}

impl MediaService {
    /// Starts the D-Bus thread. `wake` is called whenever a command
    /// arrives, so the window repaints and drains it.
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (updates, update_rx) = tokio_mpsc::unbounded_channel();
        let (command_tx, commands) = std::sync::mpsc::channel();
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(wake);
        let spawned = thread::Builder::new()
            .name("ytamp-mpris".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        log::warn!("MPRIS runtime unavailable: {error}");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                let outcome = local.block_on(&runtime, run(update_rx, command_tx, wake));
                if let Err(error) = outcome {
                    log::warn!("MPRIS is unavailable: {error}");
                }
            });
        if let Err(error) = spawned {
            log::warn!("unable to start the MPRIS thread: {error}");
        }
        Self {
            updates,
            commands,
            published: None,
            last_position_update: Instant::now() - PLAYING_POSITION_INTERVAL,
        }
    }

    pub fn drain_commands(&self) -> Vec<MediaCommand> {
        self.commands.try_iter().collect()
    }

    /// Publishes structural changes immediately; the position once a
    /// second while playing, since clients interpolate between updates.
    pub fn update(&mut self, state: MediaState) {
        let structural = self
            .published
            .as_ref()
            .is_none_or(|published| !same_except_position(published, &state));
        let position_due = state.status != PlaybackStatus::Playing
            || self.last_position_update.elapsed() >= PLAYING_POSITION_INTERVAL;
        let position_changed = self
            .published
            .as_ref()
            .is_none_or(|published| published.position_ms != state.position_ms);
        if !structural && (!position_changed || !position_due) {
            return;
        }
        if self.updates.send(Update::State(state.clone())).is_ok() {
            self.published = Some(state);
            self.last_position_update = Instant::now();
        }
    }

    /// Tells the desktop the position jumped (a seek), so it stops
    /// interpolating from the old one.
    pub fn seeked(&self, position_ms: u64) {
        let _ = self.updates.send(Update::Seeked(position_ms));
    }
}

fn same_except_position(left: &MediaState, right: &MediaState) -> bool {
    left.status == right.status
        && left.track == right.track
        && (left.volume - right.volume).abs() < 0.005
}

async fn run(
    mut updates: tokio_mpsc::UnboundedReceiver<Update>,
    commands: Sender<MediaCommand>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> mpris_server::zbus::Result<()> {
    let player = Player::builder("ytamp")
        .identity("ytamp")
        .desktop_entry("ytamp")
        .can_raise(true)
        .can_quit(true)
        .can_control(true)
        .can_play(true)
        .can_pause(true)
        .can_go_next(true)
        .can_go_previous(true)
        .can_seek(true)
        .build()
        .await?;

    let send = {
        let commands = commands.clone();
        let wake = wake.clone();
        move |command: MediaCommand| {
            if commands.send(command).is_ok() {
                wake();
            }
        }
    };
    {
        let send = send.clone();
        player.connect_play(move |_| send(MediaCommand::Play));
    }
    {
        let send = send.clone();
        player.connect_pause(move |_| send(MediaCommand::Pause));
    }
    {
        let send = send.clone();
        player.connect_play_pause(move |_| send(MediaCommand::PlayPause));
    }
    {
        let send = send.clone();
        player.connect_stop(move |_| send(MediaCommand::Stop));
    }
    {
        let send = send.clone();
        player.connect_next(move |_| send(MediaCommand::Next));
    }
    {
        let send = send.clone();
        player.connect_previous(move |_| send(MediaCommand::Previous));
    }
    {
        let send = send.clone();
        player.connect_seek(move |_, offset| send(MediaCommand::SeekBy(offset.as_millis())));
    }
    {
        let send = send.clone();
        player.connect_set_position(move |_, _track_id, position| {
            send(MediaCommand::SetPosition(position.as_millis().max(0) as u64));
        });
    }
    {
        let send = send.clone();
        player.connect_set_volume(move |_, volume| send(MediaCommand::SetVolume(volume)));
    }
    {
        let send = send.clone();
        player.connect_raise(move |_| send(MediaCommand::Raise));
    }
    {
        let send = send.clone();
        player.connect_quit(move |_| send(MediaCommand::Quit));
    }

    let server = player.run();
    let apply = async {
        let mut published: Option<MediaState> = None;
        while let Some(update) = updates.recv().await {
            match update {
                Update::Seeked(position_ms) => {
                    let _ = player.seeked(Time::from_millis(position_ms as i64)).await;
                }
                Update::State(state) => {
                    let previous = published.as_ref();
                    if previous.is_none_or(|p| p.status != state.status) {
                        let _ = player.set_playback_status(state.status).await;
                    }
                    if previous.is_none_or(|p| p.track != state.track) {
                        let _ = player.set_metadata(metadata(state.track.as_ref())).await;
                        let _ = player.set_can_seek(state.track.is_some()).await;
                    }
                    if previous.is_none_or(|p| (p.volume - state.volume).abs() >= 0.005) {
                        let _ = player.set_volume(state.volume).await;
                    }
                    player.set_position(Time::from_millis(state.position_ms as i64));
                    published = Some(state);
                }
            }
        }
    };
    tokio::select! {
        _ = server => {}
        _ = apply => {}
    }
    Ok(())
}

fn metadata(track: Option<&Track>) -> Metadata {
    let Some(track) = track else {
        return Metadata::new();
    };
    let mut builder = Metadata::builder().title(track.title.clone()).url(format!(
        "https://music.youtube.com/watch?v={}",
        track.video_id
    ));
    if let Some(track_id) = object_path_for(&track.video_id) {
        builder = builder.trackid(track_id);
    }
    if let Some(secs) = track.duration_secs {
        builder = builder.length(Time::from_secs(secs as i64));
    }
    if !track.artist.is_empty() {
        builder = builder.artist(vec![track.artist.clone()]);
    }
    if let Some(album) = track.album.as_ref().filter(|a| !a.is_empty()) {
        builder = builder.album(album.clone());
    }
    if let Some(art) = &track.thumb_url {
        builder = builder.art_url(art.clone());
    }
    builder.build()
}

/// A D-Bus object path for a video id: only letters, digits and `_` are
/// allowed, so `-` becomes `_h` and `_` becomes `_u`, which keeps it
/// unique.
fn object_path_for(video_id: &str) -> Option<TrackId> {
    let mut id = String::with_capacity(video_id.len() + 4);
    for c in video_id.chars() {
        match c {
            '-' => id.push_str("_h"),
            '_' => id.push_str("_u"),
            c if c.is_ascii_alphanumeric() => id.push(c),
            _ => {}
        }
    }
    if id.is_empty() {
        id.push_str("track");
    }
    TrackId::try_from(format!("{TRACK_OBJECT_PATH_PREFIX}{id}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ids_make_valid_object_paths() {
        let path = object_path_for("a-B_c9").unwrap();
        assert_eq!(path.as_str(), "/org/ytamp/Track/a_hB_uc9");
        assert!(object_path_for("---").is_some());
    }

    #[test]
    fn media_state_reads_the_playback_state() {
        let mut state = PlaybackState::default();
        assert_eq!(MediaState::of(&state).status, PlaybackStatus::Stopped);
        state.track = Some(Track {
            video_id: "x".into(),
            title: "t".into(),
            artist: String::new(),
            album: None,
            duration_secs: None,
            thumb_url: None,
        });
        state.position_secs = 1.5;
        assert_eq!(MediaState::of(&state).status, PlaybackStatus::Paused);
        assert_eq!(MediaState::of(&state).position_ms, 1500);
        state.playing = true;
        assert_eq!(MediaState::of(&state).status, PlaybackStatus::Playing);
    }
}
