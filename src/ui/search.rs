//! Builder C: search results and the first-run landing view.

use egui::{Align, Color32, CornerRadius, Layout, RichText, vec2};

use crate::app::YtampApp;
use crate::app::fmt_time;
use crate::model::Track;

/// The search view: a landing page before the first query, then results.
pub fn show(app: &mut YtampApp, ui: &mut egui::Ui) {
    let committed = app.search.committed.clone();
    let searching = app.search.searching;
    if committed.is_empty() && !searching {
        landing(app, ui);
        return;
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.heading(RichText::new(format!("“{committed}”")).strong());
        if searching {
            ui.spinner();
            ui.label(RichText::new("searching…").weak());
        }
    });
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);

    let results = app.search.results.clone();
    if results.is_empty() {
        if !searching {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(RichText::new(format!("No results for “{committed}”")).weak());
            });
        }
        return;
    }

    let selected = app.search.selected;
    let scroll_to = app.search.scroll_to;
    let mut clicked = None;
    let mut double_clicked = None;
    let mut play_clicked = None;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (index, track) in results.iter().enumerate() {
                let row_selected = selected == Some(index);
                let (rect, response) =
                    ui.allocate_at_least(vec2(ui.available_width(), 56.0), egui::Sense::click());
                if Some(index) == scroll_to {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
                let background = if row_selected {
                    Some(Color32::from_rgb(0x5B, 0x40, 0x99))
                } else if response.hovered() {
                    Some(Color32::from_rgb(0x24, 0x22, 0x33))
                } else {
                    None
                };
                if let Some(background) = background {
                    ui.painter()
                        .rect_filled(rect.shrink(1.0), CornerRadius::same(5), background);
                }

                let mut row = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                row.add_space(8.0);
                if let Some(url) = &track.thumb_url {
                    row.allocate_ui_with_layout(
                        vec2(44.0, 44.0),
                        Layout::centered_and_justified(egui::Direction::LeftToRight),
                        |ui| {
                            ui.add(egui::Image::new(url.as_str()).fit_to_fraction(vec2(1.0, 1.0)));
                        },
                    );
                    row.add_space(8.0);
                }
                row.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(&track.title).strong()).truncate());
                    ui.add(
                        egui::Label::new(RichText::new(meta_line(track)).weak().size(12.0))
                            .truncate(),
                    );
                });
                row.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(8.0);
                    if ui
                        .add_sized(
                            [30.0, 30.0],
                            egui::Button::new(RichText::new("▶").size(14.0)),
                        )
                        .on_hover_text("Play, and start a radio after it")
                        .clicked()
                    {
                        play_clicked = Some(index);
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            track
                                .duration_secs
                                .map(|secs| fmt_time(secs as f64))
                                .unwrap_or_default(),
                        )
                        .weak()
                        .monospace()
                        .size(12.0),
                    );
                });

                if response.double_clicked() {
                    double_clicked = Some(index);
                } else if response.clicked() {
                    clicked = Some(index);
                }
            }
        });
    if let Some(index) = play_clicked.or(double_clicked) {
        app.play_result(index);
    } else if let Some(index) = clicked {
        app.search.selected = Some(index);
    }
    if scroll_to.is_some() {
        app.search.scroll_to = None;
    }
}

/// Artist, then album when there is one.
fn meta_line(track: &Track) -> String {
    let mut line = track.artist.clone();
    if let Some(album) = &track.album
        && !album.is_empty()
    {
        line.push_str(" · ");
        line.push_str(album);
    }
    line
}

/// The view before any query: what this is, and where you have been.
fn landing(app: &mut YtampApp, ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.heading(RichText::new("⚡ ytamp").strong().size(28.0));
        ui.label(RichText::new("YouTube Music, native and skinned.").weak());
        ui.add_space(16.0);
        ui.label(
            RichText::new(
                "Type above and press Enter, then ▶ or a double-click plays a song and a radio follows.",
            )
            .weak()
            .size(12.5),
        );
        ui.label(
            RichText::new("Ctrl+M opens the Winamp mini player · drop a .wsz anywhere to wear it.")
                .weak()
                .size(12.5),
        );
    });

    let history = app.settings.search_history.clone();
    if !history.is_empty() {
        ui.add_space(20.0);
        ui.label(RichText::new("Recent").weak());
        ui.add_space(4.0);
        let mut picked: Option<String> = None;
        ui.horizontal_wrapped(|ui| {
            for query in &history {
                if ui.button(query).clicked() {
                    picked = Some(query.clone());
                }
            }
        });
        if let Some(query) = picked {
            app.search.query = query;
            app.begin_search();
        }
    }
}
