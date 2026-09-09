//! Parsing InnerTube search / next responses into `Track`s.
//!
//! Pure functions over `serde_json::Value` so they can be tested against
//! captured fixtures (`src/yt/fixtures/*.json`). YouTube nests renderers
//! several levels deep and shuffles the exact wrappers, so lookups walk the
//! tree recursively in document order rather than following one hard-coded
//! path.

use serde_json::Value;

use crate::model::Track;

/// Walk the JSON tree in document order, collecting the value under every
/// occurrence of `key`.
fn find_all<'a>(node: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                if k == key {
                    out.push(v);
                }
                find_all(v, key, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                find_all(item, key, out);
            }
        }
        _ => {}
    }
}

/// Concatenate all `runs[].text` of a text object (`{"runs": [...]}`).
fn runs_text(value: &Value) -> String {
    value
        .get("runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Split a text object's runs into the `•`-separated parts, keeping the
/// trimmed text of each part. YT Music renders "artist • album • duration"
/// as alternating text/separator runs.
fn runs_parts(value: &Value) -> Vec<String> {
    let runs = match value.get("runs").and_then(Value::as_array) {
        Some(runs) => runs,
        None => return Vec::new(),
    };
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    for run in runs {
        let text = run.get("text").and_then(Value::as_str).unwrap_or_default();
        if text.trim() == "•" {
            parts.push(current.trim().to_string());
            current.clear();
        } else {
            current.push_str(text);
        }
    }
    parts.push(current.trim().to_string());
    parts
}

/// Parse `"3:47"` / `"1:02:03"` style durations into seconds.
pub(crate) fn parse_duration(text: &str) -> Option<u64> {
    let secs: Vec<u64> = text
        .trim()
        .split(':')
        .map(|p| p.trim().parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if secs.is_empty() || secs.len() > 3 {
        return None;
    }
    let mut total = 0u64;
    for part in secs {
        total = total * 60 + part;
    }
    Some(total)
}

/// Best (largest) thumbnail URL from a `musicThumbnailRenderer` /
/// `thumbnail` block.
fn best_thumb(value: &Value) -> Option<String> {
    let mut thumbs: Vec<&Value> = Vec::new();
    find_all(value, "thumbnails", &mut thumbs);
    let mut best: Option<(i64, &str)> = None;
    for list in thumbs {
        if let Some(items) = list.as_array() {
            for t in items {
                let url = t.get("url").and_then(Value::as_str);
                let w = t.get("width").and_then(Value::as_i64).unwrap_or(0);
                if let Some(url) = url
                    && best.map(|(bw, _)| w > bw).unwrap_or(true)
                {
                    best = Some((w, url));
                }
            }
        }
    }
    best.map(|(_, url)| url.to_string())
}

/// Extract a video id from the first `watchEndpoint` found under `node`.
fn watch_video_id(node: &Value) -> Option<String> {
    let mut endpoints: Vec<&Value> = Vec::new();
    find_all(node, "watchEndpoint", &mut endpoints);
    endpoints.iter().find_map(|ep| {
        ep.get("videoId")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

/// Parse one `musicResponsiveListItemRenderer` (YT Music search result).
fn parse_search_item(item: &Value) -> Option<Track> {
    // Video id: playlistItemData first, then any watchEndpoint in the item.
    let video_id = item
        .get("playlistItemData")
        .and_then(|d| d.get("videoId"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| watch_video_id(item))?;
    if video_id.is_empty() {
        return None;
    }

    let mut columns: Vec<&Value> = Vec::new();
    find_all(
        item,
        "musicResponsiveListItemFlexColumnRenderer",
        &mut columns,
    );

    let title = columns
        .first()
        .map(|c| runs_text(c.get("text").unwrap_or(&Value::Null)))
        .filter(|t| !t.is_empty())?;

    let mut artist = String::new();
    let mut album: Option<String> = None;
    let mut duration_secs: Option<u64> = None;
    if let Some(second) = columns.get(1) {
        let parts = runs_parts(second.get("text").unwrap_or(&Value::Null));
        for (i, part) in parts.iter().enumerate() {
            if i == 0 {
                artist = part.clone();
            } else if let Some(secs) = parse_duration(part) {
                duration_secs = Some(secs);
            } else if !part.is_empty() {
                album = Some(part.clone());
            }
        }
    }

    let thumb_url = best_thumb(item.get("thumbnail").unwrap_or(&Value::Null));

    Some(Track {
        video_id,
        title,
        artist,
        album,
        duration_secs,
        thumb_url,
    })
}

/// Parse a search response body into up to `limit` tracks (document order,
/// de-duplicated by video id).
pub(crate) fn parse_search_tracks(resp: &Value, limit: usize) -> Vec<Track> {
    let mut items: Vec<&Value> = Vec::new();
    find_all(resp, "musicResponsiveListItemRenderer", &mut items);
    let mut seen = std::collections::HashSet::new();
    let mut tracks = Vec::new();
    for item in items {
        if tracks.len() >= limit {
            break;
        }
        if let Some(track) = parse_search_item(item)
            && seen.insert(track.video_id.clone())
        {
            tracks.push(track);
        }
    }
    tracks
}

/// Parse one `playlistPanelVideoRenderer` (radio / autoplay queue entry).
fn parse_radio_item(item: &Value) -> Option<Track> {
    let video_id = item
        .get("videoId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|id| !id.is_empty())?;

    let title = runs_text(item.get("title").unwrap_or(&Value::Null));
    if title.is_empty() {
        return None;
    }

    // longBylineText renders as "Artist • Album • Year" (album/year optional).
    let parts = runs_parts(item.get("longBylineText").unwrap_or(&Value::Null));
    let artist = parts.first().cloned().unwrap_or_default();
    let album = parts.get(1).filter(|p| !p.is_empty()).cloned();

    let length_text = runs_text(item.get("lengthText").unwrap_or(&Value::Null));
    let duration_secs = parse_duration(&length_text);

    let thumb_url = best_thumb(item.get("thumbnail").unwrap_or(&Value::Null));

    Some(Track {
        video_id,
        title,
        artist,
        album,
        duration_secs,
        thumb_url,
    })
}

/// Parse a `next` (radio) response body into up to `limit` tracks.
pub(crate) fn parse_radio_tracks(resp: &Value, limit: usize) -> Vec<Track> {
    let mut items: Vec<&Value> = Vec::new();
    find_all(resp, "playlistPanelVideoRenderer", &mut items);
    let mut seen = std::collections::HashSet::new();
    let mut tracks = Vec::new();
    for item in items {
        if tracks.len() >= limit {
            break;
        }
        if let Some(track) = parse_radio_item(item)
            && seen.insert(track.video_id.clone())
        {
            tracks.push(track);
        }
    }
    tracks
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH_FIXTURE: &str = include_str!("fixtures/search_daft_punk.json");
    const RADIO_FIXTURE: &str = include_str!("fixtures/next_radio.json");

    #[test]
    fn parses_real_search_fixture() {
        let resp: Value = serde_json::from_str(SEARCH_FIXTURE).expect("fixture JSON");
        let tracks = parse_search_tracks(&resp, 25);
        assert!(tracks.len() >= 10, "got {} tracks", tracks.len());
        let first = &tracks[0];
        assert_eq!(first.video_id, "JhulBGMA7G4");
        assert_eq!(first.title, "Harder, Better, Faster, Stronger");
        assert_eq!(first.artist, "Daft Punk");
        assert_eq!(first.album.as_deref(), Some("Discovery"));
        assert_eq!(first.duration_secs, Some(227));
        assert!(
            first
                .thumb_url
                .as_deref()
                .unwrap_or("")
                .starts_with("https://")
        );
    }

    #[test]
    fn search_limit_is_respected() {
        let resp: Value = serde_json::from_str(SEARCH_FIXTURE).expect("fixture JSON");
        let tracks = parse_search_tracks(&resp, 3);
        assert_eq!(tracks.len(), 3);
        let ids: Vec<&str> = tracks.iter().map(|t| t.video_id.as_str()).collect();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 3, "no duplicate ids");
    }

    #[test]
    fn parses_real_radio_fixture() {
        let resp: Value = serde_json::from_str(RADIO_FIXTURE).expect("fixture JSON");
        let tracks = parse_radio_tracks(&resp, 50);
        assert!(tracks.len() >= 10, "got {} tracks", tracks.len());
        // The seed track is always the first queue entry.
        assert_eq!(tracks[0].video_id, "JhulBGMA7G4");
        assert_eq!(tracks[0].title, "Harder, Better, Faster, Stronger");
        assert_eq!(tracks[0].duration_secs, Some(227));
        assert!(tracks[1].artist.contains("Daft Punk") || !tracks[1].artist.is_empty());
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("3:47"), Some(227));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("0:30"), Some(30));
        assert_eq!(parse_duration("Daft Punk"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn empty_responses_give_no_tracks() {
        assert!(parse_search_tracks(&Value::Null, 10).is_empty());
        assert!(parse_radio_tracks(&serde_json::json!({}), 10).is_empty());
    }
}
