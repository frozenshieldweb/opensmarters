// SPDX-License-Identifier: GPL-3.0-or-later
//! Channel logos on plates / monogram tiles (§6.4) and profile avatars (§6.15).

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Image, Painter, Rect, Sense, Ui, pos2, vec2,
};

use crate::focus::{self, Focus, FocusOpts};
use crate::services::ImageSlot;
use crate::services::catalog::monogram;
use crate::ui::assets::{self, Asset};
use crate::ui::motion;
use crate::ui::theme::{color, font, radius};
use crate::ui::{paint, text};

/// A channel logo: the provider logo on a `surface-3` plate, else a monogram tile with a
/// stable muted hue, else the placeholder symbol.
#[derive(Debug)]
pub struct ChannelLogo;

impl ChannelLogo {
    /// Paints a logo for `name` into `rect`.
    pub fn paint(ui: &Ui, painter: &Painter, rect: Rect, name: &str, image: &ImageSlot) {
        let r = CornerRadius::same(radius::SM);
        if let ImageSlot::Ready(tex) = image {
            painter.rect_filled(rect, r, color::SURFACE_3);
            let inner = rect.shrink(8.0_f32.min(rect.height() * 0.15));
            let size = tex.size_vec2();
            let target = paint::contain_rect(size, inner);
            painter.image(
                tex.id(),
                target,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            return;
        }
        let mono = monogram(name);
        if mono.is_empty() {
            painter.rect_filled(rect, r, color::SURFACE_3);
            assets::paint_contain(
                ui,
                painter,
                Asset::ChannelLogo,
                rect.shrink(6.0),
                Color32::WHITE,
            );
            return;
        }
        let hue = color::MONOGRAM[(crate::services::stable_hash(&[name]) % 6) as usize];
        painter.rect_filled(rect, r, hue);
        let size = (rect.height() * 0.5).clamp(12.0, 28.0);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            mono,
            egui::FontId::new(
                size,
                egui::FontFamily::Name(crate::ui::theme::family::UI_600.into()),
            ),
            color::TEXT,
        );
    }
}

/// A round profile avatar (136 pt in the picker) with focus ring and 1.08 lift.
#[derive(Debug)]
pub struct Avatar<'a> {
    id: Id,
    asset: Asset,
    name: &'a str,
    status: Option<(&'a str, Color32)>,
    size: f32,
    autofocus: bool,
}

impl<'a> Avatar<'a> {
    /// An avatar tile with a name below.
    pub fn new(id: Id, asset: Asset, name: &'a str) -> Self {
        Self {
            id,
            asset,
            name,
            status: None,
            size: 136.0,
            autofocus: false,
        }
    }

    /// A status line under the name (e.g. "Expired" in danger).
    #[must_use]
    pub fn status(mut self, label: &'a str, c: Color32) -> Self {
        self.status = Some((label, c));
        self
    }

    /// Diameter.
    #[must_use]
    pub fn size(mut self, d: f32) -> Self {
        self.size = d;
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self, on: bool) -> Self {
        self.autofocus = on;
        self
    }

    /// Adds the tile (circle + 12 pt + name + optional status).
    pub fn show(self, ui: &mut Ui) -> Focus {
        let w = self.size + 24.0;
        let h = self.size + 12.0 + 26.0 + if self.status.is_some() { 20.0 } else { 0.0 };
        let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let circle = Rect::from_center_size(
            pos2(rect.center().x, rect.top() + self.size / 2.0),
            vec2(self.size, self.size),
        );
        let (_, f) = focus::focusable(
            ui,
            self.id,
            circle,
            FocusOpts::default().autofocus_if(self.autofocus),
        );
        let painter = ui.painter().clone();
        let lift = if motion::reduced(ui.ctx()) {
            0.0
        } else {
            0.08 * f.anim
        };
        let c = circle.expand(self.size * lift / 2.0);
        if f.anim > 0.0 {
            paint::focus_glow(&painter, c, radius::FULL, f.anim);
        }
        let img = Image::new(self.asset.source()).corner_radius(CornerRadius::same(radius::FULL));
        img.paint_at(ui, c);
        if f.focused {
            paint::focus_ring_circle(&painter, c.center(), c.width() / 2.0, 1.0);
        }
        let name_y = circle.bottom() + 12.0 + 13.0 + self.size * lift / 2.0;
        let fg = if f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        };
        text::line(
            &painter,
            pos2(rect.center().x, name_y),
            Align2::CENTER_CENTER,
            self.name,
            font::title_sm(),
            fg,
            w + 40.0,
        );
        if let Some((label, c)) = self.status {
            text::line(
                &painter,
                pos2(rect.center().x, name_y + 22.0),
                Align2::CENTER_CENTER,
                label,
                font::caption(),
                c,
                w + 40.0,
            );
        }
        f
    }
}
