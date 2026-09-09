// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.

//! The playlist window, joined to the bottom of the main one.
//!
//! Uses `pledit.bmp` for the frame and `pledit.txt` for the list's colours.
//! It shows the queue; double-clicking a row plays it. Where fastpotify
//! drove Spotify's queue through its `Action` queue, ytamp reads the
//! host's [`PlaybackState`](crate::model::PlaybackState) and sends
//! [`PlayerCommand`]s.

use egui::{Color32, Sense};

use crate::model::PlayerCommand;
use crate::skin::layout::{self, Area};
use crate::skin::sprites;
use crate::winamp::{WinampHost, WinampState};

use super::{View, format_duration};

/// One line of the list.
struct Row {
    /// Where the song sits in the queue.
    index: usize,
    label: String,
    duration_secs: Option<u64>,
    current: bool,
}

/// The whole panel, drawn into a view whose origin is the panel's top left.
pub(super) fn show(
    view: &mut View,
    state: &mut WinampState,
    host: &mut dyn WinampHost,
    focused: bool,
) {
    if state.playlist_shaded {
        shade(view, state, host, focused);
        return;
    }
    let height = state.playlist_height.max(layout::PLAYLIST_MIN_HEIGHT);
    frame(view, height, focused);

    let title = view.interact(
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::PLAYLIST_TITLE_HEIGHT),
        "playlist-title",
        Sense::click_and_drag(),
    );
    if title.drag_started() {
        view.ui
            .ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        state.playlist_shaded = true;
    }
    if view
        .lamp_button(
            layout::PLAYLIST_CLOSE,
            sprites::PLAYLIST_CLOSE_PRESSED,
            false,
            "playlist-close",
        )
        .clicked()
    {
        state.playlist_open = false;
        host.toggle_playlist_window();
    }
    if view
        .lamp_button(
            layout::PLAYLIST_SHADE,
            sprites::PLAYLIST_SHADE_PRESSED,
            false,
            "playlist-shade",
        )
        .clicked()
    {
        state.playlist_shaded = true;
    }

    let rows = rows(host);
    list(view, state, host, &rows, height);
    scrollbar(view, state, rows.len(), height);
    grip(view, state, height);
    times(view, state, host, &rows, height);
    mini_transport(view, state, host, height);
    menus(view, state, host, &rows, height);
}

/// The little transport in the bottom right: the same as the big one,
/// painted into the skin, so these are only places to click.
fn mini_transport(
    view: &mut View,
    _state: &mut WinampState,
    host: &mut dyn WinampHost,
    height: u32,
) {
    let (x, dy) = layout::PLAYLIST_MINI_TRANSPORT;
    let y = height - layout::PLAYLIST_BOTTOM_HEIGHT + dy;
    let playback = host.state().clone();
    let playing = playback.playing;
    for (index, name) in ["previous", "play", "pause", "stop", "next", "eject"]
        .into_iter()
        .enumerate()
    {
        let cell = Area::new(x + 10 * index as u32, y, 10, 10);
        let response = view
            .interact(cell, &format!("playlist-{name}"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if !response.clicked() {
            continue;
        }
        match name {
            "previous" => host.cmd(PlayerCommand::Prev),
            "play" => host.cmd(if playing {
                PlayerCommand::SeekRatio(0.0)
            } else {
                PlayerCommand::PlayPause
            }),
            "pause" if playback.track.is_some() => host.cmd(PlayerCommand::PlayPause),
            "stop" if playback.track.is_some() => {
                if playing {
                    host.cmd(PlayerCommand::PlayPause);
                }
                host.cmd(PlayerCommand::SeekRatio(0.0));
            }
            "next" => host.cmd(PlayerCommand::Next),
            "eject" => host.leave_mini_player(),
            _ => {}
        }
    }
}

/// The playlist rolled up: a bar with the song's name and time, the
/// button to roll it down again, and its X.
fn shade(view: &mut View, state: &mut WinampState, host: &mut dyn WinampHost, focused: bool) {
    use sprites::*;
    let width = layout::WINDOW_WIDTH;
    let height = layout::PLAYLIST_SHADE_HEIGHT;
    view.sprite_at(PLAYLIST_SHADE_LEFT, 0, 0);
    let mut x = 25;
    while x < width - 50 {
        view.sprite_clipped(
            PLAYLIST_SHADE_TILE,
            x,
            0,
            Area::new(25, 0, width - 75, height),
        );
        x += 25;
    }
    let right = if focused {
        PLAYLIST_SHADE_RIGHT_ACTIVE
    } else {
        PLAYLIST_SHADE_RIGHT
    };
    view.sprite_at(right, width - 50, 0);
    let title = view.interact(
        Area::new(0, 0, width, height),
        "playlist-shade-title",
        Sense::click_and_drag(),
    );
    if title.drag_started() {
        view.ui
            .ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        state.playlist_shaded = false;
    }
    let playback = host.state();
    if let Some(track) = playback.track.as_ref() {
        let time = format_duration(playback.position_secs);
        let time_width = 5 * time.len() as u32;
        let time_x = width - 30 - time_width;
        view.text(&time, Area::new(time_x, 4, time_width, 6));
        let name = label(1, &track.artist, &track.title);
        let name_area = Area::new(5, 4, time_x.saturating_sub(10), 6);
        if name.chars().all(crate::skin::font::covered) {
            view.text(&name, name_area);
        } else {
            super::pixel_line(view, state, &name, name_area);
        }
    }
    if view
        .lamp_button(
            layout::PLAYLIST_SHADE,
            sprites::PLAYLIST_UNSHADE_PRESSED,
            false,
            "playlist-unshade",
        )
        .clicked()
    {
        state.playlist_shaded = false;
    }
    if view
        .lamp_button(
            layout::PLAYLIST_CLOSE,
            sprites::PLAYLIST_CLOSE_PRESSED,
            false,
            "playlist-shade-close",
        )
        .clicked()
    {
        state.playlist_open = false;
        host.toggle_playlist_window();
    }
}

/// The frame: corners and tiles across the top, tiles down the sides
/// however tall the list is, and the two halves of the bottom.
fn frame(view: &mut View, height: u32, focused: bool) {
    use sprites::*;
    let (top_left, title, top_tile, top_right) = if focused {
        (
            PLAYLIST_TOP_LEFT_ACTIVE,
            PLAYLIST_TITLE_ACTIVE,
            PLAYLIST_TOP_TILE_ACTIVE,
            PLAYLIST_TOP_RIGHT_ACTIVE,
        )
    } else {
        (
            PLAYLIST_TOP_LEFT,
            PLAYLIST_TITLE,
            PLAYLIST_TOP_TILE,
            PLAYLIST_TOP_RIGHT,
        )
    };
    // The title sits centred; the tiles either side of it run out to the
    // corners, the last on each side cut to fit, as Winamp cut them.
    let tile = layout::PLAYLIST_TILE_WIDTH;
    let width = layout::WINDOW_WIDTH;
    let inner = width - 2 * tile - 100;
    let left = inner / 2;
    let right = inner - left;
    view.sprite_at(top_left, 0, 0);
    let left_run = Area::new(tile, 0, left, layout::PLAYLIST_TITLE_HEIGHT);
    let mut x = tile;
    while x < tile + left {
        view.sprite_clipped(top_tile, x, 0, left_run);
        x += tile;
    }
    view.sprite_at(title, tile + left, 0);
    let right_run = Area::new(tile + left + 100, 0, right, layout::PLAYLIST_TITLE_HEIGHT);
    let mut x = tile + left + 100;
    while x < width - tile {
        view.sprite_clipped(top_tile, x, 0, right_run);
        x += tile;
    }
    view.sprite_at(top_right, width - tile, 0);

    let middle = Area::new(
        0,
        layout::PLAYLIST_TITLE_HEIGHT,
        layout::WINDOW_WIDTH,
        height - layout::PLAYLIST_TITLE_HEIGHT - layout::PLAYLIST_BOTTOM_HEIGHT,
    );
    let mut y = middle.y;
    while y < middle.y + middle.height {
        view.sprite_clipped(PLAYLIST_LEFT_TILE, 0, y, middle);
        view.sprite_clipped(
            PLAYLIST_RIGHT_TILE,
            layout::WINDOW_WIDTH - layout::PLAYLIST_RIGHT_WIDTH,
            y,
            middle,
        );
        y += layout::PLAYLIST_TILE_HEIGHT;
    }
    let bottom = height - layout::PLAYLIST_BOTTOM_HEIGHT;
    view.sprite_at(PLAYLIST_BOTTOM_LEFT, 0, bottom);
    view.sprite_at(PLAYLIST_BOTTOM_RIGHT, 125, bottom);
}

/// The list's own area, between the tiles.
fn list_area(height: u32) -> Area {
    Area::new(
        layout::PLAYLIST_LEFT_WIDTH,
        layout::PLAYLIST_TITLE_HEIGHT,
        layout::WINDOW_WIDTH - layout::PLAYLIST_LEFT_WIDTH - layout::PLAYLIST_RIGHT_WIDTH,
        height - layout::PLAYLIST_TITLE_HEIGHT - layout::PLAYLIST_BOTTOM_HEIGHT,
    )
}

fn rows_visible(height: u32) -> usize {
    (list_area(height).height / layout::PLAYLIST_TRACK_HEIGHT) as usize
}

/// The queue, numbered the way Winamp numbered a playlist.
fn rows(host: &mut dyn WinampHost) -> Vec<Row> {
    let playback = host.state();
    playback
        .queue
        .iter()
        .enumerate()
        .map(|(index, track)| Row {
            index,
            label: label(index + 1, &track.artist, &track.title),
            duration_secs: track.duration_secs,
            current: playback.queue_index == Some(index),
        })
        .collect()
}

fn label(number: usize, artist: &str, title: &str) -> String {
    if artist.is_empty() {
        format!("{number}. {title}")
    } else {
        format!("{number}. {artist} - {title}")
    }
}

/// The rows, in the skin's colours and a font the size Winamp's was.
fn list(
    view: &mut View,
    state: &mut WinampState,
    host: &mut dyn WinampHost,
    rows: &[Row],
    height: u32,
) {
    let style = view.skin.playlist.clone();
    let rgb = |color: [u8; 3]| Color32::from_rgb(color[0], color[1], color[2]);
    let area = list_area(height);
    view.fill(
        area.x,
        area.y,
        area.width,
        area.height,
        rgb(style.normal_background),
    );

    let visible = rows_visible(height);
    let most = rows.len().saturating_sub(visible);
    // The wheel, over the list.
    // `contains_pointer`, not `hovered`: the rows drawn on top of the list
    // would take the hover and the wheel with it.
    let hovered = view
        .interact(area, "playlist-list", Sense::hover())
        .contains_pointer();
    if hovered {
        let row_points = layout::PLAYLIST_TRACK_HEIGHT as f32 * view.unit;
        let (lines, points, pages) = view.ui.ctx().input(|input| {
            input
                .events
                .iter()
                .fold((0.0, 0.0, 0.0), |sum, event| match event {
                    egui::Event::MouseWheel { unit, delta, .. } => match unit {
                        // One detent is one event however many lines the
                        // system says it is worth; a free-spinning wheel's
                        // fractions still add up.
                        egui::MouseWheelUnit::Line => {
                            let notch = if delta.y.abs() >= 1.0 {
                                delta.y.signum()
                            } else {
                                delta.y
                            };
                            (sum.0 + notch, sum.1, sum.2)
                        }
                        egui::MouseWheelUnit::Point => (sum.0, sum.1 + delta.y, sum.2),
                        egui::MouseWheelUnit::Page => (sum.0, sum.1, sum.2 + delta.y),
                    },
                    _ => sum,
                })
        });
        state.playlist_wheel += rows_for_wheel(lines, points, pages, row_points, visible);
        let rows_moved = state.playlist_wheel.trunc();
        if rows_moved != 0.0 {
            state.playlist_wheel -= rows_moved;
            let scroll = state.playlist_scroll as i64 + rows_moved as i64;
            state.playlist_scroll = scroll.clamp(0, most as i64) as usize;
        }
    }
    state.playlist_scroll = state.playlist_scroll.min(most);
    let scroll = state.playlist_scroll;

    let ctx = view.ui.ctx().clone();
    let clip = view.rect(area).intersect(view.ui.clip_rect());
    let painter = view.ui.painter().with_clip_rect(clip);
    let pad = 3;
    for (offset, row) in rows.iter().skip(scroll).take(visible).enumerate() {
        let line = Area::new(
            area.x,
            area.y + offset as u32 * layout::PLAYLIST_TRACK_HEIGHT,
            area.width,
            layout::PLAYLIST_TRACK_HEIGHT,
        );
        let rect = view.rect(line);
        let response = view.ui.interact(
            rect,
            egui::Id::new(("playlist-row", scroll + offset)),
            Sense::click(),
        );
        if response.clicked() {
            let adding = view.ui.ctx().input(|input| input.modifiers.command);
            // Selection is by row, not by song: the same song can sit in
            // the list more than once and one click means one row.
            let index = scroll + offset;
            let selection = &mut state.playlist_selection;
            if adding {
                if !selection.remove(&index) {
                    selection.insert(index);
                }
            } else {
                selection.clear();
                selection.insert(index);
            }
        }
        if response.double_clicked() {
            host.cmd(PlayerCommand::PlayAt(row.index));
        }
        if state.playlist_selection.contains(&(scroll + offset)) {
            painter.rect_filled(rect, 0.0, rgb(style.selected_background));
        }
        let color = rgb(if row.current {
            style.current
        } else {
            style.normal
        });
        let text = &mut state.playlist_text;
        let duration = row
            .duration_secs
            .map(|seconds| format_duration(seconds as f64))
            .unwrap_or_default();
        let duration_width = text.width(&duration).ceil() as u32;
        let title_room = line.width.saturating_sub(3 * pad + duration_width);
        let title = fit(text, &row.label, title_room as f32);
        if !duration.is_empty() {
            let duration_line = text.line(&ctx, &duration);
            let duration_at = Area::new(
                line.x + line.width - pad - duration_line.width,
                line.y + (line.height.saturating_sub(duration_line.height)) / 2,
                duration_line.width,
                duration_line.height,
            );
            painter.image(
                duration_line.texture.id(),
                view.rect(duration_at),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                color,
            );
        }
        let title_line = text.line(&ctx, &title);
        let title_at = Area::new(
            line.x + pad,
            line.y + (line.height.saturating_sub(title_line.height)) / 2,
            title_line.width.min(title_room),
            title_line.height,
        );
        let uv_right = title_at.width as f32 / title_line.width.max(1) as f32;
        painter.image(
            title_line.texture.id(),
            view.rect(title_at),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(uv_right, 1.0)),
            color,
        );
    }
}

/// How many rows a frame's wheel moves the list, positive downwards. A
/// notch scrolls three rows, the Windows default Winamp's list followed;
/// a trackpad's points scroll a row per row's height; a page is the rows
/// in view. egui's deltas are positive when the content moves down, which
/// is scrolling up.
fn rows_for_wheel(lines: f32, points: f32, pages: f32, row_points: f32, visible: usize) -> f32 {
    -(lines * 3.0 + points / row_points + pages * visible as f32)
}

/// Text cut to a width, with an ellipsis when it had to be.
fn fit(text: &mut super::PixelText, label: &str, width: f32) -> String {
    if text.width(label) <= width {
        return label.to_string();
    }
    let chars: Vec<char> = label.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        let candidate: String = chars[..keep].iter().collect::<String>() + "\u{2026}";
        if text.width(&candidate) <= width {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

/// The handle in the right-hand tiles, dragged to scroll.
fn scrollbar(view: &mut View, state: &mut WinampState, rows: usize, height: u32) {
    let visible = rows_visible(height);
    let most = rows.saturating_sub(visible);
    let top = layout::PLAYLIST_TITLE_HEIGHT;
    let travel = height
        .saturating_sub(layout::PLAYLIST_TITLE_HEIGHT + layout::PLAYLIST_BOTTOM_HEIGHT)
        .saturating_sub(layout::PLAYLIST_SCROLL_HANDLE_HEIGHT);
    let fraction = if most == 0 {
        0.0
    } else {
        state.playlist_scroll as f32 / most as f32
    };
    let y = top + (fraction * travel as f32).round() as u32;
    let handle = Area::new(
        layout::PLAYLIST_SCROLL_X,
        y,
        8,
        layout::PLAYLIST_SCROLL_HANDLE_HEIGHT,
    );
    let response = view.interact(handle, "playlist-scroll", Sense::click_and_drag());
    if response.dragged()
        && most > 0
        && let Some(pos) = response.interact_pointer_pos()
    {
        let pointer = (pos.y - view.origin.y) / view.unit
            - top as f32
            - layout::PLAYLIST_SCROLL_HANDLE_HEIGHT as f32 / 2.0;
        let fraction = (pointer / travel as f32).clamp(0.0, 1.0);
        state.playlist_scroll = (fraction * most as f32).round() as usize;
    }
    let sprite = if response.dragged() || response.is_pointer_button_down_on() {
        sprites::PLAYLIST_SCROLL_HANDLE_PRESSED
    } else {
        sprites::PLAYLIST_SCROLL_HANDLE
    };
    view.sprite(sprite, handle);
}

/// The corner that stretches the list, a tile row at a time.
fn grip(view: &mut View, state: &mut WinampState, height: u32) {
    let corner = Area::new(
        layout::WINDOW_WIDTH - layout::PLAYLIST_GRIP,
        height - layout::PLAYLIST_GRIP,
        layout::PLAYLIST_GRIP,
        layout::PLAYLIST_GRIP,
    );
    let response = view
        .interact(corner, "playlist-grip", Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if response.dragged() {
        state.playlist_resize += response.drag_delta().y / view.unit;
        let step = layout::PLAYLIST_RESIZE_STEP as f32;
        let steps = (state.playlist_resize / step).trunc();
        if steps != 0.0 {
            state.playlist_resize -= steps * step;
            state.playlist_height = (height as i64 + steps as i64 * step as i64).clamp(
                layout::PLAYLIST_MIN_HEIGHT as i64,
                layout::PLAYLIST_MAX_HEIGHT as i64,
            ) as u32;
        }
    }
    if response.drag_stopped() {
        state.playlist_resize = 0.0;
    }
}

/// The selected songs' time over the queue's total, as Winamp's list
/// showed them, and the song's own time where Winamp kept it, in the
/// skin's small font.
fn times(
    view: &mut View,
    state: &mut WinampState,
    host: &mut dyn WinampHost,
    rows: &[Row],
    height: u32,
) {
    let bottom = height - layout::PLAYLIST_BOTTOM_HEIGHT;
    let total: u64 = rows.iter().filter_map(|row| row.duration_secs).sum();
    let selected: u64 = rows
        .iter()
        .enumerate()
        .filter(|(index, _)| state.playlist_selection.contains(index))
        .filter_map(|(_, row)| row.duration_secs)
        .sum();
    let running = format!(
        "{}/{}",
        format_duration(selected as f64),
        format_duration(total as f64)
    );
    let (x, dy) = layout::PLAYLIST_RUNNING_TIME;
    view.text(
        &running,
        Area::new(x, bottom + dy, 5 * running.len() as u32, 6),
    );
    let playback = host.state();
    if playback.track.is_some() {
        let (x, dy) = layout::PLAYLIST_TRACK_TIME;
        let elapsed = format_duration(playback.position_secs);
        view.text(
            &elapsed,
            Area::new(x, bottom + dy, 5 * elapsed.len() as u32, 6),
        );
    }
}

/// The menus along the bottom. Winamp's managed playlist files; ytamp's
/// list is the queue, so the menus manage selection and the queue.
fn menus(
    view: &mut View,
    state: &mut WinampState,
    host: &mut dyn WinampHost,
    rows: &[Row],
    height: u32,
) {
    let bottom = height - layout::PLAYLIST_BOTTOM_HEIGHT;
    for (name, x) in layout::PLAYLIST_MENUS {
        let area = Area::new(x, bottom + 8, 22, 18);
        let button = view
            .interact(area, &format!("playlist-menu-{name}"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        egui::Popup::menu(&button)
            .id(egui::Id::new(("playlist-menu-open", name)))
            .show(|ui| {
                ui.set_min_width(90.0);
                match name {
                    "add" => {
                        // Adding is the big window's search box.
                        if ui.button("Search (big window)").clicked() {
                            host.leave_mini_player();
                        }
                    }
                    "rem" => {
                        if ui.button("Clear queue").clicked() {
                            host.cmd(PlayerCommand::QueueReplace(Vec::new(), None));
                            state.playlist_selection.clear();
                        }
                    }
                    "sel" => {
                        let selection = &mut state.playlist_selection;
                        if ui.button("All").clicked() {
                            selection.extend(0..rows.len());
                        }
                        if ui.button("None").clicked() {
                            selection.clear();
                        }
                        if ui.button("Invert").clicked() {
                            for index in 0..rows.len() {
                                if !selection.remove(&index) {
                                    selection.insert(index);
                                }
                            }
                        }
                    }
                    "misc" => {
                        if ui.button("Song info (big window)").clicked() {
                            host.leave_mini_player();
                        }
                    }
                    _ => {
                        ui.add_enabled(false, egui::Button::new("No saved lists yet"));
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_numbered_the_way_winamp_did() {
        assert_eq!(label(1, "Bonobo", "Rosewood"), "1. Bonobo - Rosewood");
        assert_eq!(label(12, "", "Episode 12"), "12. Episode 12");
    }

    #[test]
    fn a_notch_scrolls_three_rows_as_windows_did() {
        assert_eq!(rows_for_wheel(-1.0, 0.0, 0.0, 20.0, 8), 3.0);
        assert_eq!(rows_for_wheel(2.0, 0.0, 0.0, 20.0, 8), -6.0);
        assert_eq!(rows_for_wheel(0.0, -40.0, 0.0, 20.0, 8), 2.0);
        assert_eq!(rows_for_wheel(0.0, 0.0, -1.0, 20.0, 8), 8.0);
    }

    #[test]
    fn the_list_holds_whole_rows_only() {
        assert_eq!(rows_visible(116), 4);
        assert_eq!(rows_visible(174), 8);
        assert_eq!(list_area(174), Area::new(12, 20, 243, 116));
    }
}
