//! The Library view: liked songs, library songs, and playlists of the
//! signed-in account. Tracks list through the search view's results panel.

use egui::RichText;

use crate::app::{LibraryTab, YtampApp};

pub fn show(app: &mut YtampApp, ui: &mut egui::Ui) {
    if !app.yt.has_cookies() {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            ui.heading(RichText::new("Your library").strong());
            ui.label(
                RichText::new(
                    "Sign in (Settings) to see your liked songs, library, and playlists.",
                )
                .weak(),
            );
        });
        return;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let tab = app.library.tab;
        if ui
            .selectable_label(tab == LibraryTab::Liked, "Liked songs")
            .clicked()
        {
            app.begin_liked();
        }
        if ui
            .selectable_label(tab == LibraryTab::Songs, "Library songs")
            .clicked()
        {
            app.begin_library_songs();
        }
        if ui
            .selectable_label(tab == LibraryTab::Playlists, "Playlists")
            .clicked()
        {
            app.begin_playlists();
        }
        if app.library.loading_playlists {
            ui.spinner();
        }
    });
    ui.separator();
    if app.library.tab == LibraryTab::Playlists {
        egui::Panel::left("ytamp-playlists")
            .default_size(260.0)
            .resizable(true)
            .show(ui, |ui| playlists(app, ui));
        if app.library.open_playlist.is_some() {
            super::search::results(app, ui);
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(RichText::new("Pick a playlist").weak());
            });
        }
    } else {
        super::search::results(app, ui);
    }
}

/// The playlist list; a click lists its tracks.
fn playlists(app: &mut YtampApp, ui: &mut egui::Ui) {
    let playlists = app.library.playlists.clone();
    let open = app.library.open_playlist.clone();
    if playlists.is_empty() && !app.library.loading_playlists {
        ui.label(RichText::new("No playlists yet.").weak().small());
        return;
    }
    let mut chosen = None;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for playlist in &playlists {
                let current = open.as_deref() == Some(playlist.browse_id.as_str());
                let label = if playlist.subtitle.is_empty() {
                    playlist.title.clone()
                } else {
                    format!("{}\n{}", playlist.title, playlist.subtitle)
                };
                if ui
                    .selectable_label(current, RichText::new(label).small())
                    .clicked()
                {
                    chosen = Some(playlist.clone());
                }
            }
        });
    if let Some(playlist) = chosen {
        app.open_playlist(&playlist);
    }
}
