//! The stand-in Winamp mini player.
//!
//! Builder C. A plain-egui miniature of the classic 275x116 main window:
//! drag strip, big time readout, spectrum bars, transport, volume, a
//! position bar, and a compact ten-band EQ panel. Everything sizes by the
//! integer skin scale the way the real skinned renderer will, and the
//! window stays a fixed rounded card on a transparent viewport.
//!
//! INTEGRATOR: delete this file and the `mod mini_player;` line in
//! [`crate::app`]; Builder A's `crate::ui::winamp::winamp_ui` replaces it
//! with the same signature.

use egui::{Align, Color32, CornerRadius, Key, Layout, Rect, RichText, Sense, Vec2, pos2, vec2};

use super::*;

/// The classic main window's size in skin pixels.
pub const WINDOW_SKIN_SIZE: Vec2 = vec2(275.0, 116.0);

/// Extra skin-pixel height the stand-in EQ panel adds below the window.
const EQ_SKIN_HEIGHT: f32 = 156.0;

/// The window size the shell should ask the viewport for, EQ panel or not.
pub fn desired_window_size(state: &WinampState) -> Vec2 {
    let mut size = WINDOW_SKIN_SIZE;
    if state.eq_open {
        size.y += EQ_SKIN_HEIGHT;
    }
    size * state.scale as f32
}

/// `m:ss`, the Winamp clock.
pub fn fmt_time(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The classic analyser gradient: green through amber to red.
fn spectrum_color(value: f32) -> Color32 {
    let mix = |a: [u8; 3], b: [u8; 3], t: f32| {
        let t = t.clamp(0.0, 1.0);
        Color32::from_rgb(
            (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
            (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
            (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
        )
    };
    if value < 0.55 {
        mix([0x2E, 0xC4, 0x2E], [0xE0, 0xD0, 0x20], value / 0.55)
    } else {
        mix(
            [0xE0, 0xD0, 0x20],
            [0xE8, 0x40, 0x30],
            (value - 0.55) / 0.45,
        )
    }
}

/// A small square skin-scale button.
fn tiny_button(ui: &mut egui::Ui, label: &str, size: f32, u: f32) -> bool {
    ui.add_sized(
        [size, (size * 0.82).max(10.0)],
        egui::Button::new(RichText::new(label).size((size * 0.5).max(6.0))),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .clicked()
        && u >= 0.0 // keep `u` in the signature for parity with later skin-scale work
}

/// Pushes the scratch EQ through the host, Winamp's own echo of a twist.
fn push_eq(state: &WinampState, host: &mut dyn WinampHost) {
    let eq = state.eq_scratch;
    host.cmd(PlayerCommand::SetEq {
        enabled: eq.enabled,
        gains_db: eq.gains_db,
        preamp_db: eq.preamp_db,
    });
}

/// Draws the whole mini player into the window-sized `ui`.
pub fn draw(ui: &mut egui::Ui, state: &mut WinampState, skin: &Skin, host: &mut dyn WinampHost) {
    let u = state.scale as f32;
    let desired = desired_window_size(state);

    // The card: fixed size, centered in whatever the viewport gives us.
    let max = ui.max_rect();
    let min = max.center() - desired / 2.0;
    let card = Rect::from_min_size(min, desired);
    let corner = CornerRadius::same((3.0 * u) as u8);
    ui.painter()
        .rect_filled(card, corner, Color32::from_rgb(0x10, 0x0F, 0x16));
    ui.painter().rect_stroke(
        card,
        corner,
        egui::Stroke::new(1.0, Color32::from_rgb(0x8B, 0x5C, 0xF6)),
        egui::StrokeKind::Middle,
    );

    let mut body = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(card.shrink((3.5 * u).max(4.0)))
            .layout(Layout::top_down(Align::LEFT)),
    );
    {
        let style = body.style_mut();
        style.spacing.item_spacing = vec2(2.0 * u, 1.5 * u);
        style.spacing.button_padding = vec2((2.5 * u).max(2.0), (1.0 * u).max(1.0));
    }

    // ---- drag strip: skin name left, exit right -----------------------
    let strip_h = (11.0 * u).max(14.0);
    let (strip_rect, strip_response) =
        body.allocate_exact_size(vec2(body.available_width(), strip_h), Sense::drag());
    let strip_response = strip_response.on_hover_cursor(egui::CursorIcon::Grab);
    if strip_response.drag_started() {
        body.ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    let mut strip = body.new_child(
        egui::UiBuilder::new()
            .max_rect(strip_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    strip.add_space(2.0 * u);
    strip.label(
        RichText::new(format!("⚡ {}", skin.name))
            .size((5.5 * u).max(9.0))
            .weak(),
    );
    strip.with_layout(Layout::right_to_left(Align::Center), |s| {
        if tiny_button(s, "✕", (9.0 * u).max(14.0), u) {
            state.wants_exit = true;
        }
    });
    body.painter().hline(
        strip_rect.left()..=strip_rect.right(),
        strip_rect.bottom(),
        egui::Stroke::new(1.0, Color32::from_rgb(0x2A, 0x28, 0x36)),
    );

    // ---- time + title + spectrum --------------------------------------
    body.horizontal(|h| {
        h.vertical(|v| {
            let st = host.state();
            let remaining = st
                .duration_secs
                .map(|d| (d - st.position_secs).max(0.0))
                .unwrap_or(st.position_secs);
            let clock = if state.time_remaining {
                format!("-{}", fmt_time(remaining))
            } else {
                fmt_time(st.position_secs)
            };
            let pressed = v
                .add(
                    egui::Button::new(
                        RichText::new(clock)
                            .monospace()
                            .size((11.0 * u).max(15.0))
                            .color(Color32::from_rgb(0x9C, 0xEF, 0x9C)),
                    )
                    .frame(false),
                )
                .on_hover_text("Click to toggle time remaining (L)")
                .clicked();
            if pressed {
                state.time_remaining = !state.time_remaining;
            }
            let (title, artist) = match &st.track {
                Some(track) => (track.title.clone(), track.artist.clone()),
                None => ("ytamp".into(), "nothing playing".into()),
            };
            v.add(
                egui::Label::new(RichText::new(title).size((5.5 * u).max(9.5)).strong()).truncate(),
            );
            v.add(
                egui::Label::new(RichText::new(artist).size((5.0 * u).max(8.5)).weak()).truncate(),
            );
        });

        let (vis_rect, _) = h.allocate_exact_size(
            vec2((68.0 * u).max(60.0), (30.0 * u).max(26.0)),
            Sense::hover(),
        );
        let frame = host.spectrum();
        let bands = frame.bands.len().max(1) as f32;
        let bar_width = vis_rect.width() / bands;
        for (i, value) in frame.bands.iter().enumerate() {
            let value = value.clamp(0.0, 1.0);
            let height = value * vis_rect.height();
            let bar = Rect::from_min_size(
                pos2(
                    vis_rect.left() + i as f32 * bar_width + 0.5 * u,
                    vis_rect.bottom() - height,
                ),
                vec2((bar_width - 0.9 * u).max(1.0), height),
            );
            h.painter()
                .rect_filled(bar, CornerRadius::ZERO, spectrum_color(value));
        }
    });

    // ---- volume --------------------------------------------------------
    body.horizontal(|h| {
        h.label(RichText::new("V").size((5.0 * u).max(8.0)).weak());
        let mut volume = host.state().volume;
        let width = h.available_width();
        let response = h.add_sized(
            [width, 14.0],
            egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false),
        );
        if response.changed() {
            host.cmd(PlayerCommand::SetVolume(volume));
        }
    });

    // ---- transport -----------------------------------------------------
    body.horizontal(|h| {
        let button = (12.0 * u).max(18.0);
        if tiny_button(h, "⏮", button, u) {
            host.cmd(PlayerCommand::Prev);
        }
        let playing = host.state().playing;
        if tiny_button(h, if playing { "⏸" } else { "▶" }, button, u) {
            host.cmd(PlayerCommand::PlayPause);
        }
        if tiny_button(h, "■", button, u) {
            host.cmd(PlayerCommand::Stop);
        }
        if tiny_button(h, "⏭", button, u) {
            host.cmd(PlayerCommand::Next);
        }
        h.add_space(2.0 * u);
        if tiny_button(h, "EQ", button, u) {
            state.wants_eq = true;
        }
        h.with_layout(Layout::right_to_left(Align::Center), |s| {
            if tiny_button(s, "⏏", button, u) {
                state.wants_exit = true;
            }
        });
    });

    // ---- position bar ----------------------------------------------------
    let (bar_rect, bar_response) = body.allocate_exact_size(
        vec2(body.available_width(), (5.0 * u).max(7.0)),
        Sense::click_and_drag(),
    );
    let st = host.state();
    let ratio = st
        .duration_secs
        .filter(|duration| *duration > 0.05)
        .map(|duration| (st.position_secs / duration).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let track_rect = bar_rect.shrink2(vec2(0.0, bar_rect.height() * 0.3));
    body.painter().rect_filled(
        track_rect,
        CornerRadius::same(2),
        Color32::from_rgb(0x24, 0x22, 0x30),
    );
    let filled = Rect::from_min_max(
        track_rect.left_center(),
        egui::pos2(
            track_rect.left() + track_rect.width() * ratio as f32,
            track_rect.right_center().y,
        ),
    );
    body.painter().rect_filled(
        filled,
        CornerRadius::same(2),
        Color32::from_rgb(0x8B, 0x5C, 0xF6),
    );
    if (bar_response.dragged() || bar_response.clicked())
        && st.duration_secs.is_some_and(|duration| duration > 0.05)
        && let Some(pointer) = bar_response.interact_pointer_pos()
    {
        let ratio = ((pointer.x - bar_rect.left()) / bar_rect.width()).clamp(0.0, 1.0);
        host.cmd(PlayerCommand::SeekRatio(f64::from(ratio)));
    }

    // ---- the stand-in EQ panel -------------------------------------------
    if state.eq_open {
        body.painter().hline(
            card.left()..=card.right(),
            body.max_rect().bottom() - (EQ_SKIN_HEIGHT * u) + (2.0 * u),
            egui::Stroke::new(1.0, Color32::from_rgb(0x2A, 0x28, 0x36)),
        );
        let mut enabled = state.eq_scratch.enabled;
        if body
            .checkbox(&mut enabled, RichText::new("ON").size((5.0 * u).max(8.5)))
            .changed()
        {
            state.eq_scratch.enabled = enabled;
            push_eq(state, host);
        }
        body.horizontal(|h| {
            h.vertical(|v| {
                let mut preamp = state.eq_scratch.preamp_db;
                let response = v.add(
                    egui::Slider::new(&mut preamp, -12.0..=12.0)
                        .vertical()
                        .show_value(false),
                );
                if response.changed() {
                    state.eq_scratch.preamp_db = preamp;
                    push_eq(state, host);
                }
                v.label(RichText::new("PRE").size((4.0 * u).max(7.0)).weak());
            });
            for band in 0..10 {
                h.vertical(|v| {
                    let mut gain = state.eq_scratch.gains_db[band];
                    let response = v.add(
                        egui::Slider::new(&mut gain, -12.0..=12.0)
                            .vertical()
                            .show_value(false),
                    );
                    if response.changed() {
                        state.eq_scratch.gains_db[band] = gain;
                        push_eq(state, host);
                    }
                    v.label(
                        RichText::new(crate::settings::eq_band_label(
                            crate::settings::EQ_BAND_CENTERS_HZ[band],
                        ))
                        .size((3.5 * u).max(7.0))
                        .weak(),
                    );
                });
            }
        });
    }

    // ---- Winamp's classic single letters ----------------------------------
    handle_keys(state, host, &body.ctx());
}

/// Z X C V B, Space, and L — the keys Winamp taught a generation.
fn handle_keys(state: &mut WinampState, host: &mut dyn WinampHost, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    let mut command = None;
    let mut toggle_clock = false;
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
            if !modifiers.is_none() {
                continue;
            }
            command = match key {
                Key::Space | Key::Enter => Some(PlayerCommand::PlayPause),
                Key::Z => Some(PlayerCommand::Prev),
                Key::X => Some(PlayerCommand::Resume),
                Key::C => Some(PlayerCommand::Pause),
                Key::V => Some(PlayerCommand::Stop),
                Key::B => Some(PlayerCommand::Next),
                Key::L => {
                    toggle_clock = true;
                    None
                }
                _ => None,
            };
        }
    });
    if let Some(command) = command {
        host.cmd(command);
    }
    if toggle_clock {
        state.time_remaining = !state.time_remaining;
    }
}
