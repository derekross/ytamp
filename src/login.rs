//! "Sign in with Google" through an embedded browser window.
//!
//! Runs as its own process (`ytamp --login`): WebKitGTK wants a GTK main
//! loop, which cannot share a thread with the egui window's winit loop.
//! The window shows Google's own sign-in page for YouTube Music; once the
//! page lands on music.youtube.com with account cookies in hand, they are
//! written as a Netscape `cookies.txt` for the app and the process exits
//! with status 0. Closing the window first exits with status 1.
//!
//! Google refuses sign-in from browsers it considers embedded; the window
//! therefore identifies itself as Firefox, the way the desktop YouTube
//! Music apps do.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::{PageLoadEvent, WebViewBuilder};

/// Google's sign-in for YouTube Music, returning to the app afterwards.
const LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin?service=youtube&uilel=3&passive=true&continue=https%3A%2F%2Fmusic.youtube.com%2F&hl=en";
/// A desktop Firefox, so the page is not turned away as an embedded browser.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0";
/// The cookie that proves an account is signed in and signs its requests.
const SIGNED_IN_COOKIE: &str = "SAPISID";

enum Notice {
    /// The page started for a URL (the window hides itself for YouTube).
    Heading(String),
    Loaded(String),
}

/// Opens the sign-in window and blocks until it closes. Writes the cookie
/// jar to `cookie_path` on success. Never returns on its own: the event
/// loop exits the process with the outcome as its status.
pub fn run(cookie_path: PathBuf) -> Result<()> {
    let event_loop = EventLoopBuilder::<Notice>::with_user_event().build();
    let window = std::rc::Rc::new(
        WindowBuilder::new()
            .with_title("Sign in to YouTube Music — ytamp")
            .with_inner_size(tao::dpi::LogicalSize::new(560.0, 760.0))
            .build(&event_loop)
            .context("opening the sign-in window")?,
    );
    let proxy = event_loop.create_proxy();
    let load_proxy = proxy.clone();
    let hidden = std::rc::Rc::clone(&window);
    let builder = WebViewBuilder::new()
        .with_url(LOGIN_URL)
        .with_user_agent(USER_AGENT)
        .with_navigation_handler(move |url| {
            // Signed in: the page heads for YouTube Music. Nobody needs
            // to see it; the cookies are read and the window is gone.
            if on_youtube(&url) {
                hidden.set_visible(false);
                let _ = proxy.send_event(Notice::Heading(url));
            }
            true
        })
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, PageLoadEvent::Finished) {
                let _ = load_proxy.send_event(Notice::Loaded(url));
            }
        });
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;
        let vbox = window
            .default_vbox()
            .context("the sign-in window has no GTK box")?;
        builder.build_gtk(vbox)
    }
    .context("creating the sign-in web view")?;
    #[cfg(not(target_os = "linux"))]
    let webview = builder
        .build(&window)
        .context("creating the sign-in web view")?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => std::process::exit(1),
            Event::UserEvent(Notice::Heading(url) | Notice::Loaded(url)) if on_youtube(&url) => {
                match webview.cookies() {
                    Ok(cookies) if signed_in(&cookies) => {
                        match std::fs::write(&cookie_path, netscape_jar(&cookies)) {
                            Ok(()) => std::process::exit(0),
                            Err(error) => {
                                eprintln!(
                                    "ytamp: could not write {}: {error}",
                                    cookie_path.display()
                                );
                                std::process::exit(2);
                            }
                        }
                    }
                    // On YouTube without an account (the page was left
                    // early): show it again so the user can carry on.
                    Ok(_) => window.set_visible(true),
                    Err(error) => eprintln!("ytamp: reading cookies: {error}"),
                }
            }
            _ => {}
        }
    })
}

fn on_youtube(url: &str) -> bool {
    url.starts_with("https://music.youtube.com") || url.starts_with("https://www.youtube.com")
}

fn signed_in(cookies: &[wry::cookie::Cookie<'static>]) -> bool {
    cookies.iter().any(|cookie| {
        cookie.name() == SIGNED_IN_COOKIE
            && cookie
                .domain()
                .is_some_and(|domain| domain.trim_start_matches('.').ends_with("youtube.com"))
    })
}

/// The cookies as a Netscape `cookies.txt`, the format the app and yt-dlp
/// read. Only Google and YouTube domains are kept.
fn netscape_jar(cookies: &[wry::cookie::Cookie<'static>]) -> String {
    let mut out = String::from("# Netscape HTTP Cookie File\n# Written by ytamp --login\n");
    for cookie in cookies {
        // The cookie crate keeps no leading dot for cookies built from
        // WebKit's fields, and every account cookie here is a domain-wide
        // one, so all are written as such (`.youtube.com`, TRUE).
        let Some(bare) = cookie.domain().map(|d| d.trim_start_matches('.')) else {
            continue;
        };
        if !(bare.ends_with("youtube.com") || bare.ends_with("google.com")) {
            continue;
        }
        let expiry = cookie
            .expires_datetime()
            .map(|at| at.unix_timestamp().max(0))
            .unwrap_or(0);
        let prefix = if cookie.http_only().unwrap_or(false) {
            "#HttpOnly_"
        } else {
            ""
        };
        out.push_str(&format!(
            "{prefix}.{bare}\tTRUE\t{}\t{}\t{expiry}\t{}\t{}\n",
            cookie.path().unwrap_or("/"),
            if cookie.secure().unwrap_or(false) {
                "TRUE"
            } else {
                "FALSE"
            },
            cookie.name(),
            cookie.value()
        ));
    }
    out
}

/// Where the jar written by the sign-in window lives: the app's default.
pub fn jar_path(config_dir: &Path) -> PathBuf {
    config_dir.join("cookies.txt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wry::cookie::Cookie;

    #[test]
    fn the_jar_is_netscape_format_for_youtube_and_google_only() {
        let mut sapisid = Cookie::new("SAPISID", "abc");
        sapisid.set_domain(".youtube.com");
        sapisid.set_path("/");
        sapisid.set_secure(true);
        let mut sid = Cookie::new("SID", "xyz");
        sid.set_domain(".google.com");
        sid.set_http_only(true);
        let mut other = Cookie::new("tracker", "1");
        other.set_domain(".example.com");
        let cookies = vec![sapisid, sid, other];
        assert!(signed_in(&cookies));
        let jar = netscape_jar(&cookies);
        assert!(jar.contains(".youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tabc\n"));
        assert!(jar.contains("#HttpOnly_.google.com\tTRUE\t/\tFALSE\t0\tSID\txyz\n"));
        assert!(!jar.contains("example.com"));
        let parsed = crate::yt::innertube_cookies_for_test(&jar);
        assert_eq!(parsed, vec!["SAPISID=abc".to_string()]);
    }

    #[test]
    fn only_youtube_pages_count() {
        assert!(on_youtube("https://music.youtube.com/"));
        assert!(!on_youtube("https://accounts.google.com/signin"));
        let mut foreign = Cookie::new("SAPISID", "x");
        foreign.set_domain(".google.com");
        assert!(!signed_in(&[foreign]));
    }
}
