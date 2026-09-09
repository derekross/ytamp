//! Stream URL resolution: InnerTube `player` client chain → AAC itag
//! selection → small TTL cache → yt-dlp escape hatch (DESIGN.md).

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

use crate::yt::innertube::{ClientCtx, YtClient, client_chain};

/// A resolved, directly fetchable audio stream (DESIGN.md contract).
#[derive(Clone, Debug)]
pub struct StreamUrl {
    pub url: String,
    pub itag: u32,
    pub mime: String,
    pub duration_secs: Option<u64>,
}

/// One candidate format after filtering, for internal selection ranking.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    url: String,
    itag: u32,
    mime: String,
    bitrate: i64,
    duration_secs: Option<u64>,
}

/// Itag preference order: 141 (Premium AAC 256k) → 140 (AAC 128k), then any
/// AAC (`mp4a.*` codec) by bitrate. **AAC only** — our symphonia build has no
/// Opus decoder, so webm/opus itags (249/250/251) are skipped.
const PREFERRED_ITAGS: [u32; 2] = [141, 140];

fn is_aac_mime(mime: &str) -> bool {
    mime.starts_with("audio/") && mime.contains("mp4a.")
}

/// Pick the best directly-fetchable audio format from a `player` response's
/// streaming data. Returns `None` when the client gave nothing usable
/// (SABR-only, LOGIN_REQUIRED, or opus-only).
pub(crate) fn pick_audio_format(resp: &Value) -> Option<Candidate> {
    let streaming = resp.get("streamingData").unwrap_or(&Value::Null);
    let mut formats: Vec<&Value> = Vec::new();
    for key in ["adaptiveFormats", "formats"] {
        if let Some(list) = streaming.get(key).and_then(Value::as_array) {
            formats.extend(list.iter());
        }
    }

    let mut candidates: Vec<Candidate> = formats
        .iter()
        .filter_map(|f| {
            // Direct URL only: entries with just `serverAbrStreamingUrl`
            // (SABR) are unusable without a UMP client.
            let url = f.get("url").and_then(Value::as_str)?;
            let itag = f.get("itag").and_then(Value::as_u64)? as u32;
            let mime = f.get("mimeType").and_then(Value::as_str)?.to_string();
            if !is_aac_mime(&mime) {
                return None;
            }
            Some(Candidate {
                url: url.to_string(),
                itag,
                mime,
                bitrate: f.get("bitrate").and_then(Value::as_i64).unwrap_or(0),
                duration_secs: f
                    .get("approxDurationMs")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|ms| ms / 1000),
            })
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.bitrate));
    candidates
        .iter()
        .position(|c| PREFERRED_ITAGS.contains(&c.itag))
        .map(|i| candidates.swap_remove(i))
        .or_else(|| candidates.into_iter().next())
}

/// Maximum cached stream age. URLs carry an `expire=` param (~6 h); we clamp
/// much tighter to keep IP-bound URLs honest.
const CACHE_TTL: Duration = Duration::from_secs(20 * 60);
/// Cache eviction threshold (entries).
const CACHE_MAX: usize = 256;

fn cache_key(client: &ClientCtx, video_id: &str) -> String {
    format!("{}:{}", client.key, video_id)
}

/// TTL of a URL derived from its `expire=` parameter, clamped to `CACHE_TTL`.
fn url_ttl(url: &str) -> Duration {
    let expire = url
        .split(['&', '?'])
        .find_map(|p| p.strip_prefix("expire="))
        .and_then(|v| v.parse::<u64>().ok());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    match expire {
        Some(e) if e > now => Duration::from_secs((e - now).max(1)).min(CACHE_TTL),
        _ => CACHE_TTL,
    }
}

fn cache_get(client: &YtClient, key: &str) -> Option<StreamUrl> {
    let mut cache = client.stream_cache.lock().ok()?;
    match cache.get(key) {
        Some((stream, cached_at)) if cached_at.elapsed() < url_ttl(&stream.url) => {
            Some(stream.clone())
        }
        Some(_) => {
            cache.remove(key);
            None
        }
        None => None,
    }
}

fn cache_put(client: &YtClient, key: String, stream: &StreamUrl) {
    if let Ok(mut cache) = client.stream_cache.lock() {
        if cache.len() >= CACHE_MAX {
            cache.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
        }
        cache.insert(key, (stream.clone(), Instant::now()));
    }
}

/// Resolve a directly-fetchable AAC stream for `video_id`.
///
/// Client chain (DESIGN.md): `web_music` (when cookies are present) →
/// `android_vr` → `ios` → `tv`; first client that yields a usable format
/// wins, with a small TTL cache keyed `client:video_id`. If every client
/// fails (bot-wall / SABR-only), fall back to a `yt-dlp` subprocess.
pub async fn resolve_stream(client: &YtClient, video_id: &str) -> Result<StreamUrl> {
    let mut attempts: Vec<String> = Vec::new();
    for ctx in client_chain(client.has_cookies()) {
        let key = cache_key(ctx, video_id);
        if let Some(stream) = cache_get(client, &key) {
            log::debug!("resolver cache hit for {key}");
            return Ok(stream);
        }

        match client.player(ctx, video_id).await {
            Ok(resp) => {
                let status = resp
                    .get("playabilityStatus")
                    .and_then(|s| s.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("UNKNOWN");
                match pick_audio_format(&resp) {
                    Some(candidate) => {
                        log::info!(
                            "resolved {} via {} (itag {}, {})",
                            video_id,
                            ctx.key,
                            candidate.itag,
                            candidate.mime
                        );
                        let stream = StreamUrl {
                            url: candidate.url,
                            itag: candidate.itag,
                            mime: candidate.mime,
                            duration_secs: candidate.duration_secs,
                        };
                        cache_put(client, key, &stream);
                        return Ok(stream);
                    }
                    None => attempts.push(format!(
                        "{}: no direct AAC formats (status {status})",
                        ctx.key
                    )),
                }
            }
            Err(e) => attempts.push(format!("{}: {e:#}", ctx.key)),
        }
    }

    // Escape hatch: hand the bleeding edge to yt-dlp.
    log::warn!(
        "no client resolved {video_id} natively ({}); trying yt-dlp",
        attempts.join("; ")
    );
    match yt_dlp_fallback(video_id).await {
        Ok(stream) => Ok(stream),
        Err(e) => Err(anyhow!(
            "no stream for {video_id} ({}). {e:#}",
            attempts.join("; ")
        )),
    }
}

/// `yt-dlp -J` fallback (youtui's proven pattern). Requires the `yt-dlp`
/// binary on PATH; produces a clean error when missing.
async fn yt_dlp_fallback(video_id: &str) -> Result<StreamUrl> {
    let watch_url = format!("https://www.youtube.com/watch?v={video_id}");
    let output = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::process::Command::new("yt-dlp")
            .arg("-J")
            .arg("-f")
            .arg("141/140/bestaudio[ext=m4a]/bestaudio")
            .arg("--no-playlist")
            .arg(&watch_url)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("yt-dlp timed out after 90s"))?
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!("yt-dlp not found on PATH — install it for the fallback resolver (see https://github.com/yt-dlp/yt-dlp)")
        } else {
            anyhow!("failed to launch yt-dlp: {e}")
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "yt-dlp exited {}: {}",
            output.status,
            stderr.lines().last().unwrap_or("(no output)")
        ));
    }
    let info: Value =
        serde_json::from_slice(&output.stdout).context("yt-dlp output was not valid JSON")?;
    let url = info
        .get("url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://"))
        .ok_or_else(|| anyhow!("yt-dlp returned no stream url"))?
        .to_string();
    let itag = info
        .get("format_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let mime = format!(
        "audio/{}",
        info.get("ext").and_then(Value::as_str).unwrap_or("mp4")
    );
    let duration_secs = info
        .get("duration")
        .and_then(Value::as_f64)
        .map(|d| d.round() as u64);
    Ok(StreamUrl {
        url,
        itag,
        mime,
        duration_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PLAYER_IOS: &str = include_str!("fixtures/player_ios.json");
    const PLAYER_SABR: &str = include_str!("fixtures/player_sabr_only.json");
    const PLAYER_ANDROID_VR: &str = include_str!("fixtures/player_android_vr.json");

    #[test]
    fn picks_itag_140_from_real_ios_player_response() {
        let resp: Value = serde_json::from_str(PLAYER_IOS).expect("fixture JSON");
        let pick = pick_audio_format(&resp).expect("a candidate");
        assert_eq!(pick.itag, 140);
        assert!(pick.mime.contains("mp4a."));
        assert!(pick.url.starts_with("https://"));
        assert_eq!(pick.duration_secs, Some(226));
    }

    #[test]
    fn prefers_itag_141_when_present() {
        let mut resp: Value = serde_json::from_str(PLAYER_IOS).expect("fixture JSON");
        let formats = resp
            .get_mut("streamingData")
            .unwrap()
            .get_mut("adaptiveFormats")
            .unwrap()
            .as_array_mut()
            .unwrap();
        // Clone the real itag-140 AUDIO entry and promote it to 141 (the
        // array mixes video and audio formats — index alone can't be
        // trusted to be the audio one).
        let audio140 = formats
            .iter()
            .find(|f| f.get("itag").and_then(Value::as_u64) == Some(140))
            .cloned()
            .expect("fixture has an itag-140 audio format");
        let mut premium = audio140;
        premium["itag"] = json!(141);
        premium["bitrate"] = json!(265000);
        formats.push(premium);
        let pick = pick_audio_format(&resp).expect("a candidate");
        assert_eq!(pick.itag, 141);
    }

    #[test]
    fn skips_opus_and_picks_lowest_aac_when_no_preferred_itag() {
        let resp = json!({
            "streamingData": { "adaptiveFormats": [
                { "itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "url": "https://x/251", "bitrate": 158132, "approxDurationMs": "226413" },
                { "itag": 139, "mimeType": "audio/mp4; codecs=\"mp4a.40.5\"", "url": "https://x/139", "bitrate": 50341, "approxDurationMs": "226413" },
                { "itag": 249, "mimeType": "audio/webm; codecs=\"opus\"", "url": "https://x/249", "bitrate": 63871 }
            ]}
        });
        let pick = pick_audio_format(&resp).expect("aac candidate");
        assert_eq!(pick.itag, 139, "opus must be skipped, aac by bitrate");
    }

    #[test]
    fn sabr_only_response_yields_none() {
        let resp: Value = serde_json::from_str(PLAYER_SABR).expect("fixture JSON");
        assert!(pick_audio_format(&resp).is_none());
    }

    #[test]
    fn login_required_response_yields_none() {
        let resp: Value = serde_json::from_str(PLAYER_ANDROID_VR).expect("fixture JSON");
        assert!(pick_audio_format(&resp).is_none());
    }

    #[test]
    fn url_ttl_clamps_to_cache_cap() {
        let far = "https://x/videoplayback?expire=9999999999&other=1";
        assert_eq!(url_ttl(far), CACHE_TTL);
        let near = format!(
            "https://x/videoplayback?expire={}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 30
        );
        assert!(url_ttl(&near) <= Duration::from_secs(30));
        assert!(!url_ttl("https://x/no-expire").is_zero());
    }

    #[test]
    fn cache_roundtrip() {
        let client = YtClient::new(None);
        let key = "ios:testvid0001".to_string();
        assert!(cache_get(&client, &key).is_none());
        let stream = StreamUrl {
            url: "https://x/videoplayback?expire=9999999999".into(),
            itag: 140,
            mime: "audio/mp4".into(),
            duration_secs: Some(226),
        };
        cache_put(&client, key.clone(), &stream);
        assert_eq!(cache_get(&client, &key).unwrap().itag, 140);
    }

    // ---- live tests (server has internet): `cargo test -- --ignored` ----

    #[tokio::test]
    #[ignore = "hits the real InnerTube API"]
    async fn live_resolve_stream_returns_aac_url() {
        let client = YtClient::new(None);
        let stream = resolve_stream(&client, "JhulBGMA7G4")
            .await
            .expect("resolve");
        assert!(stream.url.starts_with("https://"), "{}", stream.url);
        assert!(
            stream.mime.contains("mp4a."),
            "expected AAC, got {} (itag {})",
            stream.mime,
            stream.itag
        );
        assert!(stream.duration_secs.unwrap_or(0) > 0);
    }
}
