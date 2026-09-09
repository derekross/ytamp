// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.

//! The equalizer window, between the main window and the playlist.
//!
//! Draws Winamp's preamp, ten bands, power switch, presets, and response
//! graph from `eqmain.bmp`. The DSP itself lives in [`crate::eq`]; this
//! sends [`PlayerCommand::SetEq`] as the sliders move.
//!
//! Shade mode uses the compact volume and balance controls from `eq_ex.bmp`.

use egui::Sense;

use crate::eq::{self, EqSettings, RANGE_DB};
use crate::model::PlayerCommand;
use crate::skin::layout::{self, Area};
use crate::skin::sprites::Sprite;
use crate::skin::{Sheet, sprites};
use crate::winamp::{WinampHost, WinampState};

use super::{SliderEvent, View};

/// A slider's thumb is eleven pixels in a track of sixty-three.
const THUMB: u32 = 11;
const TRAVEL: u32 = layout::EQ_PREAMP.height - THUMB;
/// The graph's bands sit twelve pixels apart, two in from its edge.
const GRAPH_STEP: u32 = 12;
const GRAPH_PAD: u32 = 2;

pub(super) fn show(
    view: &mut View,
    state: &mut WinampState,
    host: &mut dyn WinampHost,
    focused: bool,
) {
    if state.eq_shaded {
        shade(view, state, host, focused);
        return;
    }
    view.sprite(
        sprites::EQ_BACKGROUND,
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::EQ_HEIGHT),
    );
    let bar = if focused {
        sprites::EQ_TITLE_BAR_ACTIVE
    } else {
        sprites::EQ_TITLE_BAR_INACTIVE
    };
    view.sprite(bar, layout::EQ_TITLE_BAR);
    let title = view.interact(layout::EQ_TITLE_BAR, "eq-title", Sense::click_and_drag());
    if title.drag_started() {
        view.ui
            .ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        state.eq_shaded = true;
    }
    if view
        .lamp_button(
            layout::EQ_SHADE,
            sprites::EQ_SHADE_BUTTON_PRESSED,
            false,
            "eq-shade",
        )
        .on_hover_text("Roll the equalizer up")
        .clicked()
    {
        state.eq_shaded = true;
    }
    if view
        .button(
            layout::EQ_CLOSE,
            sprites::EQ_CLOSE_BUTTON,
            sprites::EQ_CLOSE_BUTTON_PRESSED,
            "eq-close",
        )
        .clicked()
    {
        state.eq_open = false;
        host.toggle_eq_window();
    }

    let settings = host.eq();
    let (normal, pressed) = if settings.enabled {
        (sprites::EQ_ON_ON, sprites::EQ_ON_ON_PRESSED)
    } else {
        (sprites::EQ_ON_OFF, sprites::EQ_ON_OFF_PRESSED)
    };
    if view
        .button(layout::EQ_ON, normal, pressed, "eq-on")
        .clicked()
    {
        send(host, EqSettings {
            enabled: !settings.enabled,
            ..settings
        });
    }
    // Winamp's AUTO loaded a preset per song, which ytamp has no
    // equivalent for; here the button lays the bands flat.
    if view
        .button(
            layout::EQ_AUTO,
            sprites::EQ_AUTO_OFF,
            sprites::EQ_AUTO_OFF_PRESSED,
            "eq-auto",
        )
        .clicked()
    {
        send(host, EqSettings {
            gains_db: [0.0; 10],
            ..settings
        });
    }
    let presets = view.button(
        layout::EQ_PRESETS_BUTTON,
        sprites::EQ_PRESETS,
        sprites::EQ_PRESETS_PRESSED,
        "eq-presets",
    );
    egui::Popup::menu(&presets)
        .id(egui::Id::new("eq-presets-menu"))
        .show(|ui| {
            ui.set_min_width(120.0);
            for (index, preset) in eq::PRESETS.iter().enumerate() {
                if index == eq::WINAMP_PRESET_COUNT {
                    ui.separator();
                }
                let chosen = preset.gains_db == settings.gains_db;
                if ui.selectable_label(chosen, preset.name).clicked() {
                    send(host, EqSettings {
                        gains_db: preset.gains_db,
                        ..settings
                    });
                    ui.close();
                }
            }
        });

    graph(view, &settings);
    let preamp = fraction(settings.preamp_db);
    if let Some(value) = slider(view, layout::EQ_PREAMP, "eq-preamp", preamp) {
        send(host, EqSettings {
            preamp_db: decibels(value),
            ..settings
        });
    }
    for (band, gain_db) in settings.gains_db.into_iter().enumerate() {
        let area = layout::eq_band(band);
        if let Some(value) = slider(view, area, &format!("eq-band-{band}"), fraction(gain_db)) {
            let mut gains_db = settings.gains_db;
            gains_db[band] = decibels(value);
            send(host, EqSettings { gains_db, ..settings });
        }
    }
}

/// Sends new settings to the player, clamped to what the equalizer can do.
fn send(host: &mut dyn WinampHost, settings: EqSettings) {
    let settings = settings.clamped();
    host.cmd(PlayerCommand::SetEq {
        enabled: settings.enabled,
        gains_db: settings.gains_db,
        preamp_db: settings.preamp_db,
    });
}

/// Compact equalizer bar with volume and balance controls.
fn shade(view: &mut View, state: &mut WinampState, host: &mut dyn WinampHost, focused: bool) {
    let bar = if focused {
        sprites::EQ_SHADE_BAR_ACTIVE
    } else {
        sprites::EQ_SHADE_BAR_INACTIVE
    };
    let area = Area::new(0, 0, layout::WINDOW_WIDTH, layout::EQ_SHADE_HEIGHT);
    view.sprite(bar, area);
    let title = view.interact(area, "eq-shade-title", Sense::click_and_drag());
    if title.drag_started() {
        view.ui
            .ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        state.eq_shaded = false;
    }

    // Volume, as the main window's slider does it.
    let volume = host.state().volume.clamp(0.0, 1.0);
    let track = layout::EQ_SHADE_VOLUME;
    let (_response, event) = view.slider(track, "eq-shade-volume", layout::EQ_SHADE_THUMB);
    match event {
        SliderEvent::Dragging(value) => {
            state.volume_preview = Some(value);
            host.cmd(PlayerCommand::SetVolume(value));
        }
        SliderEvent::Committed(value) => {
            state.volume_preview = None;
            host.cmd(PlayerCommand::SetVolume(value));
        }
        SliderEvent::None => {}
    }
    let shown = match state.volume_preview {
        Some(fraction) => (fraction * 100.0).round() as u32,
        None => (volume * 100.0).round() as u32,
    };
    let thumb = look(
        shown as f32 / 100.0,
        [
            sprites::EQ_SHADE_VOLUME_THUMB_LOW,
            sprites::EQ_SHADE_VOLUME_THUMB_MIDDLE,
            sprites::EQ_SHADE_VOLUME_THUMB_HIGH,
        ],
    );
    let travel = track.width - layout::EQ_SHADE_THUMB;
    view.sprite_at(thumb, track.x + (shown * travel + 50) / 100, track.y);

    // Balance, with the same snap to the centre; ytamp's playback state
    // has no balance yet, so it previews through the marquee only.
    let track = layout::EQ_SHADE_BALANCE;
    let (_, event) = view.slider(track, "eq-shade-balance", layout::EQ_SHADE_THUMB);
    match event {
        SliderEvent::Dragging(value) => {
            state.balance_preview = Some(super::balance_of(value));
        }
        SliderEvent::Committed(_value) => {
            state.balance_preview = None;
        }
        SliderEvent::None => {}
    }
    let balance = state.balance_preview.unwrap_or(0.0);
    let fraction = (balance + 1.0) / 2.0;
    let thumb = look(
        fraction,
        [
            sprites::EQ_SHADE_BALANCE_THUMB_LEFT,
            sprites::EQ_SHADE_BALANCE_THUMB_MIDDLE,
            sprites::EQ_SHADE_BALANCE_THUMB_RIGHT,
        ],
    );
    let travel = track.width - layout::EQ_SHADE_THUMB;
    view.sprite_at(
        thumb,
        track.x + (fraction * travel as f32).round() as u32,
        track.y,
    );

    if view
        .lamp_button(
            layout::EQ_SHADE,
            sprites::EQ_UNSHADE_BUTTON_PRESSED,
            false,
            "eq-unshade",
        )
        .on_hover_text("Roll the equalizer down")
        .clicked()
    {
        state.eq_shaded = false;
    }
    if view
        .button(
            layout::EQ_CLOSE,
            sprites::EQ_SHADE_CLOSE_BUTTON,
            sprites::EQ_SHADE_CLOSE_BUTTON_PRESSED,
            "eq-shade-close",
        )
        .clicked()
    {
        state.eq_open = false;
        host.toggle_eq_window();
    }
}

/// Which of a mini slider's three looks goes with a value from 0 to 1.
fn look(value: f32, looks: [Sprite; 3]) -> Sprite {
    if value < 1.0 / 3.0 {
        looks[0]
    } else if value < 2.0 / 3.0 {
        looks[1]
    } else {
        looks[2]
    }
}

/// A gain as a fraction of the slider, 0 at the bottom.
fn fraction(gain_db: f64) -> f32 {
    (((gain_db + RANGE_DB) / (2.0 * RANGE_DB)) as f32).clamp(0.0, 1.0)
}

fn decibels(fraction: f32) -> f64 {
    f64::from(fraction) * 2.0 * RANGE_DB - RANGE_DB
}

/// A vertical slider drawn from the skin's frames, with the pointer's new
/// value while it is held.
fn slider(view: &mut View, area: Area, id: &str, value: f32) -> Option<f32> {
    let response = view
        .interact(area, id, Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let frame = (value * (sprites::EQ_SLIDER_FRAMES - 1) as f32).round() as u32;
    view.sprite(sprites::eq_slider_frame(frame), area);
    let held = response.dragged() || response.is_pointer_button_down_on();
    let thumb = if held {
        sprites::EQ_THUMB_PRESSED
    } else {
        sprites::EQ_THUMB
    };
    let thumb_y = area.y + ((1.0 - value) * TRAVEL as f32).round() as u32;
    view.sprite_at(thumb, area.x + 1, thumb_y);
    if !(response.dragged() || response.clicked()) {
        return None;
    }
    let pos = response.interact_pointer_pos()?;
    let along = (pos.y - view.origin.y) / view.unit - area.y as f32 - THUMB as f32 / 2.0;
    Some((1.0 - along / TRAVEL as f32).clamp(0.0, 1.0))
}

/// The curve through the bands, in the skin's colours row by row, with the
/// preamp's line under it.
fn graph(view: &mut View, settings: &EqSettings) {
    let area = layout::EQ_GRAPH;
    view.sprite(sprites::EQ_GRAPH, area);
    let rows = area.height;
    let row_of = |gain_db: f64| ((1.0 - fraction(gain_db)) * (rows - 1) as f32).round() as u32;
    view.sprite_at(
        sprites::EQ_PREAMP_LINE,
        area.x,
        area.y + row_of(settings.preamp_db),
    );
    let sheet = view.skin.sheet(Sheet::EqMain);
    let line = sprites::EQ_GRAPH_LINE;
    let color_at = |row: u32| {
        sheet
            .pixel(line.x, line.y + row.min(line.height - 1))
            .map(|[r, g, b, _]| egui::Color32::from_rgb(r, g, b))
            .unwrap_or(egui::Color32::WHITE)
    };
    let points: Vec<f32> = settings.gains_db.iter().map(|db| fraction(*db)).collect();
    let width = GRAPH_STEP * (eq::BANDS.len() as u32 - 1);
    let mut last = row_of(settings.gains_db[0]);
    for x in 0..=width {
        let value = spline(&points, x as f32 / GRAPH_STEP as f32);
        let row = ((1.0 - value) * (rows - 1) as f32).round() as u32;
        let (top, bottom) = if row < last { (row, last) } else { (last, row) };
        for r in top..=bottom {
            view.fill(area.x + GRAPH_PAD + x, area.y + r, 1, 1, color_at(r));
        }
        last = row;
    }
}

/// A smooth curve through evenly spaced points (Catmull-Rom), at `t`
/// measured in points.
fn spline(points: &[f32], t: f32) -> f32 {
    let last = points.len() - 1;
    let i = (t.floor() as usize).min(last.saturating_sub(1));
    let u = (t - i as f32).clamp(0.0, 1.0);
    let at = |index: isize| points[index.clamp(0, last as isize) as usize];
    let (p0, p1, p2, p3) = (
        at(i as isize - 1),
        at(i as isize),
        at(i as isize + 1),
        at(i as isize + 2),
    );
    let value = 0.5
        * (2.0 * p1
            + (-p0 + p2) * u
            + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * u * u
            + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * u * u * u);
    value.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slider_maps_the_range_and_back() {
        assert_eq!(fraction(-RANGE_DB), 0.0);
        assert_eq!(fraction(0.0), 0.5);
        assert_eq!(fraction(RANGE_DB), 1.0);
        assert!((decibels(fraction(3.0)) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn a_mini_slider_changes_its_look_by_thirds() {
        let looks = [
            sprites::EQ_SHADE_VOLUME_THUMB_LOW,
            sprites::EQ_SHADE_VOLUME_THUMB_MIDDLE,
            sprites::EQ_SHADE_VOLUME_THUMB_HIGH,
        ];
        assert_eq!(look(0.0, looks), looks[0]);
        assert_eq!(look(0.3, looks), looks[0]);
        assert_eq!(look(0.5, looks), looks[1]);
        assert_eq!(look(0.7, looks), looks[2]);
        assert_eq!(look(1.0, looks), looks[2]);
    }

    #[test]
    fn the_curve_passes_through_the_bands_and_stays_inside() {
        let points = [0.5, 1.0, 0.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
        assert!((spline(&points, 1.0) - 1.0).abs() < 1e-5);
        assert!((spline(&points, 2.0) - 0.0).abs() < 1e-5);
        for step in 0..=90 {
            let value = spline(&points, step as f32 / 10.0);
            assert!((0.0..=1.0).contains(&value));
        }
    }
}
