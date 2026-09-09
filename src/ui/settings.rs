//! Builder C: the settings view — skins, cookies, equalizer, about.

use egui::{RichText, Slider};

use crate::app::{self, YtampApp};
use crate::settings::{EQ_BAND_CENTERS_HZ, default_cookie_path, eq_band_label};

/// The settings page, one section at a time.
pub fn show(app: &mut YtampApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading(RichText::new("Settings").strong());
            ui.separator();
            skins(app, ui);
            ui.add_space(12.0);
            cookies(app, ui);
            ui.add_space(12.0);
            equalizer(app, ui);
            ui.add_space(12.0);
            about(app, ui);
            ui.add_space(12.0);
        });
}

/// Mini player: skin library, scale, and the door to mini mode.
fn skins(app: &mut YtampApp, ui: &mut egui::Ui) {
    ui.label(RichText::new("Mini player & skins").strong());
    ui.add_space(4.0);
    let mut chosen = app.settings.skin.clone();
    ui.radio_value(&mut chosen, None, "Built-in look");
    for (name, _path) in app.list_skins() {
        ui.radio_value(
            &mut chosen,
            Some(name.clone()),
            app::skin_display_label(&name),
        );
    }
    if chosen != app.settings.skin {
        match chosen {
            Some(name) => app.load_skin_by_name(&name),
            None => app.clear_skin(),
        }
    }
    ui.add_space(4.0);
    let mut scale = app.settings.skin_scale;
    ui.add(Slider::new(&mut scale, 1..=4).text("skin scale"));
    if scale != app.settings.skin_scale {
        app.settings.skin_scale = scale;
        app.mini.winamp.scale = u32::from(scale.clamp(1, 4));
        app.mark_dirty();
    }
    ui.add_space(4.0);
    if ui.button("Open mini player  (Ctrl+M)").clicked() {
        app.toggle_mini();
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Import a skin").strong().small());
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(&mut app.skin_url)
                .hint_text("Paste a Skin Museum link or a .wsz URL")
                .desired_width((ui.available_width() - 90.0).max(120.0))
                .font(egui::TextStyle::Small),
        );
        let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if (ui.button("Import").clicked() || entered) && !app.skin_url.trim().is_empty() {
            let url = app.skin_url.trim().to_string();
            app.import_skin_url(&url);
            app.skin_url.clear();
        }
    });
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Thousands of classic skins live at")
                .weak()
                .small(),
        );
        ui.hyperlink_to(
            RichText::new("skins.webamp.org").small(),
            "https://skins.webamp.org/",
        );
    });
    ui.label(
        RichText::new(format!(
            "Skin library: {} — drop a .wsz anywhere to install.",
            app.skins_dir.display()
        ))
        .weak()
        .small(),
    );
}

/// The optional Netscape cookie jar for account-level streams.
fn cookies(app: &mut YtampApp, ui: &mut egui::Ui) {
    ui.label(RichText::new("YouTube Music cookies").strong());
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Optional: a Netscape-format cookie jar exported from your browser. \
             With it you get your account — Premium-rate streams included.",
        )
        .weak()
        .small(),
    );
    ui.add_space(4.0);
    let mut path = app.settings.cookie_path.clone().unwrap_or_default();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut path)
                .hint_text("path to cookies.txt")
                .desired_width((ui.available_width() - 200.0).max(120.0))
                .font(egui::TextStyle::Small),
        );
        if ui.button("Default").clicked() {
            path = default_cookie_path().display().to_string();
        }
        if ui.button("Clear").clicked() {
            path.clear();
        }
        if ui.button("Apply").clicked() {
            let trimmed = path.trim().to_owned();
            let next = (!trimmed.is_empty()).then_some(trimmed);
            app.set_cookie_path(next);
        }
    });
    let status = app.settings.cookie_path.as_deref().map(|cookie_path| {
        std::fs::read_to_string(cookie_path)
            .map(|text| !text.trim().is_empty())
            .unwrap_or(false)
    });
    match status {
        Some(true) => {
            ui.label(
                RichText::new("Cookies loaded.")
                    .small()
                    .color(egui::Color32::from_rgb(0x7C, 0xD9, 0x92)),
            );
        }
        Some(false) => {
            ui.label(
                RichText::new("Cookie file missing or empty.")
                    .small()
                    .color(egui::Color32::from_rgb(0xE8, 0x6A, 0x5A)),
            );
        }
        None => {}
    }
}

/// The ten-band Winamp curve; the mini player's EQ panel shares it.
fn equalizer(app: &mut YtampApp, ui: &mut egui::Ui) {
    ui.label(RichText::new("Equalizer").strong());
    ui.add_space(4.0);
    let mut eq = app.settings.eq;
    ui.horizontal(|ui| {
        ui.checkbox(&mut eq.enabled, "Enable");
        ui.label(RichText::new("Preamp").weak().small());
        ui.add_sized(
            [170.0, 16.0],
            Slider::new(&mut eq.preamp_db, -12.0..=12.0).show_value(true),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for (band, center) in EQ_BAND_CENTERS_HZ.iter().enumerate() {
            ui.vertical(|ui| {
                ui.add(
                    egui::Slider::new(&mut eq.gains_db[band], -12.0..=12.0)
                        .vertical()
                        .show_value(false),
                );
                ui.label(RichText::new(eq_band_label(*center)).weak().small());
            });
        }
    });
    if eq != app.settings.eq {
        app.settings.eq = eq;
        app.mark_dirty();
        app.send_eq();
    }
}

/// Versions, paths, and the engine's state of grace.
fn about(app: &mut YtampApp, ui: &mut egui::Ui) {
    ui.label(RichText::new("About").strong());
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "ytamp {} — YouTube Music, native and skinned.",
            env!("CARGO_PKG_VERSION")
        ))
        .weak()
        .small(),
    );
    ui.label(
        RichText::new(format!(
            "Config: {}",
            crate::settings::config_dir().display()
        ))
        .weak()
        .small(),
    );
    if let Some(note) = &app.engine_note {
        ui.label(
            RichText::new(format!("Audio engine: {note}"))
                .weak()
                .small()
                .color(egui::Color32::from_rgb(0xE0, 0xB0, 0x50)),
        );
    }
}
