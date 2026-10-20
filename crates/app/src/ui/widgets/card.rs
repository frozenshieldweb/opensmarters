// SPDX-License-Identifier: GPL-3.0-or-later
//! Cards (§6.3): poster (2:3), backdrop (16:9, continue watching / episodes), channel
//! tile (live now). Focus = 1.06 lift, ring, glow, `e1`; title/meta revealed.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Painter, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
    pos2, vec2,
};

use crate::focus::{self, Focus, FocusOpts};
use crate::services::ImageSlot;
use crate::ui::assets::{self, Asset};
use crate::ui::motion;
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::widgets::callout::Badge;
use crate::ui::widgets::logo::ChannelLogo;
use crate::ui::{paint, text};

fn lifted(ui: &Ui, rect: Rect, anim: f32) -> Rect {
    if motion::reduced(ui.ctx()) {
        rect
    } else {
        rect.expand2(rect.size() * 0.03 * anim)
    }
}

fn paint_texture_cover(
    painter: &Painter,
    tex: &egui::TextureHandle,
    rect: Rect,
    r: u8,
    tint: Color32,
) {
    let uv = paint::cover_uv(tex.size_vec2(), rect.size(), vec2(0.5, 0.3));
    paint::image_rounded(painter, tex.id(), rect, uv, r, tint);
}

/// The look-ahead used when a card scrolls into view (part of the next card peeks).
fn card_scroll_margin(size: Vec2) -> Vec2 {
    vec2(size.x * 0.6 + 16.0, 64.0)
}

/// A 2:3 poster card with title and meta below.
#[derive(Debug)]
pub struct PosterCard<'a> {
    id: Id,
    image: &'a ImageSlot,
    title: &'a str,
    meta: Option<&'a str>,
    progress: Option<f32>,
    width: f32,
    meta_on_focus_only: bool,
    autofocus: bool,
}

impl<'a> PosterCard<'a> {
    /// A poster card `width` wide (height 1.5×).
    pub fn new(id: Id, image: &'a ImageSlot, title: &'a str, width: f32) -> Self {
        Self {
            id,
            image,
            title,
            meta: None,
            progress: None,
            width,
            meta_on_focus_only: false,
            autofocus: false,
        }
    }

    /// Meta line ("2026 · ★ 7.4").
    #[must_use]
    pub fn meta(mut self, meta: Option<&'a str>) -> Self {
        self.meta = meta;
        self
    }

    /// Watch progress 0..1.
    #[must_use]
    pub fn progress(mut self, p: Option<f32>) -> Self {
        self.progress = p;
        self
    }

    /// TV mode: show meta only when focused.
    #[must_use]
    pub fn meta_on_focus_only(mut self, on: bool) -> Self {
        self.meta_on_focus_only = on;
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self, on: bool) -> Self {
        self.autofocus = on;
        self
    }

    /// Total height including the text below.
    pub fn height(width: f32) -> f32 {
        (width * 1.5).round() + 10.0 + 22.0 + 20.0
    }

    /// Adds the card.
    pub fn show(self, ui: &mut Ui) -> Focus {
        let img_h = (self.width * 1.5).round();
        let (rect, _) =
            ui.allocate_exact_size(vec2(self.width, Self::height(self.width)), Sense::hover());
        let img_rect = Rect::from_min_size(rect.min, vec2(self.width, img_h));
        let (resp, f) = focus::focusable(
            ui,
            self.id,
            img_rect,
            FocusOpts::default()
                .autofocus_if(self.autofocus)
                .scroll_margin2(card_scroll_margin(img_rect.size())),
        );
        let painter = ui.painter().clone();
        let pressed = resp.is_pointer_button_down_on();
        let mut r = lifted(ui, img_rect, f.anim);
        if pressed && !motion::reduced(ui.ctx()) {
            r = r.shrink2(r.size() * 0.01);
        }
        if f.anim > 0.0 {
            paint::shadow(&painter, r, radius::MD, elevation::E1);
            paint::focus_glow(&painter, r, radius::MD, f.anim);
        }
        match self.image {
            ImageSlot::Ready(tex) => {
                paint_texture_cover(&painter, tex, r, radius::MD, Color32::WHITE)
            }
            ImageSlot::Loading => feedback_skeleton(ui, &painter, r, radius::MD),
            ImageSlot::None | ImageSlot::Failed => {
                if !assets::paint_cover(
                    ui,
                    &painter,
                    Asset::PosterPlaceholder,
                    r,
                    vec2(0.5, 0.5),
                    radius::MD,
                    Color32::WHITE,
                ) {
                    painter.rect_filled(r, CornerRadius::same(radius::MD), color::SURFACE_2);
                }
                let g = text::galley(
                    &painter,
                    self.title,
                    crate::ui::theme::font::display_sized(20.0),
                    color::TEXT_2,
                    r.width() - 24.0,
                    3,
                );
                let pos = r.center() - g.size() / 2.0;
                painter.galley(pos, g, color::TEXT_2);
            }
        }
        painter.rect_stroke(
            r,
            CornerRadius::same(radius::MD),
            Stroke::new(1.0, color::STROKE_SUBTLE),
            StrokeKind::Inside,
        );
        if let Some(p) = self.progress {
            let bar = Rect::from_min_size(
                pos2(r.left() + 10.0, r.bottom() - 12.0),
                vec2(r.width() - 20.0, 4.0),
            );
            paint::progress_bar(&painter, bar, p, color::ACCENT, color::PROGRESS_TRACK);
        }
        if f.focused {
            paint::focus_ring(&painter, r, radius::MD, 1.0);
        }
        let ty = r.bottom() + 10.0;
        let (font_t, fg) = if f.focused {
            (font::title_sm(), color::TEXT)
        } else {
            (font::body_strong(), color::TEXT_2)
        };
        text::line(
            &painter,
            pos2(r.left(), ty),
            Align2::LEFT_TOP,
            self.title,
            font_t,
            fg,
            r.width() + 8.0,
        );
        if let Some(meta) = self.meta
            && (!self.meta_on_focus_only || f.focused)
        {
            text::line(
                &painter,
                pos2(r.left(), ty + 24.0),
                Align2::LEFT_TOP,
                meta,
                font::caption(),
                color::TEXT_3,
                r.width(),
            );
        }
        f
    }
}

fn feedback_skeleton(ui: &Ui, painter: &Painter, rect: Rect, r: u8) {
    crate::ui::widgets::feedback::skeleton(ui.ctx(), painter, rect, r);
}

/// A 16:9 backdrop card with the title inside (Continue watching, episodes).
#[derive(Debug)]
pub struct BackdropCard<'a> {
    id: Id,
    image: &'a ImageSlot,
    title: &'a str,
    meta: Option<&'a str>,
    progress: Option<f32>,
    size: Vec2,
    autofocus: bool,
}

impl<'a> BackdropCard<'a> {
    /// A backdrop card.
    pub fn new(id: Id, image: &'a ImageSlot, title: &'a str, size: Vec2) -> Self {
        Self {
            id,
            image,
            title,
            meta: None,
            progress: None,
            size,
            autofocus: false,
        }
    }

    /// Meta ("1 h 06 remaining").
    #[must_use]
    pub fn meta(mut self, meta: Option<&'a str>) -> Self {
        self.meta = meta;
        self
    }

    /// Progress 0..1.
    #[must_use]
    pub fn progress(mut self, p: Option<f32>) -> Self {
        self.progress = p;
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self, on: bool) -> Self {
        self.autofocus = on;
        self
    }

    /// Adds the card.
    pub fn show(self, ui: &mut Ui) -> Focus {
        let (rect, _) = ui.allocate_exact_size(self.size, Sense::hover());
        let (_, f) = focus::focusable(
            ui,
            self.id,
            rect,
            FocusOpts::default()
                .autofocus_if(self.autofocus)
                .scroll_margin2(card_scroll_margin(rect.size())),
        );
        let painter = ui.painter().clone();
        let r = lifted(ui, rect, f.anim);
        if f.anim > 0.0 {
            paint::shadow(&painter, r, radius::MD, elevation::E1);
            paint::focus_glow(&painter, r, radius::MD, f.anim);
        }
        match self.image {
            ImageSlot::Ready(tex) => {
                paint_texture_cover(&painter, tex, r, radius::MD, Color32::WHITE)
            }
            ImageSlot::Loading => feedback_skeleton(ui, &painter, r, radius::MD),
            _ => {
                if !assets::paint_cover(
                    ui,
                    &painter,
                    Asset::BackdropPlaceholder,
                    r,
                    vec2(0.5, 0.5),
                    radius::MD,
                    Color32::WHITE,
                ) {
                    painter.rect_filled(r, CornerRadius::same(radius::MD), color::SURFACE_2);
                }
            }
        }
        // Scrim for the text (rounded bottom corners kept by an inset gradient).
        let scrim = Rect::from_min_max(pos2(r.left(), r.top() + r.height() * 0.35), r.max);
        paint::v_gradient(
            &painter,
            scrim.shrink2(vec2(0.5, 0.0)),
            &[
                (0.0, Color32::from_black_alpha(0)),
                (0.55, Color32::from_black_alpha(150)),
                (1.0, Color32::from_black_alpha(215)),
            ],
        );
        painter.rect_stroke(
            r,
            CornerRadius::same(radius::MD),
            Stroke::new(1.0, color::STROKE_SUBTLE),
            StrokeKind::Inside,
        );
        let has_progress = self.progress.is_some();
        let bottom_pad = if has_progress { 24.0 } else { 14.0 };
        let mut y = r.bottom() - bottom_pad;
        if let Some(meta) = self.meta {
            text::line(
                &painter,
                pos2(r.left() + 12.0, y),
                Align2::LEFT_BOTTOM,
                meta,
                font::caption(),
                color::TEXT_2,
                r.width() - 24.0,
            );
            y -= 20.0;
        }
        text::line(
            &painter,
            pos2(r.left() + 12.0, y),
            Align2::LEFT_BOTTOM,
            self.title,
            font::body_strong(),
            color::TEXT,
            r.width() - 24.0,
        );
        if let Some(p) = self.progress {
            let bar = Rect::from_min_size(
                pos2(r.left() + 12.0, r.bottom() - 14.0),
                vec2(r.width() - 24.0, 4.0),
            );
            paint::progress_bar(&painter, bar, p, color::ACCENT, color::PROGRESS_TRACK);
        }
        if f.focused {
            paint::focus_ring(&painter, r, radius::MD, 1.0);
        }
        f
    }
}

/// A live channel tile (Live now): logo plate, "● LIVE", now line and progress.
#[derive(Debug)]
pub struct ChannelTile<'a> {
    id: Id,
    logo: &'a ImageSlot,
    name: &'a str,
    now: Option<&'a str>,
    progress: Option<f32>,
    size: Vec2,
}

impl<'a> ChannelTile<'a> {
    /// A tile.
    pub fn new(id: Id, logo: &'a ImageSlot, name: &'a str, size: Vec2) -> Self {
        Self {
            id,
            logo,
            name,
            now: None,
            progress: None,
            size,
        }
    }

    /// Current programme title.
    #[must_use]
    pub fn now(mut self, now: Option<&'a str>) -> Self {
        self.now = now;
        self
    }

    /// Programme progress 0..1.
    #[must_use]
    pub fn progress(mut self, p: Option<f32>) -> Self {
        self.progress = p;
        self
    }

    /// Adds the tile.
    pub fn show(self, ui: &mut Ui) -> Focus {
        let (rect, _) = ui.allocate_exact_size(self.size, Sense::hover());
        let (_, f) = focus::focusable(
            ui,
            self.id,
            rect,
            FocusOpts::default().scroll_margin2(card_scroll_margin(rect.size())),
        );
        let painter = ui.painter().clone();
        let r = lifted(ui, rect, f.anim);
        if f.anim > 0.0 {
            paint::shadow(&painter, r, radius::MD, elevation::E1);
            paint::focus_glow(&painter, r, radius::MD, f.anim);
        }
        painter.rect_filled(
            r,
            CornerRadius::same(radius::MD),
            if f.focused {
                color::SURFACE_3
            } else {
                color::SURFACE_2
            },
        );
        Badge::live_dot(&painter, pos2(r.left() + 12.0, r.top() + 15.0));
        // Logo plate in the 120×68 box (the logo itself fills ~85 % of it).
        let logo_h = (r.height() * 0.46).round();
        let logo = Rect::from_center_size(
            pos2(r.center().x, r.top() + 8.0 + logo_h / 2.0),
            vec2(logo_h * 120.0 / 68.0, logo_h),
        );
        ChannelLogo::paint(ui, &painter, logo, self.name, self.logo);
        // Name (body-strong) and programme (caption) on two lines.
        text::line(
            &painter,
            pos2(r.left() + 12.0, r.bottom() - 42.0),
            Align2::LEFT_CENTER,
            self.name,
            font::body_strong(),
            color::TEXT,
            r.width() - 24.0,
        );
        if let Some(n) = self.now.filter(|n| !n.is_empty()) {
            text::line(
                &painter,
                pos2(r.left() + 12.0, r.bottom() - 22.0),
                Align2::LEFT_CENTER,
                n,
                font::caption(),
                if f.focused {
                    color::TEXT
                } else {
                    color::TEXT_2
                },
                r.width() - 24.0,
            );
        }
        let bar = Rect::from_min_size(
            pos2(r.left() + 12.0, r.bottom() - 10.0),
            vec2(r.width() - 24.0, 3.0),
        );
        paint::progress_bar(
            &painter,
            bar,
            self.progress.unwrap_or(0.0),
            color::ACCENT,
            color::PROGRESS_TRACK,
        );
        if f.focused {
            paint::focus_ring(&painter, r, radius::MD, 1.0);
        }
        f
    }
}
