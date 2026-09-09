//! Builder B: YouTube Music InnerTube client + stream resolver.
//!
//! - [`innertube`]: transport (client contexts, cookies, endpoints)
//! - [`search`]: response parsing into [`Track`]s
//! - [`resolver`]: AAC stream URL resolution with client fallback chain

mod innertube;
mod resolver;
mod search;

pub use innertube::YtClient;
pub use resolver::{resolve_stream, StreamUrl};

#[cfg(test)]
mod tests {
    use super::*;

    // ---- live tests (server has internet): `cargo test -- --ignored` ----

    #[tokio::test]
    #[ignore = "hits the real InnerTube API"]
    async fn live_search_daft_punk() {
        let client = YtClient::new(None);
        let tracks = client
            .search_tracks("daft punk", 10)
            .await
            .expect("search works from this network");
        assert!(!tracks.is_empty(), "expected >0 tracks");
        for t in tracks.iter().take(3) {
            assert_eq!(t.video_id.len(), 11, "video id: {}", t.video_id);
            assert!(!t.title.is_empty());
        }
        println!("live_search_daft_punk: {} tracks, first = {}", tracks.len(), tracks[0].display());
    }

    #[tokio::test]
    #[ignore = "hits the real InnerTube API"]
    async fn live_radio_for_seed_track() {
        let client = YtClient::new(None);
        let tracks = client
            .radio_for("JhulBGMA7G4", 10)
            .await
            .expect("radio works from this network");
        assert!(tracks.len() >= 5, "expected a real queue, got {}", tracks.len());
        assert_eq!(tracks[0].video_id, "JhulBGMA7G4");
        println!("live_radio_for_seed_track: {} tracks", tracks.len());
    }
}
