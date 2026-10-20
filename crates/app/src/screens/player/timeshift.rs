// SPDX-License-Identifier: GPL-3.0-or-later
//! Live TV in the player (pause and rewind): the `LIVE` / "−02:13" badge, the rewind bar,
//! ←/→ in time, the ±10 s buttons and "Go live". `player.rs` calls these from a few
//! marked places; the controller side is [`crate::playback::timeshift`].

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Key, Modifiers, Rect, Ui, pos2, vec2};

use super::PlayerScreen;
use crate::focus::{self, FocusOpts};
use crate::playback::timeshift::AT_LIVE;
use crate::playback::{LiveSeek, LiveView};
use crate::screens::AppCtx;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, radius};
use crate::ui::widgets::callout::Badge;
use crate::ui::widgets::{Button, IconButton};
use crate::ui::{paint, text};

/// The live bar's focus id.
pub(crate) fn bar_id() -> Id {
    Id::unique("player.timeshift")
}

/// "−02:13": how far behind live, for the badge.
pub(crate) fn behind_label(secs: f64) -> String {
    let total = if secs.is_finite() {
        secs.max(0.0).round() as u64
    } else {
        0
    };
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("−{h}:{m:02}:{s:02}")
    } else {
        format!("−{m:02}:{s:02}")
    }
}

/// The live badge: `LIVE`, or "−02:13" (warning colour, with a rewind icon) when the
/// viewer is behind live. Used by the player OSD and the Live TV preview.
pub(crate) fn live_badge(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: Align2,
    behind: Option<f64>,
) -> Rect {
    let Some(behind) = behind.filter(|b| *b >= AT_LIVE) else {
        return Badge::live(painter, pos, align);
    };
    let label = behind_label(behind);
    let g = painter.layout_job(text::overline_job(&label, color::WARNING));
    let size = vec2(g.size().x + 16.0 + 20.0, 22.0);
    let rect = align.anchor_size(pos, size);
    painter.rect_filled(
        rect,
        CornerRadius::same(radius::FULL),
        Color32::from_black_alpha(170),
    );
    painter.rect_filled(rect, CornerRadius::same(radius::FULL), color::WARNING_BG);
    icons::paint(
        painter,
        pos2(rect.left() + 15.0, rect.center().y),
        icons::CLOCK_COUNTER_CLOCKWISE,
        14.0,
        Style::Regular,
        color::WARNING,
    );
    painter.galley(
        pos2(rect.left() + 26.0, rect.center().y - g.size().y / 2.0),
        g,
        color::WARNING,
    );
    rect
}

/// What the live OSD bar shows: the rewind window when rewinding is on, or whenever the
/// viewer is behind live; `None` = the programme's progress as before.
pub(super) fn bar_view(cx: &AppCtx<'_>) -> Option<LiveView> {
    cx.playback
        .live_view()
        .filter(|v| cx.playback.can_rewind_live() || !v.is_live())
}

/// Paints the live bar in place of the programme progress. Returns false when there is
/// no rewind window to show.
pub(super) fn paint_bar(cx: &AppCtx<'_>, p: &egui::Painter, bar: Rect) -> bool {
    let Some(v) = bar_view(cx) else {
        return false;
    };
    let focused = focus::is_focused(&cx.egui, bar_id());
    let h = if focused { 10.0 } else { 6.0 };
    let track = Rect::from_center_size(bar.center(), vec2(bar.width(), h));
    let (start, pos) = v.bar();
    let x = |f: f32| track.left() + track.width() * f;
    let r = CornerRadius::same(radius::XS);
    // Track = the window, lighter = what is buffered, champagne = oldest point → position.
    p.rect_filled(track, r, Color32::from_white_alpha(51));
    p.rect_filled(
        Rect::from_min_max(pos2(x(start), track.top()), track.max),
        r,
        Color32::from_white_alpha(89),
    );
    p.rect_filled(
        Rect::from_min_max(pos2(x(start), track.top()), pos2(x(pos), track.bottom())),
        r,
        color::ACCENT,
    );
    if focused {
        p.circle_filled(pos2(x(pos), track.center().y), 10.0, color::CREAM);
    }
    true
}

const BACK: f64 = -10.0;
const FORWARD: f64 = 10.0;
const BACK_LONG: f64 = -60.0;
const FORWARD_LONG: f64 = 60.0;

impl PlayerScreen {
    /// Moves inside the rewind buffer and says what happened.
    pub(super) fn live_seek(&mut self, cx: &mut AppCtx<'_>, delta: f64) {
        match cx.playback.live_seek(delta) {
            LiveSeek::Moved => self.flash(if delta > 0.0 {
                format!("+{delta:.0} s")
            } else {
                format!("{delta:.0} s")
            }),
            LiveSeek::AtLive => self.flash("Live"),
            LiveSeek::AtOldest => self.flash("This is the oldest part kept"),
            LiveSeek::Unavailable => self.flash("Nothing to rewind yet"),
        }
    }

    /// "Go live" (L, the OSD button).
    pub(super) fn go_live(&mut self, cx: &mut AppCtx<'_>) {
        if cx.playback.go_live() {
            self.flash("Live");
        }
    }

    /// ←/→ (Shift: 60 s) with the OSD hidden on live TV: move in time when rewinding is
    /// on, and show the OSD with the live bar focused so more arrows keep moving. Returns
    /// false when rewinding is off (the arrows change channel then).
    pub(super) fn live_arrows(
        &mut self,
        cx: &mut AppCtx<'_>,
        ctx: &egui::Context,
        keys: [bool; 4],
    ) -> bool {
        if !cx.playback.can_rewind_live() {
            return false;
        }
        let step = match keys {
            [true, ..] => BACK,
            [_, true, ..] => FORWARD,
            [_, _, true, _] => BACK_LONG,
            [_, _, _, true] => FORWARD_LONG,
            _ => return true,
        };
        self.live_seek(cx, step);
        focus::request(ctx, bar_id());
        self.poke();
        true
    }

    /// ←/→ while the live bar has the focus.
    pub(super) fn live_bar_keys(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        let step = ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                BACK
            } else if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                FORWARD
            } else if i.consume_key(Modifiers::SHIFT, Key::ArrowLeft) {
                BACK_LONG
            } else if i.consume_key(Modifiers::SHIFT, Key::ArrowRight) {
                FORWARD_LONG
            } else {
                0.0
            }
        });
        if step != 0.0 {
            self.live_seek(cx, step);
        }
    }

    /// The live bar as a control (inside the OSD's focus scope): focusable, a click
    /// jumps there.
    pub(super) fn live_bar(
        &mut self,
        ui: &mut Ui,
        cx: &mut AppCtx<'_>,
        p: &egui::Painter,
        bar: Rect,
    ) {
        let Some(view) = bar_view(cx) else { return };
        let hit = bar.expand2(vec2(0.0, 12.0));
        let (resp, f) = focus::focusable(ui, bar_id(), hit, FocusOpts::default());
        if f.focused {
            paint::focus_ring(p, hit, radius::SM, 1.0);
        }
        if resp.clicked()
            && let Some(ptr) = resp.interact_pointer_pos()
        {
            let frac = f64::from(((ptr.x - bar.left()) / bar.width()).clamp(0.0, 1.0));
            self.live_seek(cx, view.time_at(frac) - view.pos);
        }
    }

    /// The "back 10 s" button of the live transport row (rewinding on). Returns the next
    /// free x.
    pub(super) fn live_back_button(
        &mut self,
        ui: &mut Ui,
        cx: &mut AppCtx<'_>,
        x: f32,
        cy: f32,
    ) -> f32 {
        if !cx.playback.can_rewind_live() {
            return x;
        }
        let r = Rect::from_center_size(pos2(x + 24.0, cy), vec2(48.0, 48.0));
        if IconButton::new(icons::ARROW_COUNTER_CLOCKWISE)
            .size(48.0, 24.0)
            .caption("10")
            .id(Id::unique("player.live-back10"))
            .show_at(ui, r)
            .activated
        {
            self.live_seek(cx, BACK);
        }
        x + 48.0 + 24.0
    }

    /// After play/pause on live TV: "forward 10 s" (rewinding on) and "Go live" (behind
    /// live).
    pub(super) fn live_forward_controls(
        &mut self,
        ui: &mut Ui,
        cx: &mut AppCtx<'_>,
        mut x: f32,
        cy: f32,
    ) {
        if cx.playback.can_rewind_live() {
            let r = Rect::from_center_size(pos2(x + 24.0, cy), vec2(48.0, 48.0));
            if IconButton::new(icons::ARROW_CLOCKWISE)
                .size(48.0, 24.0)
                .caption("10")
                .id(Id::unique("player.live-fwd10"))
                .show_at(ui, r)
                .activated
            {
                self.live_seek(cx, FORWARD);
            }
            x += 48.0 + 24.0;
        }
        if bar_view(cx).is_some_and(|v| !v.is_live()) {
            let r = Rect::from_min_size(pos2(x, cy - 22.0), vec2(180.0, 44.0));
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(r)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            if Button::secondary("Go live")
                .icon(icons::BROADCAST)
                .id(Id::unique("player.golive"))
                .show(&mut child)
                .activated
            {
                self.go_live(cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behind_labels() {
        assert_eq!(behind_label(133.0), "−02:13");
        assert_eq!(behind_label(3725.4), "−1:02:05");
        assert_eq!(behind_label(-5.0), "−00:00");
        assert_eq!(behind_label(f64::NAN), "−00:00");
    }
}
