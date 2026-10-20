// SPDX-License-Identifier: GPL-3.0-or-later
//! The zap banner: shown for 3 s (200 ms fade) on every live channel change, in the
//! fullscreen player and over the Live TV preview, and when a zap stops at an end of the
//! list ("First channel" / "Last channel").
//!
//! Panel `surface-1` @ 92 %, r-lg, e2, anchored bottom-left inside the safe area:
//! channel number (`title`, 600, tabular), [`ChannelLogo`] 72×44, name (`title-sm`), the
//! current programme with its time range and a 3 pt `accent` progress bar, and the next
//! programme (`caption`). A compact variant fits the preview.

use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{Align2, CornerRadius, Painter, Pos2, Rect, Ui, pos2, vec2};
use opensmarters_core::chrono::{DateTime, Local, Utc};

use crate::playback::ZapEdge;
use crate::screens::AppCtx;
use crate::services::catalog::{LiveChannel, Programme};
use crate::services::{ImageSize, ImageSlot, Prefs, Session};
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::widgets::ChannelLogo;
use crate::ui::{paint, text};

/// What the banner shows.
#[derive(Debug)]
pub struct ZapBanner<'a> {
    /// Channel number.
    pub number: Option<u64>,
    /// Channel name (provider string).
    pub name: &'a str,
    /// Logo.
    pub logo: &'a ImageSlot,
    /// Programme on now.
    pub now: Option<&'a Programme>,
    /// Programme on next.
    pub next: Option<&'a Programme>,
    /// The zap stopped at this end of the list.
    pub edge: Option<ZapEdge>,
    /// Smaller layout (Live TV preview).
    pub compact: bool,
    /// The time for programme progress.
    pub time: DateTime<Utc>,
}

impl ZapBanner<'_> {
    /// Size of the panel for `max_width`.
    pub fn size(&self, max_width: f32) -> eframe::egui::Vec2 {
        if self.compact {
            vec2(max_width.min(520.0), 88.0)
        } else {
            vec2(max_width.min(620.0), 124.0)
        }
    }

    /// Paints the panel with its bottom-left corner at `anchor`, at opacity `alpha`.
    pub fn paint(
        &self,
        ui: &Ui,
        painter: &Painter,
        anchor: Pos2,
        max_width: f32,
        alpha: f32,
        prefs: &Prefs,
    ) -> Rect {
        let size = self.size(max_width);
        let rect = Rect::from_min_size(anchor - vec2(0.0, size.y), size);
        let mut p = painter.clone();
        p.set_opacity(alpha);
        paint::shadow(&p, rect, radius::LG, elevation::E2);
        p.rect_filled(
            rect,
            CornerRadius::same(radius::LG),
            color::SURFACE_1.gamma_multiply(0.92),
        );
        let pad = if self.compact { 14.0 } else { 20.0 };
        let cy = rect.center().y;
        // Number.
        let mut x = rect.left() + pad;
        if let Some(n) = self.number {
            let r = p.text(
                pos2(x, cy),
                Align2::LEFT_CENTER,
                format!("{n:03}"),
                if self.compact {
                    font::title_sm()
                } else {
                    font::title()
                },
                color::TEXT,
            );
            x = r.right() + if self.compact { 12.0 } else { 16.0 };
        }
        // Logo.
        let logo_size = if self.compact {
            vec2(60.0, 36.0)
        } else {
            vec2(72.0, 44.0)
        };
        let logo = Rect::from_min_size(pos2(x, cy - logo_size.y / 2.0), logo_size);
        ChannelLogo::paint(ui, &p, logo, self.name, self.logo);
        let tx = logo.right() + if self.compact { 12.0 } else { 16.0 };
        let right = rect.right() - pad;
        // Edge hint (top-right pill).
        let mut name_right = right;
        if let Some(edge) = self.edge {
            let g = p.layout_no_wrap(
                edge.label().to_owned(),
                font::caption_strong(),
                color::TEXT_ON_ACCENT,
            );
            let pill = Rect::from_min_size(
                pos2(right - g.size().x - 20.0, rect.top() + pad - 2.0),
                g.size() + vec2(20.0, 8.0),
            );
            p.rect_filled(pill, CornerRadius::same(radius::FULL), color::ACCENT);
            p.galley(pill.center() - g.size() / 2.0, g, color::TEXT_ON_ACCENT);
            name_right = pill.left() - 12.0;
        }
        let tw = (right - tx).max(40.0);
        let (name_y, now_y, bar_y, next_y) = if self.compact {
            let top = rect.top() + pad;
            (top + 10.0, top + 34.0, top + 48.0, None)
        } else {
            let top = rect.top() + pad;
            (top + 11.0, top + 38.0, top + 52.0, Some(top + 72.0))
        };
        text::line(
            &p,
            pos2(tx, name_y),
            Align2::LEFT_CENTER,
            self.name,
            if self.compact {
                font::body_strong()
            } else {
                font::title_sm()
            },
            color::TEXT,
            (name_right - tx).max(40.0),
        );
        match self.now {
            Some(prog) => {
                let range = format!(
                    "{}\u{2013}{}",
                    prefs.format_time(&prog.start.with_timezone(&Local)),
                    prefs.format_time(&prog.stop.with_timezone(&Local))
                );
                let rr = p.text(
                    pos2(right, now_y),
                    Align2::RIGHT_CENTER,
                    range,
                    font::caption(),
                    color::TEXT_3,
                );
                text::line(
                    &p,
                    pos2(tx, now_y),
                    Align2::LEFT_CENTER,
                    &prog.title,
                    font::caption(),
                    color::TEXT_2,
                    (rr.left() - 12.0 - tx).max(40.0),
                );
                paint::progress_bar(
                    &p,
                    Rect::from_min_size(pos2(tx, bar_y), vec2(tw, 3.0)),
                    prog.progress(self.time),
                    color::ACCENT,
                    color::PROGRESS_TRACK,
                );
            }
            None => {
                p.text(
                    pos2(tx, now_y),
                    Align2::LEFT_CENTER,
                    "No programme information",
                    font::caption(),
                    color::TEXT_3,
                );
            }
        }
        if let (Some(y), Some(next)) = (next_y, self.next) {
            text::line(
                &p,
                pos2(tx, y),
                Align2::LEFT_CENTER,
                &format!(
                    "Next \u{b7} {} {}",
                    prefs.format_time(&next.start.with_timezone(&Local)),
                    next.title
                ),
                font::caption(),
                color::TEXT_3,
                tw,
            );
        }
        rect
    }
}

/// Draws the banner for the live channel `cx.playback` plays, if it is up: bottom-left
/// at `anchor`, at most `max_width` wide. Returns true while it is visible (keep
/// repainting).
pub fn show(
    cx: &AppCtx<'_>,
    ui: &Ui,
    painter: &Painter,
    anchor: Pos2,
    max_width: f32,
    compact: bool,
) -> bool {
    let Some(banner) = cx.playback.banner() else {
        return false;
    };
    let Some(req) = cx.playback.request() else {
        return false;
    };
    let alpha = banner.alpha(Instant::now());
    let channel: Option<LiveChannel> = cx.playback.live_channel().cloned();
    let (number, name, logo_url) = match (&channel, &req.item) {
        (Some(c), _) => (c.number, c.name.clone(), c.logo.clone()),
        (None, crate::playback::PlaybackItem::Live { number, .. }) => {
            (*number, req.title.clone(), req.artwork.clone())
        }
        _ => return false,
    };
    let session: Option<Arc<Session>> = cx.session();
    let (now_p, next_p) = match (&session, &channel) {
        (Some(s), Some(c)) => {
            let (_, n, x) = s.now_next(c, cx.now);
            (n, x)
        }
        _ => (None, None),
    };
    let logo = cx.services.images.get(logo_url.as_deref(), ImageSize::Logo);
    let b = ZapBanner {
        number,
        name: &name,
        logo: &logo,
        now: now_p.as_ref(),
        next: next_p.as_ref(),
        edge: banner.edge,
        compact,
        time: cx.now,
    };
    b.paint(ui, painter, anchor, max_width, alpha, cx.prefs);
    ui.ctx().request_repaint();
    true
}
