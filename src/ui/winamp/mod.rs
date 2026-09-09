// Adapted from fastpotify (https://github.com/crmne/fastpotify), MIT license.

//! The skinned windows: the main Winamp window, with the equalizer and the
//! playlist hung beneath it, all drawn from the skin's sprite sheets.
//!
//! Where fastpotify drove this from its `App` and `Action` queue, ytamp
//! drives it from the [`WinampHost`](crate::winamp::WinampHost) contract:
//! the host owns playback state and receives [`PlayerCommand`]s.

mod equalizer;
mod pixel_text;
mod playlist;

pub use pixel_text::PixelText;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Color32, Id, Pos2, Rect, Response, Sense, TextureId, Ui, pos2, vec2};

use crate::model::PlayerCommand;
use crate::skin::layout::{self, Area};
use crate::skin::{Mask, Sheet, Skin, Sprite, font, sprites};
use crate::vis;
use crate::winamp::{MAX_SCALE, SkinTextures, WinampHost, WinampState};

/// How often the visualiser moves.
const VIS_FRAME: Duration = Duration::from_micros(16_667);

/// What a slider reports while the pointer is on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SliderEvent {
    None,
    /// The thumb is being dragged; the value is where it is now.
    Dragging(f32),
    /// The drag ended, or the track was clicked; the value is final.
    Committed(f32),
}

/// The stack's height in logical points: the window's size at this scale.
pub fn window_size(state: &WinampState) -> egui::Vec2 {
    vec2(layout::WINDOW_WIDTH as f32, state.stack_height() as f32) * state.scale as f32
}

/// Draws the skin's sprites into the window and reads the pointer against
/// the skin's layout.
pub(crate) struct View<'a> {
    pub ui: &'a mut Ui,
    pub origin: Pos2,
    pub unit: f32,
    pub skin: &'a Skin,
    pub textures: &'a HashMap<Sheet, TextureId>,
    /// The window's shape, when the skin is not a rectangle: nothing is
    /// painted outside it.
    pub mask: Option<&'a Mask>,
}

impl View<'_> {
    pub fn rect(&self, area: Area) -> Rect {
        Rect::from_min_size(
            self.origin + vec2(area.x as f32, area.y as f32) * self.unit,
            vec2(area.width as f32, area.height as f32) * self.unit,
        )
    }

    /// A pointer position as a skin x coordinate.
    fn skin_x(&self, pos: Pos2) -> f32 {
        (pos.x - self.origin.x) / self.unit
    }

    fn paint(&self, painter: &egui::Painter, sprite: Sprite, x: u32, y: u32) {
        let Some((bitmap, clipped)) = self.skin.sprite(sprite) else {
            return;
        };
        let Some(&texture) = self.textures.get(&sprite.sheet) else {
            return;
        };
        let (width, height) = (bitmap.width as f32, bitmap.height as f32);
        // A piece of the sprite, `dx` in and `columns` wide, on one row or
        // all of them.
        let piece = |dx: u32, dy: u32, columns: u32, rows: u32| {
            let uv = Rect::from_min_max(
                pos2(
                    (clipped.x + dx) as f32 / width,
                    (clipped.y + dy) as f32 / height,
                ),
                pos2(
                    (clipped.x + dx + columns) as f32 / width,
                    (clipped.y + dy + rows) as f32 / height,
                ),
            );
            let dest = Rect::from_min_size(
                self.origin + vec2((x + dx) as f32, (y + dy) as f32) * self.unit,
                vec2(columns as f32, rows as f32) * self.unit,
            );
            painter.image(texture, dest, uv, Color32::WHITE);
        };
        match self.mask {
            None => piece(0, 0, clipped.width, clipped.height),
            Some(mask) => {
                for dy in 0..clipped.height {
                    for (start, end) in mask.spans(y + dy) {
                        let from = (*start).max(x);
                        let to = (*end).min(x + clipped.width);
                        if to > from {
                            piece(from - x, dy, to - from, 1);
                        }
                    }
                }
            }
        }
    }

    pub fn sprite_at(&self, sprite: Sprite, x: u32, y: u32) {
        self.paint(self.ui.painter(), sprite, x, y);
    }

    /// A sprite cut to an area, for tiles that run past the edge.
    pub fn sprite_clipped(&self, sprite: Sprite, x: u32, y: u32, clip: Area) {
        let clip = self.rect(clip).intersect(self.ui.clip_rect());
        let painter = self.ui.painter().with_clip_rect(clip);
        self.paint(&painter, sprite, x, y);
    }

    /// A block of skin pixels in one colour.
    pub fn fill(&self, x: u32, y: u32, width: u32, height: u32, color: Color32) {
        let block = |x: u32, y: u32, width: u32, height: u32| {
            let rect = Rect::from_min_size(
                self.origin + vec2(x as f32, y as f32) * self.unit,
                vec2(width as f32, height as f32) * self.unit,
            );
            self.ui.painter().rect_filled(rect, 0.0, color);
        };
        match self.mask {
            None => block(x, y, width, height),
            Some(mask) => {
                for row in y..y + height {
                    for (start, end) in mask.spans(row) {
                        let from = (*start).max(x);
                        let to = (*end).min(x + width);
                        if to > from {
                            block(from, row, to - from, 1);
                        }
                    }
                }
            }
        }
    }

    pub fn sprite(&self, sprite: Sprite, area: Area) {
        self.sprite_at(sprite, area.x, area.y);
    }

    /// A line of the skin's bitmap font, cut off at the area's edge.
    pub fn text(&self, text: &str, area: Area) {
        let clip = self.rect(area).intersect(self.ui.clip_rect());
        let painter = self.ui.painter().with_clip_rect(clip);
        for (index, character) in text.chars().enumerate() {
            let x = area.x + 5 * index as u32;
            if x >= area.x + area.width {
                break;
            }
            self.paint(&painter, font::glyph(character), x, area.y);
        }
    }

    pub fn interact(&mut self, area: Area, id: &str, sense: Sense) -> Response {
        let rect = self.rect(area);
        self.ui.interact(rect, Id::new(("winamp", id)), sense)
    }

    /// A button drawn pressed while the pointer holds it down.
    pub fn button(&mut self, area: Area, normal: Sprite, pressed: Sprite, id: &str) -> Response {
        let response = self
            .interact(area, id, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let sprite = if response.is_pointer_button_down_on() {
            pressed
        } else {
            normal
        };
        self.sprite(sprite, area);
        response
    }

    /// A button whose only sprite is its lit state, drawn over the
    /// background while it is on or held.
    pub fn lamp_button(&mut self, area: Area, lit: Sprite, on: bool, id: &str) -> Response {
        let response = self
            .interact(area, id, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if on || response.is_pointer_button_down_on() {
            self.sprite(lit, area);
        }
        response
    }

    /// A slider along an area, its thumb `thumb` pixels wide: the pointer's
    /// position as a fraction of the thumb's travel.
    pub fn slider(&mut self, area: Area, id: &str, thumb: u32) -> (Response, SliderEvent) {
        let response = self
            .interact(area, id, Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let memory = Id::new(("winamp-slider", id));
        let dragging = self.ui.data(|data| data.get_temp::<f32>(memory));
        let travel = (area.width - thumb) as f32;
        let pointer = response.interact_pointer_pos().map(|pos| {
            ((self.skin_x(pos) - area.x as f32 - thumb as f32 / 2.0) / travel).clamp(0.0, 1.0)
        });
        let mut event = SliderEvent::None;
        if (response.drag_started() || response.dragged())
            && let Some(value) = pointer
        {
            self.ui.data_mut(|data| data.insert_temp(memory, value));
            event = SliderEvent::Dragging(value);
        }
        if response.drag_stopped() {
            if let Some(value) = dragging.or(pointer) {
                event = SliderEvent::Committed(value);
            }
            self.ui.data_mut(|data| data.remove::<f32>(memory));
        } else if response.clicked()
            && let Some(value) = pointer
        {
            event = SliderEvent::Committed(value);
        }
        (response, event)
    }
}

/// The whole stack: the main window, and the equalizer and playlist under
/// it, whichever are open. This is the entry point the app shell calls.
pub fn winamp_ui(
    ui: &mut Ui,
    state: &mut WinampState,
    skin: &Skin,
    host: &mut dyn WinampHost,
    textures: &mut SkinTextures,
) {
    let ctx = ui.ctx().clone();
    let unit = state.scale as f32;
    let origin = ui.max_rect().min;
    let focused = ctx.input(|input| input.viewport().focused).unwrap_or(true);
    for path in ctx.input(|input| {
        input
            .raw
            .dropped_files
            .iter()
            .map(|file| file.path().to_path_buf())
            .collect::<Vec<_>>()
    }) {
        host.load_skin_file(&path);
    }

    let textures = textures.get(&ctx, skin);
    let time = ctx.input(|input| input.time);
    let shaded = state.shaded;
    let mask = if shaded {
        skin.regions.shade.as_ref()
    } else {
        skin.regions.normal.as_ref()
    };
    let mut view = View {
        ui,
        origin,
        unit,
        skin,
        textures: &textures,
        mask,
    };
    let vis_moving = if shaded {
        shade_bar(&mut view, state, host, &ctx, focused);
        false
    } else {
        full_window(&mut view, state, host, &ctx, focused, time)
    };

    // The other windows hang under this one in Winamp's order.
    let mut below_y = if shaded {
        layout::SHADE_HEIGHT
    } else {
        layout::WINDOW_HEIGHT
    };
    if state.eq_open {
        let eq_shaded = state.eq_shaded;
        let mut below = View {
            ui: view.ui,
            origin: origin + vec2(0.0, below_y as f32 * unit),
            unit,
            skin,
            textures: &textures,
            mask: if eq_shaded {
                skin.regions.equalizer_shade.as_ref()
            } else {
                skin.regions.equalizer.as_ref()
            },
        };
        equalizer::show(&mut below, state, host, focused);
        below_y += if eq_shaded {
            layout::EQ_SHADE_HEIGHT
        } else {
            layout::EQ_HEIGHT
        };
    }
    if state.playlist_open {
        let mut below = View {
            ui: view.ui,
            origin: origin + vec2(0.0, below_y as f32 * unit),
            unit,
            skin,
            textures: &textures,
            mask: None,
        };
        playlist::show(&mut below, state, host, focused);
    }

    // The visualiser wants a frame every 60th of a second while it moves;
    // otherwise the marquee steps and the time ticks, and while paused the
    // time blinks. egui takes one predicted frame (a 60th) off every delay
    // on the assumption that vsync paces the loop; asking for two frames
    // waits one.
    if vis_moving {
        ctx.request_repaint_after(VIS_FRAME * 2);
    } else if host.state().track.is_some() {
        ctx.request_repaint_after(Duration::from_millis(220));
    }
}

/// The main window as it usually is. Returns whether the visualiser is
/// still moving.
fn full_window(
    view: &mut View,
    state: &mut WinampState,
    host: &mut dyn WinampHost,
    ctx: &egui::Context,
    focused: bool,
    time: f64,
) -> bool {
    view.sprite(
        sprites::MAIN_BACKGROUND,
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT),
    );
    title_bar(view, state, host, ctx, focused);
    clutter_bar(view, state, host);
    status(view, state, host);
    time_display(view, state, host, time);
    let vis_moving = visualiser(view, state, host);
    marquee(view, state, host);
    rates(view, host);
    sliders(view, state, host);
    windows_buttons(view, state, host);
    transport(view, state, host);
    shuffle_repeat(view, state, host);
    if view
        .interact(layout::ABOUT, "about", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Back to the big window")
        .clicked()
    {
        host.leave_mini_player();
    }

    vis_moving
}

/// The main window rolled up: the bar with the time in the small font,
/// a little transport, and a little seek bar, as Winamp's shade mode had.
fn shade_bar(
    view: &mut View,
    state: &mut WinampState,
    host: &mut dyn WinampHost,
    ctx: &egui::Context,
    focused: bool,
) {
    let bar = if focused {
        sprites::SHADE_BAR_ACTIVE
    } else {
        sprites::SHADE_BAR_INACTIVE
    };
    view.sprite(
        bar,
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::SHADE_HEIGHT),
    );
    let title = view.interact(
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::SHADE_HEIGHT),
        "shade-bar",
        Sense::click_and_drag(),
    );
    if title.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        state.shaded = false;
    }
    if view
        .button(
            layout::OPTIONS_BUTTON,
            sprites::OPTIONS_BUTTON,
            sprites::OPTIONS_BUTTON_PRESSED,
            "logo",
        )
        .on_hover_text("Back to the big window")
        .clicked()
    {
        host.leave_mini_player();
    }
    if view
        .button(
            layout::MINIMIZE_BUTTON,
            sprites::MINIMIZE_BUTTON,
            sprites::MINIMIZE_BUTTON_PRESSED,
            "minimize",
        )
        .clicked()
    {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
    if view
        .button(
            layout::SHADE_BUTTON,
            sprites::UNSHADE_BUTTON,
            sprites::UNSHADE_BUTTON_PRESSED,
            "unshade",
        )
        .on_hover_text("Roll the window down")
        .clicked()
    {
        state.shaded = false;
    }
    if view
        .button(
            layout::CLOSE_BUTTON,
            sprites::CLOSE_BUTTON,
            sprites::CLOSE_BUTTON_PRESSED,
            "close",
        )
        .on_hover_text("Back to the big window")
        .clicked()
    {
        host.leave_mini_player();
    }

    // The time, in the small font; a click counts down instead.
    if view
        .interact(layout::SHADE_TIME, "shade-time", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        state.time_remaining = !state.time_remaining;
    }
    let playback = host.state().clone();
    let playing = playback.playing;
    if !stopped(&playback) {
        let position = match state.seek_preview {
            Some(fraction) => fraction * playback.duration_secs.unwrap_or(0.0),
            None => playback.position_secs,
        };
        let duration = playback.duration_secs.unwrap_or(0.0);
        let remaining = state.time_remaining && duration > 0.0;
        let shown = if remaining {
            (duration - position).max(0.0)
        } else {
            position
        };
        let text = format!(
            "{}{}",
            if remaining { "-" } else { " " },
            format_duration(shown)
        );
        view.text(&text, layout::SHADE_TIME);
    }

    // The little transport: painted into the bar, so these only listen.
    let mini: [(&str, Area); 6] = [
        ("previous", layout::SHADE_PREVIOUS),
        ("play", layout::SHADE_PLAY),
        ("pause", layout::SHADE_PAUSE),
        ("stop", layout::SHADE_STOP),
        ("next", layout::SHADE_NEXT),
        ("eject", layout::SHADE_EJECT),
    ];
    for (name, area) in mini {
        let response = view
            .interact(area, &format!("shade-{name}"), Sense::click())
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

    // The little seek bar.
    view.sprite(sprites::SHADE_POSITION_TRACK, layout::SHADE_POSITION);
    let duration = playback.duration_secs.unwrap_or(0.0);
    if playback.track.is_none() || duration <= 0.0 || stopped(&playback) {
        return;
    }
    let (response, event) = view.slider(layout::SHADE_POSITION, "shade-position", 3);
    match event {
        SliderEvent::Dragging(value) => state.seek_preview = Some(value as f64),
        SliderEvent::Committed(value) => {
            state.seek_preview = None;
            host.cmd(PlayerCommand::SeekRatio(value as f64));
        }
        SliderEvent::None => {}
    }
    let fraction = state
        .seek_preview
        .unwrap_or(playback.position_secs / duration)
        .clamp(0.0, 1.0) as f32;
    let travel = layout::SHADE_POSITION.width - 3;
    let thumb = if response.dragged() {
        sprites::SHADE_POSITION_THUMB_RIGHT
    } else {
        sprites::SHADE_POSITION_THUMB
    };
    view.sprite_at(
        thumb,
        layout::SHADE_POSITION.x + (fraction * travel as f32).round() as u32,
        layout::SHADE_POSITION.y,
    );
}

fn title_bar(
    view: &mut View,
    state: &mut WinampState,
    host: &mut dyn WinampHost,
    ctx: &egui::Context,
    focused: bool,
) {
    let bar = if focused {
        sprites::TITLE_BAR_ACTIVE
    } else {
        sprites::TITLE_BAR_INACTIVE
    };
    view.sprite(bar, layout::TITLE_BAR);
    let title = view.interact(layout::TITLE_BAR, "title", Sense::click_and_drag());
    if title.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        state.shaded = true;
    }
    // The logo and the close button lead back to the big window: the mini
    // player is a way of looking at the same app, not a second one to
    // close.
    if view
        .button(
            layout::OPTIONS_BUTTON,
            sprites::OPTIONS_BUTTON,
            sprites::OPTIONS_BUTTON_PRESSED,
            "logo",
        )
        .on_hover_text("Back to the big window")
        .clicked()
    {
        host.leave_mini_player();
    }
    if view
        .button(
            layout::MINIMIZE_BUTTON,
            sprites::MINIMIZE_BUTTON,
            sprites::MINIMIZE_BUTTON_PRESSED,
            "minimize",
        )
        .clicked()
    {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
    if view
        .button(
            layout::SHADE_BUTTON,
            sprites::SHADE_BUTTON,
            sprites::SHADE_BUTTON_PRESSED,
            "shade",
        )
        .on_hover_text("Roll the window up")
        .clicked()
    {
        state.shaded = true;
    }
    if view
        .button(
            layout::CLOSE_BUTTON,
            sprites::CLOSE_BUTTON,
            sprites::CLOSE_BUTTON_PRESSED,
            "close",
        )
        .on_hover_text("Back to the big window")
        .clicked()
    {
        host.leave_mini_player();
    }
}

/// The O A I D V strip: options, always on top, info, double size, and
/// the visualiser. Each lights while held; D stays lit while past 1x.
fn clutter_bar(view: &mut View, state: &mut WinampState, host: &mut dyn WinampHost) {
    view.sprite(sprites::CLUTTER_BAR, layout::CLUTTER_BAR);
    // O opened Winamp's options menu; here it leaves for the big window,
    // where the settings live.
    if view
        .lamp_button(
            layout::CLUTTER_O,
            sprites::CLUTTER_O_LIT,
            false,
            "clutter-o",
        )
        .on_hover_text("Options (in the big window)")
        .clicked()
    {
        host.leave_mini_player();
    }
    // A was "always on top"; the host owns window level, so the lamp is
    // decorative until the host wires it.
    view.lamp_button(
        layout::CLUTTER_A,
        sprites::CLUTTER_A_LIT,
        false,
        "clutter-a",
    );
    // I showed the song's info; the big window's now-playing panel is
    // that.
    if view
        .lamp_button(
            layout::CLUTTER_I,
            sprites::CLUTTER_I_LIT,
            false,
            "clutter-i",
        )
        .on_hover_text("Song info (in the big window)")
        .clicked()
    {
        host.leave_mini_player();
    }
    // D goes round the sizes worth having on today's displays, 2x to 4x;
    // 1x comes back round after 4x.
    let scale = state.scale;
    if view
        .lamp_button(
            layout::CLUTTER_D,
            sprites::CLUTTER_D_LIT,
            scale >= 2,
            "clutter-d",
        )
        .on_hover_text("Size: 2x, 3x, 4x")
        .clicked()
    {
        state.scale = if scale >= MAX_SCALE {
            2
        } else {
            (scale + 1).max(2)
        };
    }
    // V opened Winamp's visualisation menu; a click on the display itself
    // cycles it, so the lamp only lights.
    view.lamp_button(
        layout::CLUTTER_V,
        sprites::CLUTTER_V_LIT,
        false,
        "clutter-v",
    );
}

/// The display's left box: the spectrum analyser in the skin's own
/// colours, or nothing. A click cycles the modes. Returns whether
/// anything is still moving.
fn visualiser(view: &mut View, state: &mut WinampState, host: &mut dyn WinampHost) -> bool {
    let area = layout::VISUALIZER;
    if view
        .interact(area, "visualiser", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        state.vis_mode = match state.vis_mode {
            vis::VisMode::Bars => vis::VisMode::Off,
            vis::VisMode::Off => vis::VisMode::Bars,
        };
    }
    if state.vis_mode == vis::VisMode::Off {
        return false;
    }
    let palette = view.skin.vis_colors;
    let color =
        |index: usize| Color32::from_rgb(palette[index][0], palette[index][1], palette[index][2]);
    view.fill(area.x, area.y, area.width, area.height, color(0));
    for y in (0..area.height).step_by(2) {
        for x in (0..area.width).step_by(2) {
            view.fill(area.x + x, area.y + y, 1, 1, color(1));
        }
    }
    let sounding = host.state().playing;
    let bands = if sounding {
        host.spectrum().bands
    } else {
        Vec::new()
    };
    let bars = state.analyser.step(&bands, Instant::now());
    for (index, bar) in bars.iter().enumerate() {
        let x = area.x + 4 * index as u32;
        for row in (vis::ROWS - bar.height)..vis::ROWS {
            view.fill(
                x,
                area.y + u32::from(row),
                3,
                1,
                color(2 + usize::from(row)),
            );
        }
        if let Some(peak) = bar.peak {
            let row = vis::ROWS - peak;
            view.fill(x, area.y + u32::from(row), 3, 1, color(23));
        }
    }
    sounding || !state.analyser.settled()
}

/// Whether the player is stopped, as Winamp meant it: something loaded and
/// paused at the very start.
fn stopped(playback: &crate::model::PlaybackState) -> bool {
    playback.track.is_some() && !playback.playing && playback.position_secs == 0.0
}

/// The play, pause, and stop lamp, the work indicator, and the mono and
/// stereo lamps, which only light here: ytamp has no mono fold yet.
fn status(view: &mut View, _state: &mut WinampState, host: &mut dyn WinampHost) {
    let playback = host.state();
    let status = if playback.playing {
        sprites::STATUS_PLAYING
    } else if playback.track.is_some() && !stopped(playback) {
        sprites::STATUS_PAUSED
    } else {
        sprites::STATUS_STOPPED
    };
    view.sprite(status, layout::STATUS);
    view.sprite(sprites::WORK_INDICATOR_OFF, layout::WORK_INDICATOR);
    let sounding = playback.track.is_some() && !stopped(playback);
    view.sprite(
        if sounding {
            sprites::STEREO_ON
        } else {
            sprites::STEREO_OFF
        },
        layout::STEREO,
    );
    view.sprite(sprites::MONO_OFF, layout::MONO);
}

/// The time in the skin's digits: elapsed, or remaining with a minus sign,
/// blinking while paused, blank with nothing on.
fn time_display(view: &mut View, state: &mut WinampState, host: &mut dyn WinampHost, time: f64) {
    let whole = Area::new(
        layout::MINUS_EX.x,
        layout::MINUS_EX.y,
        layout::SECOND_ONES.x + layout::SECOND_ONES.width - layout::MINUS_EX.x,
        layout::MINUS_EX.height,
    );
    if view
        .interact(whole, "time", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        state.time_remaining = !state.time_remaining;
    }
    let extended = view.skin.has_extended_digits();
    // The blank digit is painted, not left out: what the main sheet has
    // under the digits is the skin's own idea of an empty display, which
    // is not always empty.
    let blank = |view: &mut View| {
        if extended {
            for cell in layout::TIME_DIGITS {
                view.sprite(sprites::NUMS_EX_BLANK, cell);
            }
            view.sprite(sprites::NUMS_EX_BLANK, layout::MINUS_EX);
        } else {
            for cell in layout::TIME_DIGITS {
                view.sprite(sprites::NUMBERS_BLANK, cell);
            }
            view.sprite(sprites::NUMBERS_NO_MINUS, layout::MINUS);
        }
    };
    let playback = host.state();
    if playback.track.is_none() || stopped(playback) {
        blank(view);
        return;
    }
    let paused = !playback.playing;
    if paused && (time * 2.0).floor() as i64 % 2 == 1 {
        blank(view);
        return;
    }
    let duration = playback.duration_secs.unwrap_or(0.0);
    let position = match state.seek_preview {
        Some(fraction) => fraction * duration,
        None => playback.position_secs,
    };
    let remaining = state.time_remaining && duration > 0.0;
    let shown = if remaining {
        (duration - position).max(0.0)
    } else {
        position
    };
    let seconds = shown as u64;
    let minutes = (seconds / 60).min(99);
    let seconds = seconds % 60;
    let digits = [
        (minutes / 10) as u32,
        (minutes % 10) as u32,
        (seconds / 10) as u32,
        (seconds % 10) as u32,
    ];
    for (value, cell) in digits.into_iter().zip(layout::TIME_DIGITS) {
        let sprite = if extended {
            sprites::digit_ex(value)
        } else {
            sprites::digit(value)
        };
        view.sprite(sprite, cell);
    }
    match (extended, remaining) {
        (true, true) => view.sprite(sprites::NUMS_EX_MINUS, layout::MINUS_EX),
        (true, false) => view.sprite(sprites::NUMS_EX_BLANK, layout::MINUS_EX),
        (false, true) => view.sprite(sprites::NUMBERS_MINUS, layout::MINUS),
        (false, false) => view.sprite(sprites::NUMBERS_NO_MINUS, layout::MINUS),
    }
}

/// What the marquee says: a slider while it moves, as Winamp announced
/// them, a seek while it is dragged, else the song.
pub fn marquee_text(
    playback: &crate::model::PlaybackState,
    seek_preview: Option<f64>,
    volume_preview: Option<f32>,
    balance_preview: Option<f32>,
) -> String {
    if let Some(balance) = balance_preview {
        let percent = (balance.abs() * 100.0).round() as u32;
        return match balance {
            b if b < 0.0 => format!("Balance: {percent}% left"),
            b if b > 0.0 => format!("Balance: {percent}% right"),
            _ => "Balance: center".to_string(),
        };
    }
    if let Some(volume) = volume_preview {
        return format!("Volume: {}%", (volume * 100.0).round() as u32);
    }
    let Some(track) = playback.track.as_ref() else {
        return "ytamp".to_string();
    };
    let duration = playback.duration_secs.unwrap_or(0.0);
    if let Some(fraction) = seek_preview
        && duration > 0.0
    {
        return format!(
            "Seek to: {}/{} ({}%)",
            format_duration(fraction * duration),
            format_duration(duration),
            (fraction * 100.0).round() as u32
        );
    }
    let mut text = track.display();
    if duration > 0.0 {
        text.push_str(&format!(" ({})", format_duration(duration)));
    }
    text
}

fn marquee(view: &mut View, state: &mut WinampState, host: &mut dyn WinampHost) {
    let text = marquee_text(
        host.state(),
        state.seek_preview,
        state.volume_preview,
        state.balance_preview,
    );
    let (shown, offset) = state.marquee(&text, Instant::now());
    if text.chars().all(font::covered) {
        view.text(&shown, layout::MARQUEE);
    } else {
        // The skin's bitmap font cannot say this (Japanese, say, came out
        // as question marks): the whole line is rasterised in the pixel
        // face instead and slid past the window a cell at a time.
        marquee_pixels(view, state, &text, offset);
    }
}

/// A still line of text drawn from the pixel face, for skin areas whose
/// bitmap font cannot say it: scaled to the area's height, cut at its
/// edge, tinted in the playlist's colour.
pub(crate) fn pixel_line(view: &mut View, state: &mut WinampState, text: &str, area: Area) {
    let ctx = view.ui.ctx().clone();
    let line = state.playlist_text.line(&ctx, text);
    let (texture, width, height) = (line.texture.id(), line.width, line.height);
    let (ink_top, ink_height) = (line.ink_top, line.ink_height);
    if width == 0 || ink_height == 0 {
        return;
    }
    let unit = view.unit;
    // Scale the ink, not the face's padded line, so the glyphs fill the bar.
    let scale = (area.height as f32 * unit) / ink_height as f32;
    let drawn = (width as f32 * scale).min(area.width as f32 * unit);
    let rect = view.rect(area);
    let colour = view.skin.playlist.normal;
    let tint = Color32::from_rgb(colour[0], colour[1], colour[2]);
    let image = egui::Rect::from_min_size(rect.min, vec2(drawn, area.height as f32 * unit));
    let uv = egui::Rect::from_min_max(
        egui::pos2(0.0, ink_top as f32 / height as f32),
        egui::pos2(
            drawn / (width as f32 * scale),
            (ink_top + ink_height) as f32 / height as f32,
        ),
    );
    view.ui.painter().image(texture, image, uv, tint);
}

/// The marquee drawn from the pixel face: the strip is rendered once,
/// scaled to the marquee's height, and a window of it shown, wrapping
/// through the gap the way the character marquee does.
fn marquee_pixels(view: &mut View, state: &mut WinampState, text: &str, offset: usize) {
    let area = layout::MARQUEE;
    let scrolling = state.marquee_scrolling();
    let strip = if scrolling {
        crate::winamp::marquee_strip(text)
    } else {
        text.to_string()
    };
    let ctx = view.ui.ctx().clone();
    let line = state.playlist_text.line(&ctx, &strip);
    let (texture, width, height) = (line.texture.id(), line.width, line.height);
    let (ink_top, ink_height) = (line.ink_top, line.ink_height);
    if width == 0 || ink_height == 0 {
        return;
    }
    let unit = view.unit;
    // Scale the ink, not the face's padded line, so the glyphs fill the bar.
    let scale = (area.height as f32 * unit) / ink_height as f32;
    let strip_width = width as f32 * scale;
    let rect = view.rect(area);
    let painter = view.ui.painter_at(rect.intersect(view.ui.clip_rect()));
    let colour = view.skin.playlist.normal;
    let tint = Color32::from_rgb(colour[0], colour[1], colour[2]);
    let offset_px = if scrolling && strip_width > 0.0 {
        (offset as f32 * 5.0 * unit) % strip_width
    } else {
        0.0
    };
    let uv = egui::Rect::from_min_max(
        egui::pos2(0.0, ink_top as f32 / height as f32),
        egui::pos2(1.0, (ink_top + ink_height) as f32 / height as f32),
    );
    for copy in 0..2 {
        let left = rect.left() - offset_px + copy as f32 * strip_width;
        if left > rect.right() {
            break;
        }
        let image = egui::Rect::from_min_size(
            egui::pos2(left, rect.top()),
            vec2(strip_width, area.height as f32 * unit),
        );
        painter.image(texture, image, uv, tint);
        if !scrolling {
            break;
        }
    }
}

/// The bitrate and sample rate, as far as they are known: ytamp does not
/// report a negotiated bitrate yet, so only the sample rate's stand-in.
fn rates(view: &mut View, host: &mut dyn WinampHost) {
    let playback = host.state();
    if playback.track.is_none() || stopped(playback) {
        return;
    }
    view.text(" 44", layout::KHZ);
}

fn sliders(view: &mut View, state: &mut WinampState, host: &mut dyn WinampHost) {
    // Volume: the track is drawn at the level, the thumb rides on it.
    let volume = host.state().volume.clamp(0.0, 1.0);
    let (response, event) = view.slider(layout::VOLUME, "volume", 14);
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
    let frame = (shown * (sprites::SLIDER_FRAMES - 1) + 50) / 100;
    view.sprite(sprites::volume_frame(frame), layout::VOLUME);
    let thumb = if response.dragged() || response.is_pointer_button_down_on() {
        sprites::VOLUME_THUMB_PRESSED
    } else {
        sprites::VOLUME_THUMB
    };
    let thumb_x = layout::VOLUME.x + (shown * layout::VOLUME_TRAVEL + 50) / 100;
    view.sprite_at(thumb, thumb_x, layout::VOLUME.y + 1);

    // Balance: ytamp's playback state has no balance yet, so the slider
    // previews through the marquee but sends nothing.
    let (response, event) = view.slider(layout::BALANCE, "balance", 14);
    match event {
        SliderEvent::Dragging(value) => {
            state.balance_preview = Some(balance_of(value));
        }
        SliderEvent::Committed(_value) => {
            state.balance_preview = None;
        }
        SliderEvent::None => {}
    }
    let balance = state.balance_preview.unwrap_or(0.0);
    let frame = (balance.abs() * (sprites::SLIDER_FRAMES - 1) as f32).round() as u32;
    view.sprite(sprites::balance_frame(frame), layout::BALANCE);
    let thumb = if response.dragged() || response.is_pointer_button_down_on() {
        sprites::BALANCE_THUMB_PRESSED
    } else {
        sprites::BALANCE_THUMB
    };
    let thumb_x =
        layout::BALANCE.x + ((balance + 1.0) / 2.0 * layout::BALANCE_TRAVEL as f32).round() as u32;
    view.sprite_at(thumb, thumb_x, layout::BALANCE.y + 1);

    // The seek bar. The thumb only exists while something plays, as in
    // Winamp, so an empty or stopped player has nothing to drag.
    view.sprite(sprites::POSITION_TRACK, layout::POSITION);
    let playback = host.state().clone();
    let duration = playback.duration_secs.unwrap_or(0.0);
    if playback.track.is_none() || duration <= 0.0 || stopped(&playback) {
        return;
    }
    let (response, event) = view.slider(layout::POSITION, "position", 29);
    match event {
        SliderEvent::Dragging(value) => state.seek_preview = Some(value as f64),
        SliderEvent::Committed(value) => {
            state.seek_preview = None;
            host.cmd(PlayerCommand::SeekRatio(value as f64));
        }
        SliderEvent::None => {}
    }
    let fraction = state
        .seek_preview
        .unwrap_or(playback.position_secs / duration)
        .clamp(0.0, 1.0) as f32;
    let thumb = if response.dragged() || response.is_pointer_button_down_on() {
        sprites::POSITION_THUMB_PRESSED
    } else {
        sprites::POSITION_THUMB
    };
    let thumb_x = layout::POSITION.x + (fraction * layout::POSITION_TRAVEL as f32).round() as u32;
    view.sprite_at(thumb, thumb_x, layout::POSITION.y);
}

/// A slider position as a balance, -1 to 1, snapping to the centre the
/// way Winamp's did.
pub(crate) fn balance_of(value: f32) -> f32 {
    let balance = value * 2.0 - 1.0;
    if balance.abs() < 0.08 { 0.0 } else { balance }
}

/// The EQ and PL toggles, each lit while its window hangs below.
fn windows_buttons(view: &mut View, state: &mut WinampState, host: &mut dyn WinampHost) {
    let (normal, pressed) = if state.eq_open {
        (sprites::EQ_ON, sprites::EQ_ON_PRESSED)
    } else {
        (sprites::EQ_OFF, sprites::EQ_OFF_PRESSED)
    };
    if view
        .button(layout::EQ_BUTTON, normal, pressed, "equalizer")
        .clicked()
    {
        state.eq_open = !state.eq_open;
        host.toggle_eq_window();
    }
    let (normal, pressed) = if state.playlist_open {
        (sprites::PLAYLIST_ON, sprites::PLAYLIST_ON_PRESSED)
    } else {
        (sprites::PLAYLIST_OFF, sprites::PLAYLIST_OFF_PRESSED)
    };
    if view
        .button(layout::PLAYLIST_BUTTON, normal, pressed, "playlist")
        .clicked()
    {
        state.playlist_open = !state.playlist_open;
        host.toggle_playlist_window();
    }
}

fn transport(view: &mut View, _state: &mut WinampState, host: &mut dyn WinampHost) {
    let playback = host.state().clone();
    let playing = playback.playing;
    if view
        .button(
            layout::PREVIOUS,
            sprites::PREVIOUS,
            sprites::PREVIOUS_PRESSED,
            "previous",
        )
        .clicked()
    {
        host.cmd(PlayerCommand::Prev);
    }
    // Play sits pressed in while the music plays, pause while it waits.
    let play = if playing {
        sprites::PLAY_PRESSED
    } else {
        sprites::PLAY
    };
    if view
        .button(layout::PLAY, play, sprites::PLAY_PRESSED, "play")
        .clicked()
    {
        // Play on a playing song starts it over, as it did.
        if playing {
            host.cmd(PlayerCommand::SeekRatio(0.0));
        } else {
            host.cmd(PlayerCommand::PlayPause);
        }
    }
    let pause = if playback.track.is_some() && !playing && !stopped(&playback) {
        sprites::PAUSE_PRESSED
    } else {
        sprites::PAUSE
    };
    if view
        .button(layout::PAUSE, pause, sprites::PAUSE_PRESSED, "pause")
        .clicked()
        && playback.track.is_some()
    {
        host.cmd(PlayerCommand::PlayPause);
    }
    let stop = if stopped(&playback) {
        sprites::STOP_PRESSED
    } else {
        sprites::STOP
    };
    if view
        .button(layout::STOP, stop, sprites::STOP_PRESSED, "stop")
        .clicked()
        && playback.track.is_some()
    {
        if playing {
            host.cmd(PlayerCommand::PlayPause);
        }
        host.cmd(PlayerCommand::SeekRatio(0.0));
    }
    if view
        .button(layout::NEXT, sprites::NEXT, sprites::NEXT_PRESSED, "next")
        .clicked()
    {
        host.cmd(PlayerCommand::Next);
    }
    if view
        .button(
            layout::EJECT,
            sprites::EJECT,
            sprites::EJECT_PRESSED,
            "eject",
        )
        .on_hover_text("Back to the big window")
        .clicked()
    {
        host.leave_mini_player();
    }
}

fn shuffle_repeat(view: &mut View, state: &mut WinampState, _host: &mut dyn WinampHost) {
    let (normal, pressed) = if state.shuffle {
        (sprites::SHUFFLE_ON, sprites::SHUFFLE_ON_PRESSED)
    } else {
        (sprites::SHUFFLE_OFF, sprites::SHUFFLE_OFF_PRESSED)
    };
    if view
        .button(layout::SHUFFLE, normal, pressed, "shuffle")
        .clicked()
    {
        state.shuffle = !state.shuffle;
    }
    let (normal, pressed) = if state.repeat {
        (sprites::REPEAT_ON, sprites::REPEAT_ON_PRESSED)
    } else {
        (sprites::REPEAT_OFF, sprites::REPEAT_OFF_PRESSED)
    };
    if view
        .button(layout::REPEAT, normal, pressed, "repeat")
        .clicked()
    {
        state.repeat = !state.repeat;
    }
}

/// Seconds as `m:ss`, the way Winamp wrote the time.
pub(crate) fn format_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PlaybackState, Track};

    fn playback(title: &str, artist: &str, duration_secs: f64) -> PlaybackState {
        PlaybackState {
            playing: true,
            track: Some(Track {
                video_id: "x".into(),
                title: title.into(),
                artist: artist.into(),
                album: None,
                duration_secs: Some(duration_secs as u64),
                thumb_url: None,
            }),
            position_secs: 0.0,
            duration_secs: Some(duration_secs),
            volume: 0.5,
            queue: Vec::new(),
            queue_index: None,
        }
    }

    #[test]
    fn the_marquee_names_the_song_the_way_winamp_did() {
        let playing = playback("Karma Police", "Radiohead", 264.0);
        assert_eq!(
            marquee_text(&playing, None, None, None),
            "Radiohead — Karma Police (4:24)"
        );
        assert_eq!(
            marquee_text(&PlaybackState::default(), None, None, None),
            "ytamp"
        );
        let untitled = playback("Episode 12", "", 0.0);
        assert_eq!(marquee_text(&untitled, None, None, None), "Episode 12");
    }

    #[test]
    fn a_seek_in_progress_says_where_it_is_going() {
        let playing = playback("Karma Police", "Radiohead", 264.0);
        assert_eq!(
            marquee_text(&playing, Some(0.5), None, None),
            "Seek to: 2:12/4:24 (50%)"
        );
    }

    #[test]
    fn sliders_announce_themselves_while_they_move() {
        let playing = playback("Karma Police", "Radiohead", 264.0);
        assert_eq!(
            marquee_text(&playing, None, Some(0.57), None),
            "Volume: 57%"
        );
        assert_eq!(
            marquee_text(&playing, None, None, Some(-0.25)),
            "Balance: 25% left"
        );
        assert_eq!(
            marquee_text(&PlaybackState::default(), None, None, Some(0.0)),
            "Balance: center"
        );
        assert_eq!(balance_of(0.5), 0.0);
        assert_eq!(balance_of(0.52), 0.0);
        assert!((balance_of(1.0) - 1.0).abs() < 1e-6);
        assert!(stopped(&PlaybackState {
            playing: false,
            position_secs: 0.0,
            ..playing.clone()
        }));
        assert!(!stopped(&playing));
    }

    #[test]
    fn durations_are_written_the_way_winamp_wrote_them() {
        assert_eq!(format_duration(0.0), "0:00");
        assert_eq!(format_duration(61.0), "1:01");
        assert_eq!(format_duration(264.4), "4:24");
    }
}
