//! Builder C: the main window — topbar, search or settings, queue panel,
//! and the now-playing bar. Ported in spirit from fastpotify's `ui::show`.

use egui::{Key, RichText, Slider};

use crate::app::YtampApp;
use crate::app::fmt_time;
use crate::model::PlayerCommand;

/// The whole main window, into the root `Ui` the shell hands us.
pub fn show(app: &mut YtampApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    handle_keys(app, &ctx);

    egui::Panel::top("ytamp-topbar").show(ui, |ui| topbar(app, ui));
    // Added before the side panel so the queue sits above it.
    egui::Panel::bottom("ytamp-now-playing").show(ui, |ui| now_playing(app, ui));
    if app.show_queue {
        egui::Panel::right("ytamp-queue")
            .exact_size(290.0)
            .resizable(false)
            .show(ui, |ui| super::queue::panel(app, ui));
    }
    egui::CentralPanel::default().show(ui, |ui| match app.view {
        crate::app::View::Search => super::search::show(app, ui),
        crate::app::View::Settings => super::settings::show(app, ui),
    });
}

/// Keyboard: Ctrl+F focuses search, Ctrl+Q flips the queue panel, and
/// (when not typing) Space plays, the arrows walk the results, Enter
/// plays the selection. Ctrl+M is handled once, globally, by the app.
fn handle_keys(app: &mut YtampApp, ctx: &egui::Context) {
    let typing = ctx.egui_wants_keyboard_input();
    let mut focus_search = false;
    let mut toggle_queue = false;
    let mut play_pause = false;
    let mut next_result = false;
    let mut previous_result = false;
    let mut play_selected = false;
    ctx.input(|input| {
        for event in &input.events {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            if modifiers.ctrl && !modifiers.shift {
                match key {
                    Key::F => focus_search = true,
                    Key::Q => toggle_queue = true,
                    _ => {}
                }
            } else if modifiers.is_none() && !typing {
                match key {
                    Key::Space => play_pause = true,
                    Key::ArrowDown => next_result = true,
                    Key::ArrowUp => previous_result = true,
                    Key::Enter => play_selected = true,
                    _ => {}
                }
            }
        }
    });
    if focus_search && let Some(id) = app.search.id {
        ctx.memory_mut(|memory| memory.request_focus(id));
    }
    if toggle_queue {
        app.show_queue = !app.show_queue;
    }
    if play_pause {
        app.send_cmd(PlayerCommand::PlayPause);
    }
    let results = app.search.results.len();
    if results > 0 {
        let mut selected = app.search.selected.unwrap_or(0);
        if next_result {
            selected = (selected + 1).min(results - 1);
            app.search.scroll_to = Some(selected);
        }
        if previous_result {
            selected = selected.saturating_sub(1);
            app.search.scroll_to = Some(selected);
        }
        app.search.selected = Some(selected);
        if play_selected {
            app.play_result(selected);
        }
    } else if play_selected {
        app.begin_search();
    }
}

/// Logo, the search box, view tabs, and the mini-player toggle.
fn topbar(app: &mut YtampApp, ui: &mut egui::Ui) {
    ui.horizontal_centered(|ui| {
        ui.label(RichText::new("⚡ ytamp").strong().size(18.0));
        ui.separator();
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.search.query)
                .hint_text("Search YouTube Music…  (Ctrl+F)")
                .desired_width((ui.available_width() - 235.0).max(140.0)),
        );
        app.search.id = Some(response.id);
        if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
            app.begin_search();
            response.request_focus();
        }
        let searched = ui.button("🔍").clicked();
        if app.search.searching {
            ui.spinner();
        }
        ui.separator();
        ui.selectable_value(&mut app.view, crate::app::View::Search, "Search");
        ui.selectable_value(&mut app.view, crate::app::View::Settings, "Settings");
        ui.toggle_value(&mut app.show_queue, "Queue");
        if ui
            .button("⧉ Mini")
            .on_hover_text("Winamp mini player (Ctrl+M)")
            .clicked()
        {
            app.toggle_mini();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if searched {
            app.begin_search();
        }
    });
}

/// Transport, track, scrubbing position bar, and volume.
fn now_playing(app: &mut YtampApp, ui: &mut egui::Ui) {
    ui.horizontal_centered(|ui| {
        ui.style_mut().spacing.item_spacing.x = 4.0;
        let transport = |ui: &mut egui::Ui, label: &str| -> bool {
            ui.add_sized(
                [34.0, 30.0],
                egui::Button::new(RichText::new(label).size(14.0)),
            )
            .clicked()
        };
        if transport(ui, "⏮") {
            app.send_cmd(PlayerCommand::Prev);
        }
        let playing = app.state.playing;
        if transport(ui, if playing { "⏸" } else { "▶" }) {
            app.send_cmd(PlayerCommand::PlayPause);
        }
        if transport(ui, "■") {
            app.send_cmd(PlayerCommand::Stop);
        }
        if transport(ui, "⏭") {
            app.send_cmd(PlayerCommand::Next);
        }

        ui.separator();
        let (title, artist) = match &app.state.track {
            Some(track) => (track.title.clone(), track.artist.clone()),
            None => (
                "Nothing playing".to_owned(),
                "search to fill the queue".to_owned(),
            ),
        };
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
            ui.add(egui::Label::new(RichText::new(artist).weak().size(12.0)).truncate());
        });

        ui.separator();
        let duration = app.state.duration_secs.unwrap_or(0.0);
        let mut position = app.scrub.unwrap_or(app.state.position_secs);
        let clock = format!("{} / {}", fmt_time(position), fmt_time(duration));
        ui.vertical(|ui| {
            let slider =
                egui::Slider::new(&mut position, 0.0..=duration.max(0.1)).show_value(false);
            let response = ui.add_enabled(duration > 0.05, slider);
            if response.changed() {
                app.scrub = Some(position);
            }
            if response.drag_stopped() || response.clicked() {
                let scrub = app
                    .scrub
                    .take()
                    .or_else(|| (duration > 0.05).then_some(position));
                if let Some(scrub) = scrub {
                    let ratio = (scrub / duration).clamp(0.0, 1.0);
                    app.send_cmd(PlayerCommand::SeekRatio(ratio));
                }
            }
            ui.label(RichText::new(clock).weak().monospace().size(11.0));
        });

        ui.separator();
        ui.label("🔊");
        let mut volume = app.settings.volume;
        let changed = ui
            .add_sized(
                [90.0, 16.0],
                Slider::new(&mut volume, 0.0..=1.0).show_value(false),
            )
            .changed();
        if changed {
            app.set_volume(volume);
        }
    });
}
