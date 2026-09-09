//! InnerTube transport: client contexts, cookie handling, and the search /
//! next / player endpoints.
//!
//! Client constants extracted from yt-dlp 2026.08.19
//! (`yt_dlp/extractor/youtube/_base.py`, `INNERTUBE_CLIENTS`) — the freshest
//! maintained source as of 2026-09-09. Refresh these when yt-dlp bumps them.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

use crate::model::Track;
use crate::yt::resolver::StreamUrl;
use crate::yt::search;

/// One impersonated InnerTube client.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClientCtx {
    /// Cache/chain key, e.g. `"android_vr"`.
    pub key: &'static str,
    /// Host the requests go to.
    pub host: &'static str,
    /// `context.client.clientName`.
    pub name: &'static str,
    /// `context.client.clientVersion`.
    pub version: &'static str,
    /// `X-YouTube-Client-Name` header.
    pub id: u32,
    /// `User-Agent` header.
    pub ua: &'static str,
    /// Extra `context.client` fields (device identity for mobile clients),
    /// as a JSON object literal. A string because `serde_json::Value` cannot
    /// be built in a `const` — parsed once per request in `context_for`.
    pub device_json: &'static str,
    /// True for music.youtube.com clients (WEB_REMIX).
    pub music: bool,
    /// This client is only tried when cookies are present.
    pub needs_cookies: bool,
}

/// The plain web client: only used to mint a visitor id.
const WEB: ClientCtx = ClientCtx {
    key: "web",
    host: "www.youtube.com",
    name: "WEB",
    version: "2.20260707.00.00",
    id: 1,
    ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    device_json: "",
    music: false,
    needs_cookies: false,
};

/// YT Music web client. Fresh search/next metadata; with cookies also the
/// Premium lane (itag 141 AAC 256k, no PO token per the research doc).
const WEB_MUSIC: ClientCtx = ClientCtx {
    key: "web_music",
    host: "music.youtube.com",
    name: "WEB_REMIX",
    version: "1.20260707.12.00",
    id: 67,
    ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36,gzip(gfe)",
    device_json: "",
    music: true,
    needs_cookies: true,
};

/// Anonymous player client with no PO-token policy in yt-dlp 2026.08.19:
/// its URLs serve the whole file without a token (the others are cut off
/// after the first megabyte). First in the anonymous chain.
const VISIONOS: ClientCtx = ClientCtx {
    key: "visionos",
    host: "www.youtube.com",
    name: "VISIONOS",
    version: "1.02",
    id: 101,
    ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
    device_json: r#"{"deviceMake":"Apple","deviceModel":"RealityDevice17,1","osName":"visionOS","osVersion":"26.5.23O471"}"#,
    music: false,
    needs_cookies: false,
};

/// Anonymous player client. yt-dlp 2026.08.19 marks its streams as
/// needing a GVS PO token; kept in the chain for the networks where it
/// still answers.
const ANDROID_VR: ClientCtx = ClientCtx {
    key: "android_vr",
    host: "www.youtube.com",
    name: "ANDROID_VR",
    version: "1.65.10",
    id: 28,
    ua: "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip",
    device_json: r#"{"deviceMake":"Oculus","deviceModel":"Quest 3","androidSdkVersion":32,"osName":"Android","osVersion":"12L"}"#,
    music: false,
    needs_cookies: false,
};

/// Anonymous player client; observed working from datacenter IPs when
/// android_vr is walled (LOGIN_REQUIRED).
const IOS: ClientCtx = ClientCtx {
    key: "ios",
    host: "www.youtube.com",
    name: "IOS",
    version: "21.26.4",
    id: 5,
    ua: "com.google.ios.youtube/21.26.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
    device_json: r#"{"deviceMake":"Apple","deviceModel":"iPhone16,2","osName":"iPhone","osVersion":"18.3.2.22D82"}"#,
    music: false,
    needs_cookies: false,
};

/// Last anonymous player client in the chain.
const TV: ClientCtx = ClientCtx {
    key: "tv",
    host: "www.youtube.com",
    name: "TVHTML5",
    version: "7.20260707.07.00",
    id: 7,
    ua: "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)",
    device_json: "",
    music: false,
    needs_cookies: false,
};

/// Player-request clients in chain order (DESIGN.md): cookies unlock
/// `web_music` first, then anonymous mobile/TV clients.
const PLAYER_CLIENTS: [&ClientCtx; 5] = [&WEB_MUSIC, &VISIONOS, &ANDROID_VR, &IOS, &TV];

/// The chain for a session: every client when cookies are present, only the
/// anonymous-capable ones otherwise (`needs_cookies` marks the difference).
pub(crate) fn client_chain(has_cookies: bool) -> Vec<&'static ClientCtx> {
    PLAYER_CLIENTS
        .iter()
        .copied()
        .filter(|ctx| has_cookies || !ctx.needs_cookies)
        .collect()
}

/// YT Music search filter for songs (`params` is a percent-encoded protobuf).
const SEARCH_PARAMS_SONGS: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";

/// A parsed Netscape cookie.txt entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cookie {
    pub domain: String,
    pub name: String,
    pub value: String,
    pub expiry: i64,
}

/// Parse Netscape-format `cookies.txt` contents into cookie entries,
/// skipping comments, malformed lines, and entries expired at `now`
/// (expiry `0` means session cookie — kept).
pub(crate) fn parse_netscape_cookies(text: &str, now: i64) -> Vec<Cookie> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        let expiry = fields[4].trim().parse::<i64>().unwrap_or(0);
        if expiry > 0 && expiry < now {
            continue; // expired
        }
        out.push(Cookie {
            domain: fields[0].to_string(),
            name: fields[5].to_string(),
            value: fields[6].to_string(),
            expiry,
        });
    }
    out
}

/// Build a `Cookie:` header value from entries whose domain matches YouTube.
pub(crate) fn cookie_header(cookies: &[Cookie]) -> Option<String> {
    let yt: Vec<&Cookie> = cookies
        .iter()
        .filter(|c| is_youtube_domain(&c.domain))
        .collect();
    if yt.is_empty() {
        return None;
    }
    let pairs: Vec<String> = yt
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect();
    Some(pairs.join("; "))
}

fn is_youtube_domain(domain: &str) -> bool {
    let d = domain.trim_start_matches('.');
    d == "youtube.com" || d == "music.youtube.com" || d.ends_with(".youtube.com")
}

/// InnerTube API client (DESIGN.md contract).
///
/// `YtClient::new(cookies)` takes the *contents* of a Netscape-format
/// `cookies.txt` (Builder C reads the file; we own the parsing). Anonymous by
/// default.
#[derive(Clone)]
pub struct YtClient {
    http: reqwest::Client,
    cookie_header: Option<String>,
    /// Small TTL cache of resolved stream URLs (see resolver).
    pub(crate) stream_cache:
        Arc<Mutex<std::collections::HashMap<String, (StreamUrl, std::time::Instant)>>>,
    /// The session's visitor id (`visitorData`), fetched once and kept for
    /// [`VISITOR_TTL`]. Player requests without it are answered with the
    /// "confirm you're not a bot" wall on ordinary home connections too.
    visitor: Arc<Mutex<Option<(String, std::time::Instant)>>>,
}

/// How long a visitor id is reused before a fresh one is fetched.
const VISITOR_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

impl std::fmt::Debug for YtClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YtClient")
            .field("cookies", &self.cookie_header.is_some())
            .finish()
    }
}

impl YtClient {
    /// Anonymous client. `cookies` is the raw text of a Netscape cookie.txt
    /// export; `None` means anonymous.
    pub fn new(cookies: Option<String>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let header = cookies.as_deref().and_then(|text| {
            let parsed = parse_netscape_cookies(text, now);
            cookie_header(&parsed)
        });
        Self {
            http: Self::build_http(header.is_some()),
            cookie_header: header,
            stream_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
            visitor: Arc::new(Mutex::new(None)),
        }
    }

    /// True when YouTube cookies were supplied and applied.
    pub fn has_cookies(&self) -> bool {
        self.cookie_header.is_some()
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    fn build_http(with_cookies: bool) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .user_agent(WEB_MUSIC.ua)
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10));
        if with_cookies {
            builder = builder.cookie_store(true);
        }
        builder.build().unwrap_or_default()
    }

    /// The cached visitor id, if it is still fresh.
    fn cached_visitor(&self) -> Option<String> {
        let visitor = self.visitor.lock().ok()?;
        visitor
            .as_ref()
            .filter(|(_, at)| at.elapsed() < VISITOR_TTL)
            .map(|(id, _)| id.clone())
    }

    /// A visitor id for this session, fetched from the InnerTube
    /// `visitor_id` endpoint (yt-dlp reads the same value out of the watch
    /// page's `ytcfg`). Failures are logged and leave requests without one.
    pub(crate) async fn visitor_data(&self) -> Option<String> {
        if let Some(id) = self.cached_visitor() {
            return Some(id);
        }
        let body = json!({ "context": Self::context_for(&WEB, None) });
        let id = match self.call_api(&WEB, "visitor_id", body).await {
            Ok(resp) => resp
                .get("responseContext")
                .and_then(|r| r.get("visitorData"))
                .and_then(Value::as_str)
                .map(str::to_string),
            Err(e) => {
                log::warn!("visitor id: {e:#}");
                None
            }
        };
        if let Some(id) = &id
            && let Ok(mut visitor) = self.visitor.lock()
        {
            *visitor = Some((id.clone(), std::time::Instant::now()));
        }
        id
    }

    /// POST one InnerTube request and return the parsed JSON.
    pub(crate) async fn call_api(
        &self,
        ctx: &ClientCtx,
        endpoint: &str,
        body: Value,
    ) -> Result<Value> {
        let url = format!(
            "https://{}/youtubei/v1/{}?prettyPrint=false",
            ctx.host, endpoint
        );
        let mut req = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-YouTube-Client-Name", ctx.id.to_string())
            .header("X-YouTube-Client-Version", ctx.version)
            .header("Origin", format!("https://{}", ctx.host))
            .header("Accept", "*/*");
        // Client-specific UA wins; cookies ride along on every YouTube host.
        req = req.header("User-Agent", ctx.ua);
        if let Some(visitor) = self.cached_visitor() {
            req = req.header("X-Goog-Visitor-Id", visitor);
        }
        if ctx.music {
            req = req.header("Referer", "https://music.youtube.com/");
        }
        if let Some(cookies) = &self.cookie_header {
            req = req.header("Cookie", cookies);
        }
        let resp = req
            .json(&body)
            .send()
            .await
            .with_context(|| format!("{} {endpoint} request failed", ctx.key))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .with_context(|| format!("{} {endpoint}: reading body failed", ctx.key))?;
        if !status.is_success() {
            return Err(anyhow!("{} {endpoint} returned HTTP {status}", ctx.key));
        }
        serde_json::from_str(&text).with_context(|| {
            format!(
                "{} {endpoint}: body is not JSON ({} bytes)",
                ctx.key,
                text.len()
            )
        })
    }

    /// `context` block for a client, with the visitor id when there is one.
    fn context_for(ctx: &ClientCtx, visitor: Option<&str>) -> Value {
        let mut client = json!({
            "clientName": ctx.name,
            "clientVersion": ctx.version,
            "hl": "en",
            "gl": "US",
        });
        if let (Some(visitor), Some(target)) = (visitor, client.as_object_mut()) {
            target.insert("visitorData".into(), Value::String(visitor.to_string()));
        }
        if !ctx.device_json.is_empty()
            && let (Some(target), Ok(Value::Object(src))) = (
                client.as_object_mut(),
                serde_json::from_str::<Value>(ctx.device_json),
            )
        {
            for (k, v) in src {
                target.insert(k, v);
            }
        }
        json!({ "client": client })
    }

    /// Search YT Music for tracks (songs filter).
    pub async fn search_tracks(&self, q: &str, limit: usize) -> Result<Vec<Track>> {
        let body = json!({
            "context": Self::context_for(&WEB_MUSIC, self.cached_visitor().as_deref()),
            "query": q,
            "params": SEARCH_PARAMS_SONGS,
        });
        let resp = self
            .call_api(&WEB_MUSIC, "search", body)
            .await
            .context("YT Music search failed")?;
        let tracks = search::parse_search_tracks(&resp, limit);
        if tracks.is_empty() {
            return Err(anyhow!("no tracks found for {q:?}"));
        }
        Ok(tracks)
    }

    /// The YT Music radio/autoplay queue seeded by a video ("Watch Next" mix).
    pub async fn radio_for(&self, video_id: &str, limit: usize) -> Result<Vec<Track>> {
        let body = json!({
            "context": Self::context_for(&WEB_MUSIC, self.cached_visitor().as_deref()),
            "videoId": video_id,
            "playlistId": format!("RDAMVM{video_id}"),
            "isAudioOnly": true,
        });
        let resp = self
            .call_api(&WEB_MUSIC, "next", body)
            .await
            .context("YT Music radio (next) failed")?;
        let tracks = search::parse_radio_tracks(&resp, limit);
        if tracks.is_empty() {
            return Err(anyhow!("radio for {video_id} returned no tracks"));
        }
        Ok(tracks)
    }

    /// Raw `player` response for one client. Public within the crate for the
    /// resolver.
    pub(crate) async fn player(&self, ctx: &ClientCtx, video_id: &str) -> Result<Value> {
        let visitor = self.visitor_data().await;
        let body = json!({
            "context": Self::context_for(ctx, visitor.as_deref()),
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        self.call_api(ctx, "player", body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_COOKIES: &str = "# Netscape HTTP Cookie File\n\
        #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1893456000\tSID\tabc123\n\
        .youtube.com\tTRUE\t/\tFALSE\t0\tLOGIN_INFO\txyz\n\
        .example.com\tTRUE\t/\tFALSE\t0\tOTHER\tdropped\n\
        .youtube.com\tTRUE\t/\tFALSE\t1000\tOLD\texpired\n";

    #[test]
    fn parses_netscape_cookies() {
        let now = 1_700_000_000;
        let cookies = parse_netscape_cookies(SAMPLE_COOKIES, now);
        // Domain filtering is a cookie_header concern (see the next test);
        // parsing keeps every live YouTube-family entry: the #HttpOnly_
        // prefix is stripped, expired entries are dropped, comments skipped.
        assert_eq!(
            cookies.len(),
            3,
            "httponly kept, expired dropped; domain filtered later"
        );
        assert_eq!(cookies[0].name, "SID");
        assert_eq!(cookies[1].name, "LOGIN_INFO");
        assert_eq!(cookies[2].name, "OTHER");
    }

    #[test]
    fn builds_youtube_cookie_header_in_file_order() {
        let now = 1_700_000_000;
        let cookies = parse_netscape_cookies(SAMPLE_COOKIES, now);
        let header = cookie_header(&cookies).expect("header");
        assert_eq!(header, "SID=abc123; LOGIN_INFO=xyz");
    }

    #[test]
    fn no_youtube_cookies_means_no_header() {
        let cookies = parse_netscape_cookies(".example.com\tTRUE\t/\tFALSE\t0\tA\tb\n", 0);
        assert!(cookie_header(&cookies).is_none());
    }

    #[test]
    fn client_chain_respects_cookies() {
        let anon = client_chain(false);
        assert_eq!(
            anon.iter().map(|c| c.key).collect::<Vec<_>>(),
            vec!["visionos", "android_vr", "ios", "tv"]
        );
        let authed = client_chain(true);
        assert_eq!(authed[0].key, "web_music");
        assert_eq!(authed.len(), 5);
    }
}
