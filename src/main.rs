//! ytamp — YouTube Music, native and skinned.
//!
//! Builder C: the eframe shell. The application state outlives any one
//! window; switching between the main window and the Winamp mini player
//! closes the current window and reopens the other kind, the way
//! fastpotify does (see docs/DESIGN.md).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use clap::Parser;

use ytamp::app::YtampApp;
use ytamp::settings::{self, Settings};

/// YouTube Music, native and skinned.
#[derive(Debug, Parser)]
#[command(name = "ytamp", version, about)]
struct Cli {
    /// A Winamp skin (.wsz) to wear: copied into the library and selected.
    #[arg(long, value_name = "PATH")]
    skin: Option<PathBuf>,

    /// A Netscape-format cookie jar for YouTube Music.
    #[arg(long, value_name = "PATH")]
    cookies: Option<PathBuf>,

    /// Log filter, e.g. `debug,ytamp=trace` (default `warn,ytamp=info`).
    #[arg(long, value_name = "FILTER")]
    log_filter: Option<String>,

    /// Open the Google sign-in window and write the cookie jar, then exit.
    /// The app runs this itself behind "Sign in with Google".
    #[arg(long, hide = true)]
    login: bool,

    /// Open under X11 (XWayland on Wayland desktops), where dropped files
    /// reach the window. Same as the setting, for one run.
    #[arg(long)]
    x11: bool,
}

/// The shared app slot; a poisoned lock is read through, never fatal.
fn lock(slot: &Arc<Mutex<Option<YtampApp>>>) -> MutexGuard<'_, Option<YtampApp>> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The Winamp mini player's window, when that is the window to open.
struct MiniWindow {
    /// A first size; the first frame corrects it once it knows the display.
    size: egui::Vec2,
    position: Option<[f32; 2]>,
    /// Own eframe storage, so the mini window never overwrites the main
    /// window's persisted geometry.
    storage_path: PathBuf,
}

impl MiniWindow {
    fn wanted(app: &YtampApp) -> Option<Self> {
        app.settings.winamp_window.then(|| Self {
            size: ytamp::ui::winamp::window_size(&app.mini.winamp),
            position: app.mini.pos.restore,
            storage_path: settings::cache_dir().join("winamp.ron"),
        })
    }
}

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();
    let filter = cli
        .log_filter
        .clone()
        .unwrap_or_else(|| "warn,ytamp=info".into());
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter)).init();

    if let Err(error) = settings::ensure_config_dirs() {
        log::warn!("unable to create the config directories: {error}");
    }
    if cli.login {
        #[cfg(feature = "login-webview")]
        if let Err(error) = ytamp::login::run(settings::default_cookie_path()) {
            eprintln!("ytamp --login: {error:#}");
            std::process::exit(2);
        }
        #[cfg(not(feature = "login-webview"))]
        {
            eprintln!("ytamp was built without the login-webview feature");
            std::process::exit(2);
        }
    }
    let settings_path = settings::config_dir().join("settings.json");
    let mut settings = Settings::load(&settings_path);
    if let Some(cookies) = &cli.cookies {
        settings.cookie_path = Some(cookies.display().to_string());
    }

    let force_x11 = cli.x11 || settings.force_x11;
    let mut app =
        YtampApp::new(settings, settings_path, settings::skins_dir()).unwrap_or_else(|error| {
            eprintln!("ytamp: could not start: {error}");
            std::process::exit(1);
        });
    if let Some(skin) = &cli.skin {
        app.install_skin(skin);
    }

    // The application outlives any one window: toggling modes closes one
    // kind of window (main <-> mini), the state comes back to the slot, and
    // this loop opens the other kind. A plain close quits the app.
    let slot = Arc::new(Mutex::new(Some(app)));
    loop {
        let creator_slot = Arc::clone(&slot);
        let mini = lock(&slot).as_ref().and_then(MiniWindow::wanted);
        let mini_window = mini.is_some();
        let options = native_options(mini, force_x11);
        eframe::run_native(
            "ytamp",
            options,
            Box::new(move |cc| {
                let mut app = lock(&creator_slot)
                    .take()
                    .expect("application state present");
                app.attach(&cc.egui_ctx);
                Ok(Box::new(Shell {
                    app: Some(app),
                    slot: creator_slot,
                    mini_window,
                }))
            }),
        )?;

        let switch = lock(&slot).as_ref().is_some_and(|app| app.switch_intent);
        if switch {
            continue; // straight back round: the other kind of window opens
        }
        if let Some(mut app) = lock(&slot).take() {
            app.save_settings();
        }
        break;
    }
    Ok(())
}

fn native_options(mini: Option<MiniWindow>, force_x11: bool) -> eframe::NativeOptions {
    // The mini player keeps its own geometry; its closing window must not
    // replace the main window's persisted size, and vice versa.
    let persist_window = mini.is_none();
    let persistence_path = mini.as_ref().map(|mini| mini.storage_path.clone());
    let viewport = egui::ViewportBuilder::default()
        .with_title("ytamp")
        .with_app_id("ytamp")
        .with_icon(app_icon());
    let viewport = match mini {
        Some(mini) => {
            // See-through for skins that are not rectangles: the skin
            // paints every pixel that is the window.
            let viewport = viewport
                .with_decorations(false)
                .with_transparent(true)
                .with_resizable(false)
                .with_maximize_button(false)
                .with_inner_size(mini.size)
                .with_min_inner_size(mini.size)
                .with_max_inner_size(mini.size);
            match mini.position {
                Some([x, y]) => viewport.with_position([x, y]),
                None => viewport,
            }
        }
        None => viewport
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([720.0, 520.0]),
    };
    // winit 0.30 delivers dropped files on X11 and not on Wayland; a
    // Wayland desktop with XWayland can opt in.
    let event_loop_builder: Option<eframe::EventLoopBuilderHook> = force_x11.then(|| {
        Box::new(
            |builder: &mut eframe::EventLoopBuilder<eframe::UserEvent>| {
                use winit::platform::x11::EventLoopBuilderExtX11 as _;
                builder.with_x11();
            },
        ) as eframe::EventLoopBuilderHook
    });
    eframe::NativeOptions {
        viewport,
        persist_window,
        persistence_path,
        event_loop_builder,
        // A Wayland compositor stops sending frame callbacks to a hidden
        // window; waiting for vsync there would block the event loop.
        // Repaints are event-driven, so nothing spins. (fastpotify's call.)
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: false,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The eframe adapter around the long-lived [`YtampApp`]: delegates frames
/// and, when the window goes away, hands the state back for the next one.
struct Shell {
    app: Option<YtampApp>,
    slot: Arc<Mutex<Option<YtampApp>>>,
    /// The mode this window opened in, even after an action switches modes.
    mini_window: bool,
}

impl eframe::App for Shell {
    fn persist_egui_memory(&self) -> bool {
        !self.mini_window
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(app) = self.app.as_mut() {
            app.background_frame(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(app) = self.app.as_mut() {
            app.frame_ui(ui);
        }
    }

    /// The mini player's window is see-through where the skin leaves it
    /// out; the big window paints itself over eframe's own ground.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self
            .app
            .as_ref()
            .is_some_and(|app| app.settings.winamp_window)
        {
            [0.0; 4]
        } else {
            [0.055, 0.053, 0.07, 1.0]
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(app) = self.app.as_mut() {
            app.save_settings();
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        *lock(&self.slot) = self.app.take();
    }
}

/// The window icon: a purple field with a white bolt, drawn in code —
/// no asset to carry, no license to keep track of.
fn app_icon() -> egui::IconData {
    const SIZE: usize = 64;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let center = SIZE as f32 / 2.0;
    let radius = SIZE as f32 * 0.42;
    for (index, chunk) in rgba.chunks_exact_mut(4).enumerate() {
        let (x, y) = (index % SIZE, index / SIZE);
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let distance = ((fx - center).powi(2) + (fy - center).powi(2)).sqrt();
        let (r, g, b, a) = if distance > radius {
            (0, 0, 0, 0)
        } else {
            // A diagonal bolt across the anti-diagonal band.
            let along = fx + fy;
            let bolt = (fx - fy).abs() < 6.0 && (34.0..62.0).contains(&along);
            let edge = distance > radius - 1.5;
            let (r, g, b) = if bolt {
                (0xE7, 0xE5, 0xF0)
            } else if edge {
                (0x5B, 0x40, 0x99)
            } else {
                (0x8B, 0x5C, 0xF6)
            };
            (r, g, b, 255)
        };
        chunk.copy_from_slice(&[r, g, b, a]);
    }
    egui::IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}
