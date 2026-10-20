// SPDX-License-Identifier: GPL-3.0-or-later
//! The "Up next" prompt shown when an episode finishes: the next episode's still, code
//! and title, a [`COUNTDOWN_SECS`] countdown ring that plays it automatically, and
//! Play now / Cancel (then Play next / Replay / Back).
//!
//! The countdown state lives in egui's temp data, keyed by the prompt id and the episode
//! that finished, so it restarts for every episode. Esc should stop the countdown first:
//! the player calls [`cancel`] from `on_back`.

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
    after: u64,
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

/// The prompt. See the [module docs](self).
#[derive(Debug)]
pub struct UpNextPrompt<'a> {
    id: Id,
    after: u64,
    code: &'a str,
    title: &'a str,
    series: &'a str,
    image: &'a ImageSlot,
}

impl<'a> UpNextPrompt<'a> {
    /// A prompt for the episode after `after` (the finished episode's id).
    pub fn new(
        id: Id,
        after: u64,
        code: &'a str,
        title: &'a str,
        series: &'a str,
        image: &'a ImageSlot,
    ) -> Self {
        Self {
            id,
            after,
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
            .filter(|s| s.after == self.after)
            .unwrap_or(State {
                after: self.after,
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
            let c = thumb.center();
            painter.circle_filled(c, 46.0, Color32::from_black_alpha(215));
            painter.circle_stroke(c, 36.0, Stroke::new(4.0, color::PROGRESS_TRACK));
            let frac = (1.0 - remaining / COUNTDOWN_SECS) as f32;
            let n = 48;
            let start = -std::f32::consts::FRAC_PI_2;
            let pts: Vec<_> = (0..=n)
                .map(|i| {
                    let a = start + std::f32::consts::TAU * frac * (i as f32 / n as f32);
                    c + vec2(a.cos(), a.sin()) * 36.0
                })
                .collect();
            if frac > 0.0 {
                painter.add(egui::Shape::line(pts, Stroke::new(4.0, color::ACCENT)));
            }
            painter.text(
                c,
                Align2::CENTER_CENTER,
                format!("{}", remaining.ceil() as u64),
                font::title(),
                color::TEXT,
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
