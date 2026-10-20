// SPDX-License-Identifier: GPL-3.0-or-later
//! "Up next": the [`UpNextPrompt`] shown when an episode finishes (the next episode's
//! still, code and title, a [`COUNTDOWN_SECS`] countdown ring that plays it automatically,
//! and Play now / Cancel, then Play next / Replay / Back), and the [`UpNextCard`] shown
//! over the video during the episode's last seconds.
//!
//! The countdown state lives in egui's temp data, keyed by the prompt id and a key the
//! caller derives from the finished episode **and** the finish (`UpNext::key`), so it
//! restarts for every episode and for every replay of one. The card and the prompt share
//! that state: while the card is up, the countdown follows the time left in the episode
//! (so the next one starts when it ends), and hiding the card stops autoplay. Esc
//! should stop the countdown first: the player calls [`cancel`] from `on_back`.

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Rect, Stroke, Ui, pos2, vec2};

use crate::focus;
use crate::services::ImageSlot;
use crate::ui::assets::{self, Asset};
use crate::ui::icons;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::Button;
use crate::ui::{paint, text};

/// Seconds before the next episode starts on its own.
pub const COUNTDOWN_SECS: f64 = 10.0;

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpNextAction {
    /// Nothing yet.
    None,
    /// Play the next episode (button or countdown).
    Play,
    /// Replay the finished episode.
    Replay,
    /// Leave the player.
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct State {
    key: u64,
    started: f64,
    cancelled: bool,
}

fn state_id(id: Id) -> Id {
    id.with("up-next-state")
}

/// Stops a running countdown of prompt `id`. Returns true if one was running.
pub fn cancel(ctx: &egui::Context, id: Id) -> bool {
    let sid = state_id(id);
    let Some(mut s) = ctx.data(|d| d.get_temp::<State>(sid)) else {
        return false;
    };
    if s.cancelled {
        return false;
    }
    s.cancelled = true;
    ctx.data_mut(|d| d.insert_temp(sid, s));
    true
}

/// True if the countdown for `key` was stopped (the card was hidden).
pub fn is_cancelled(ctx: &egui::Context, id: Id, key: u64) -> bool {
    ctx.data(|d| d.get_temp::<State>(state_id(id)))
        .is_some_and(|s| s.key == key && s.cancelled)
}

/// Stops the countdown for `key` (creating its state if needed).
pub fn dismiss(ctx: &egui::Context, id: Id, key: u64) {
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| {
        d.insert_temp(
            state_id(id),
            State {
                key,
                started: now,
                cancelled: true,
            },
        );
    });
}

fn ring(
    painter: &egui::Painter,
    c: egui::Pos2,
    r: f32,
    frac: f32,
    label: &str,
    font: egui::FontId,
) {
    // Opaque disc: nothing of the still (e.g. a play glyph) shows through the digits.
    painter.circle_filled(c, r + 10.0, Color32::from_rgb(0x12, 0x0E, 0x10));
    painter.circle_stroke(c, r, Stroke::new(4.0, color::PROGRESS_TRACK));
    let n = 48;
    let start = -std::f32::consts::FRAC_PI_2;
    let pts: Vec<_> = (0..=n)
        .map(|i| {
            let a = start + std::f32::consts::TAU * frac * (i as f32 / n as f32);
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    if frac > 0.0 {
        painter.add(egui::Shape::line(pts, Stroke::new(4.0, color::ACCENT)));
    }
    painter.text(c, Align2::CENTER_CENTER, label, font, color::TEXT);
}

/// The prompt. See the [module docs](self).
#[derive(Debug)]
pub struct UpNextPrompt<'a> {
    id: Id,
    key: u64,
    code: &'a str,
    title: &'a str,
    series: &'a str,
    image: &'a ImageSlot,
}

impl<'a> UpNextPrompt<'a> {
    /// A prompt; `key` identifies the finish (`UpNext::key`).
    pub fn new(
        id: Id,
        key: u64,
        code: &'a str,
        title: &'a str,
        series: &'a str,
        image: &'a ImageSlot,
    ) -> Self {
        Self {
            id,
            key,
            code,
            title,
            series,
            image,
        }
    }

    /// Draws the prompt centred in `area`.
    pub fn show(self, ui: &mut Ui, area: Rect) -> UpNextAction {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let sid = state_id(self.id);
        let mut st = ctx
            .data(|d| d.get_temp::<State>(sid))
            .filter(|s| s.key == self.key)
            .unwrap_or(State {
                key: self.key,
                started: now,
                cancelled: false,
            });
        let remaining = (COUNTDOWN_SECS - (now - st.started)).max(0.0);
        let counting = !st.cancelled;
        let mut action = UpNextAction::None;
        if counting && remaining <= 0.0 {
            action = UpNextAction::Play;
        }

        let painter = ui.painter().clone();
        let thumb_w = 400.0_f32.min(area.width() - 96.0);
        let thumb = Rect::from_center_size(
            pos2(area.center().x, area.center().y - 60.0),
            vec2(thumb_w, (thumb_w * 9.0 / 16.0).round()),
        );
        // Kicker and series above the still.
        text::overline(
            &painter,
            pos2(thumb.center().x, thumb.top() - 52.0),
            Align2::CENTER_CENTER,
            "Up next",
            color::TEXT_2,
        );
        text::line(
            &painter,
            pos2(thumb.center().x, thumb.top() - 26.0),
            Align2::CENTER_CENTER,
            self.series,
            font::body(),
            color::TEXT_2,
            area.width() - 96.0,
        );
        // Still (cover-fit) or the backdrop placeholder.
        paint::shadow(&painter, thumb, radius::MD, crate::ui::theme::elevation::E2);
        match self.image {
            ImageSlot::Ready(tex) => {
                let uv = paint::cover_uv(tex.size_vec2(), thumb.size(), vec2(0.5, 0.4));
                paint::image_rounded(&painter, tex.id(), thumb, uv, radius::MD, Color32::WHITE);
            }
            _ => {
                if !assets::paint_cover(
                    ui,
                    &painter,
                    Asset::BackdropPlaceholder,
                    thumb,
                    vec2(0.5, 0.5),
                    radius::MD,
                    Color32::WHITE,
                ) {
                    painter.rect_filled(thumb, CornerRadius::same(radius::MD), color::SURFACE_2);
                }
            }
        }
        painter.rect_stroke(
            thumb,
            CornerRadius::same(radius::MD),
            Stroke::new(1.0, color::STROKE_SUBTLE),
            egui::StrokeKind::Inside,
        );
        if counting {
            // Countdown ring in the middle of the still.
            let frac = (1.0 - remaining / COUNTDOWN_SECS) as f32;
            ring(
                &painter,
                thumb.center(),
                36.0,
                frac,
                &format!("{}", remaining.ceil() as u64),
                font::title(),
            );
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Episode line and status.
        let ty = thumb.bottom() + 32.0;
        text::line(
            &painter,
            pos2(thumb.center().x, ty),
            Align2::CENTER_CENTER,
            &format!("{} · {}", self.code, self.title),
            font::title(),
            color::TEXT,
            area.width() - 96.0,
        );
        let status = if counting {
            format!("Starts in {} s", remaining.ceil() as u64)
        } else {
            "Autoplay stopped".to_owned()
        };
        painter.text(
            pos2(thumb.center().x, ty + 30.0),
            Align2::CENTER_CENTER,
            status,
            font::caption(),
            color::TEXT_3,
        );

        // Buttons.
        let row =
            Rect::from_center_size(pos2(area.center().x, ty + 92.0), vec2(area.width(), 52.0));
        let buttons: Vec<Button> = if counting {
            vec![
                Button::primary("Play now")
                    .icon(icons::PLAY)
                    .large()
                    .autofocus()
                    .id(self.id.with("play")),
                Button::secondary("Cancel")
                    .large()
                    .id(self.id.with("cancel")),
            ]
        } else {
            vec![
                Button::primary("Play next")
                    .icon(icons::SKIP_FORWARD)
                    .large()
                    .autofocus()
                    .id(self.id.with("play-next")),
                Button::secondary("Replay")
                    .icon(icons::ARROW_COUNTER_CLOCKWISE)
                    .large()
                    .id(self.id.with("replay")),
                Button::secondary("Back")
                    .icon(icons::CARET_LEFT)
                    .large()
                    .id(self.id.with("back")),
            ]
        };
        let total: f32 = buttons.iter().map(|b| b.size(ui).x).sum::<f32>()
            + 12.0 * (buttons.len().saturating_sub(1)) as f32;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(
                    pos2(row.center().x - total / 2.0, row.top()),
                    vec2(total + 1.0, row.height()),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        focus::scope(&mut child, self.id.with("buttons"), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for (i, b) in buttons.into_iter().enumerate() {
                if b.show(ui).activated {
                    action = match (counting, i) {
                        (true, 0) | (false, 0) => UpNextAction::Play,
                        (true, _) => {
                            st.cancelled = true;
                            UpNextAction::None
                        }
                        (false, 1) => UpNextAction::Replay,
                        (false, _) => UpNextAction::Back,
                    };
                }
            }
        });
        if st.cancelled && counting {
            // The focus was on "Cancel", which is gone now.
            focus::request(&ctx, self.id.with("play-next"));
        }
        ctx.data_mut(|d| d.insert_temp(sid, st));
        action
    }
}

/// What the user did with the [`UpNextCard`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpNextCardAction {
    /// Nothing.
    None,
    /// Play the next episode now.
    Play,
    /// Hide the card (stops autoplay at the end).
    Dismiss,
}

/// The small "Up next" card over the video during an episode's last seconds
/// (bottom-right, `surface-1` @ 96 %, r-lg, e3): still, code · title, the time left on a
/// ring, Play now / Hide. Shares its countdown state with [`UpNextPrompt`] (same `id` and
/// `key`): when the episode ends, the next one starts at once unless the card was hidden.
#[derive(Debug)]
pub struct UpNextCard<'a> {
    id: Id,
    key: u64,
    code: &'a str,
    title: &'a str,
    image: &'a ImageSlot,
    remaining: f64,
    window: f64,
}

impl<'a> UpNextCard<'a> {
    /// The card's size.
    pub const SIZE: egui::Vec2 = vec2(460.0, 122.0);

    /// A card; `remaining` = seconds left in the playing episode, `window` = how long
    /// before the end the card appears (for the ring).
    pub fn new(
        id: Id,
        key: u64,
        code: &'a str,
        title: &'a str,
        image: &'a ImageSlot,
        remaining: f64,
        window: f64,
    ) -> Self {
        Self {
            id,
            key,
            code,
            title,
            image,
            remaining,
            window,
        }
    }

    /// The id of the card's "Play now" button.
    pub fn play_id(id: Id) -> Id {
        id.with("card-play")
    }

    /// Draws the card with its bottom-right corner at `corner`, unless it was hidden for
    /// this key (then it draws nothing and returns [`UpNextCardAction::None`]).
    pub fn show(self, ui: &mut Ui, corner: egui::Pos2) -> UpNextCardAction {
        let ctx = ui.ctx().clone();
        if is_cancelled(&ctx, self.id, self.key) {
            return UpNextCardAction::None;
        }
        let now = ctx.input(|i| i.time);
        // Line the shared countdown up with the end of the episode.
        let started = now - COUNTDOWN_SECS + self.remaining.min(COUNTDOWN_SECS);
        ctx.data_mut(|d| {
            d.insert_temp(
                state_id(self.id),
                State {
                    key: self.key,
                    started,
                    cancelled: false,
                },
            );
        });
        let rect = Rect::from_min_max(corner - Self::SIZE, corner);
        let painter = ui.painter().clone();
        paint::shadow(&painter, rect, radius::LG, crate::ui::theme::elevation::E3);
        painter.rect_filled(
            rect,
            CornerRadius::same(radius::LG),
            color::SURFACE_1.gamma_multiply(0.96),
        );
        let pad = 16.0;
        let thumb = Rect::from_min_size(rect.min + vec2(pad, pad), vec2(160.0, 90.0));
        match self.image {
            ImageSlot::Ready(tex) => {
                let uv = paint::cover_uv(tex.size_vec2(), thumb.size(), vec2(0.5, 0.4));
                paint::image_rounded(&painter, tex.id(), thumb, uv, radius::SM, Color32::WHITE);
            }
            _ => {
                if !assets::paint_cover(
                    ui,
                    &painter,
                    Asset::BackdropPlaceholder,
                    thumb,
                    vec2(0.5, 0.5),
                    radius::SM,
                    Color32::WHITE,
                ) {
                    painter.rect_filled(thumb, CornerRadius::same(radius::SM), color::SURFACE_2);
                }
            }
        }
        let window = self.window.max(1.0);
        let frac = (1.0 - self.remaining / window).clamp(0.0, 1.0) as f32;
        ring(
            &painter,
            thumb.center(),
            20.0,
            frac,
            &format!("{:.0}", self.remaining.ceil().max(0.0)),
            font::body_strong(),
        );
        let tx = thumb.right() + 16.0;
        let tw = rect.right() - pad - tx;
        text::overline(
            &painter,
            pos2(tx, rect.top() + pad + 8.0),
            Align2::LEFT_CENTER,
            "Up next",
            color::TEXT_3,
        );
        text::line(
            &painter,
            pos2(tx, rect.top() + pad + 32.0),
            Align2::LEFT_CENTER,
            &format!("{} \u{b7} {}", self.code, self.title),
            font::body_strong(),
            color::TEXT,
            tw,
        );
        let row = Rect::from_min_max(
            pos2(tx, rect.bottom() - pad - 40.0),
            pos2(rect.right() - pad, rect.bottom() - pad),
        );
        let mut action = UpNextCardAction::None;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        focus::scope(&mut child, self.id.with("card"), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if Button::primary("Play now")
                .icon(icons::PLAY)
                .small()
                .id(Self::play_id(self.id))
                .show(ui)
                .activated
            {
                action = UpNextCardAction::Play;
            }
            if Button::ghost("Hide")
                .small()
                .id(self.id.with("card-hide"))
                .show(ui)
                .activated
            {
                action = UpNextCardAction::Dismiss;
            }
        });
        if action == UpNextCardAction::Dismiss {
            dismiss(&ctx, self.id, self.key);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        action
    }
}
