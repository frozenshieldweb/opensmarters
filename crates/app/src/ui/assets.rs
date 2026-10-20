// SPDX-License-Identifier: GPL-3.0-or-later
//! Bundled artwork (`assets/`), embedded with `include_image!` and decoded on demand by
//! egui's image loaders (`egui_extras`). Large backgrounds are ~8 MB of RGBA each:
//! screens that show one should call [`forget`] when they are dropped.

use eframe::egui::{
    self, Color32, Image, ImageSource, Painter, Rect, TextureFilter, TextureOptions, Ui, Vec2,
    load::TexturePoll,
};

use crate::ui::paint;

/// A bundled image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asset {
    /// Login/onboarding hero (anchor right).
    Hero,
    /// Ambient Home/browse background.
    HomeAmbient,
    /// Player idle backdrop.
    PlayerIdle,
    /// Poster placeholder (2:3).
    PosterPlaceholder,
    /// Backdrop placeholder (16:9).
    BackdropPlaceholder,
    /// Channel logo fallback (only when the name is empty).
    ChannelLogo,
    /// Brand symbol (champagne → rose).
    Symbol,
    /// Wordmark (cream).
    Wordmark,
    /// Symbol + wordmark.
    Lockup,
    /// App icon 256 px.
    Icon256,
    /// Avatar 1–6 (index 0–5).
    Avatar(u8),
    /// Empty-state illustration.
    Illustration(Illustration),
}

/// Empty/error state illustrations (§6.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Illustration {
    /// "Nothing saved yet".
    EmptyFavourites,
    /// "No matches".
    EmptySearch,
    /// "Can't reach your provider".
    ErrorOffline,
    /// "No guide for this channel".
    EmptyGuide,
    /// "This category is locked".
    ParentalLock,
}

impl Asset {
    /// The image source (`bytes://…` URI).
    pub fn source(self) -> ImageSource<'static> {
        match self {
            Asset::Hero => egui::include_image!("../../../../assets/backgrounds/hero.jpg"),
            Asset::HomeAmbient => {
                egui::include_image!("../../../../assets/backgrounds/home-ambient.jpg")
            }
            Asset::PlayerIdle => {
                egui::include_image!("../../../../assets/backgrounds/player-idle.jpg")
            }
            Asset::PosterPlaceholder => {
                egui::include_image!("../../../../assets/placeholders/poster.jpg")
            }
            Asset::BackdropPlaceholder => {
                egui::include_image!("../../../../assets/placeholders/backdrop.jpg")
            }
            Asset::ChannelLogo => {
                egui::include_image!("../../../../assets/placeholders/channel-logo.png")
            }
            Asset::Symbol => egui::include_image!("../../../../assets/brand/symbol.png"),
            Asset::Wordmark => egui::include_image!("../../../../assets/brand/wordmark.png"),
            Asset::Lockup => egui::include_image!("../../../../assets/brand/lockup.png"),
            Asset::Icon256 => egui::include_image!("../../../../assets/icon/icon-256.png"),
            Asset::Avatar(i) => match i % 6 {
                0 => egui::include_image!("../../../../assets/avatars/avatar-1.jpg"),
                1 => egui::include_image!("../../../../assets/avatars/avatar-2.jpg"),
                2 => egui::include_image!("../../../../assets/avatars/avatar-3.jpg"),
                3 => egui::include_image!("../../../../assets/avatars/avatar-4.jpg"),
                4 => egui::include_image!("../../../../assets/avatars/avatar-5.jpg"),
                _ => egui::include_image!("../../../../assets/avatars/avatar-6.jpg"),
            },
            Asset::Illustration(i) => match i {
                Illustration::EmptyFavourites => {
                    egui::include_image!("../../../../assets/illustrations/empty-favourites.png")
                }
                Illustration::EmptySearch => {
                    egui::include_image!("../../../../assets/illustrations/empty-search.png")
                }
                Illustration::ErrorOffline => {
                    egui::include_image!("../../../../assets/illustrations/error-offline.png")
                }
                Illustration::EmptyGuide => {
                    egui::include_image!("../../../../assets/illustrations/empty-guide.png")
                }
                Illustration::ParentalLock => {
                    egui::include_image!("../../../../assets/illustrations/parental-lock.png")
                }
            },
        }
    }

    /// The `bytes://` URI (for [`forget`]).
    pub fn uri(self) -> String {
        match self.source() {
            ImageSource::Bytes { uri, .. } | ImageSource::Uri(uri) => uri.into_owned(),
            ImageSource::Texture(_) => String::new(),
        }
    }

    fn downscaled(self) -> bool {
        !matches!(self, Asset::Hero | Asset::HomeAmbient | Asset::PlayerIdle)
    }

    /// Loads the texture (if decoded) for drawing at `size`.
    pub fn texture(self, ctx: &egui::Context, size: Vec2) -> Option<egui::load::SizedTexture> {
        let mut img = Image::new(self.source());
        if self.downscaled() {
            img = img.texture_options(TextureOptions {
                mipmap_mode: Some(TextureFilter::Linear),
                ..TextureOptions::LINEAR
            });
        }
        match img.load_for_size(ctx, size) {
            Ok(TexturePoll::Ready { texture }) => Some(texture),
            _ => None,
        }
    }
}

/// Avatar asset for a profile (stable by id).
pub fn avatar_for(seed: &str) -> Asset {
    Asset::Avatar((crate::services::stable_hash(&[seed]) % 6) as u8)
}

/// Paints `asset` cover-fitted into `rect` (anchor 0..1 per axis), rounded, tinted.
/// Returns false while it is still decoding.
pub fn paint_cover(
    ui: &Ui,
    painter: &Painter,
    asset: Asset,
    rect: Rect,
    anchor: Vec2,
    radius: u8,
    tint: Color32,
) -> bool {
    match asset.texture(ui.ctx(), rect.size()) {
        Some(t) => {
            let uv = paint::cover_uv(t.size, rect.size(), anchor);
            paint::image_rounded(painter, t.id, rect, uv, radius, tint);
            true
        }
        None => false,
    }
}

/// Paints `asset` fitted inside `rect` (letterboxed, centred). Returns the painted rect.
pub fn paint_contain(
    ui: &Ui,
    painter: &Painter,
    asset: Asset,
    rect: Rect,
    tint: Color32,
) -> Option<Rect> {
    let t = asset.texture(ui.ctx(), rect.size())?;
    let r = paint::contain_rect(t.size, rect);
    painter.image(
        t.id,
        r,
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        tint,
    );
    Some(r)
}

/// Paints `asset` at `rect` exactly (no aspect correction).
pub fn paint_exact(ui: &Ui, painter: &Painter, asset: Asset, rect: Rect, tint: Color32) -> bool {
    match asset.texture(ui.ctx(), rect.size()) {
        Some(t) => {
            painter.image(
                t.id,
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                tint,
            );
            true
        }
        None => false,
    }
}

/// Frees a decoded bundled image (backgrounds are large).
pub fn forget(ctx: &egui::Context, asset: Asset) {
    ctx.forget_image(&asset.uri());
}

/// Wordmark/lockup aspect (width / height) of the pre-rendered PNGs.
pub fn aspect(asset: Asset) -> f32 {
    match asset {
        Asset::Wordmark => 713.0 / 117.0,
        Asset::Lockup => 856.0 / 129.0,
        _ => 1.0,
    }
}
