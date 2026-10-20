// SPDX-License-Identifier: GPL-3.0-or-later
//! Trailers: the app never embeds web content. A provider's `youtube_trailer` (validated
//! to a bare 11-character id by [`crate::services::vod::youtube_id`]) is opened in the
//! system browser, only after an explicit confirmation that says where it goes.

use eframe::egui::{self, Id};

use crate::screens::AppCtx;
use crate::services::vod;
use crate::ui::icons;
use crate::ui::widgets::{Button, Dialog};

/// The only URLs this module will ever hand to the OS.
const ALLOWED_PREFIX: &str = "https://www.youtube.com/watch?v=";

/// The confirmation dialog state.
#[derive(Debug, Default)]
pub(super) struct TrailerPrompt {
    video: Option<String>,
}

impl TrailerPrompt {
    /// Asks before opening the trailer `video_id` (already validated).
    pub(super) fn ask(&mut self, video_id: String) {
        self.video = Some(video_id);
    }

    /// Closes the dialog (Esc). Returns true if it was open.
    pub(super) fn close(&mut self) -> bool {
        self.video.take().is_some()
    }

    /// Shows the dialog while open.
    pub(super) fn show(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        let Some(video) = self.video.clone() else {
            return;
        };
        let mut open = false;
        let mut cancel = false;
        Dialog::new(
            Id::unique("detail.trailer"),
            "Watch the trailer on YouTube?",
        )
        .body(
            "The trailer opens in your web browser on youtube.com, outside opensmarters. \
                 YouTube may collect data about your visit.",
        )
        .show(ctx, |ui| {
            if Button::primary("Open in browser")
                .icon(icons::ARROW_SQUARE_OUT)
                .id(Id::unique("detail.trailer.open"))
                .show(ui)
                .activated
            {
                open = true;
            }
            if Button::secondary("Cancel")
                .id(Id::unique("detail.trailer.cancel"))
                .autofocus()
                .show(ui)
                .activated
            {
                cancel = true;
            }
        });
        if open {
            self.video = None;
            let url = vod::youtube_url(&video);
            cx.services.rt().spawn_blocking(move || {
                if let Err(e) = open_in_browser(&url) {
                    tracing::warn!(error = %e, "couldn't open the browser");
                }
            });
            cx.toasts.info("Opening the trailer in your browser…");
        } else if cancel {
            self.video = None;
        }
    }
}

/// Opens a YouTube watch URL in the default browser. Anything else is refused.
///
/// # Errors
///
/// The URL isn't an allowed YouTube watch page, or the OS launcher failed to start.
pub(super) fn open_in_browser(url: &str) -> std::io::Result<()> {
    let id = url.strip_prefix(ALLOWED_PREFIX).unwrap_or_default();
    if vod::youtube_id(id).as_deref() != Some(id) || id.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a YouTube watch URL",
        ));
    }
    // The URL's characters are limited to the fixed prefix plus [A-Za-z0-9_-]; the system
    // launcher is started by absolute path with the URL as its single argument (no shell,
    // no PATH lookup: SR-16).
    crate::platform::open_url(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_youtube_watch_urls_are_opened() {
        for bad in [
            "https://evil.example/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&x=1",
            "https://www.youtube.com/watch?v=",
            "file:///C:/Windows/System32/calc.exe",
            "https://www.youtube.com/watch?v=abc\" & calc",
        ] {
            assert!(open_in_browser(bad).is_err(), "{bad}");
        }
    }
}
