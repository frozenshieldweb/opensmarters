// SPDX-License-Identifier: GPL-3.0-or-later
//! The stream details panel (D, or the ⓘ OSD button): what is playing and how the
//! connection is doing. Values come from mpv's observed properties
//! ([`opensmarters_player::PlayerState`]), the controller's counters
//! ([`crate::playback::StreamHealth`]) and the connection budget. Codec and decoder names
//! come from the media; the player crate strips control characters and caps them.

use eframe::egui::{self, Align2, CornerRadius, Rect, pos2, vec2};
use opensmarters_player::{PlayerState, TrackKind};

use crate::playback::{LiveView, StreamHealth};
use crate::screens::AppCtx;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::{paint, text};

/// "4.2 Mbit/s", "128 kbit/s".
fn bitrate(bits_per_sec: f64) -> String {
    if bits_per_sec >= 1.0e6 {
        format!("{:.1} Mbit/s", bits_per_sec / 1.0e6)
    } else {
        format!("{:.0} kbit/s", bits_per_sec / 1.0e3)
    }
}

/// "1 s", "2.5 s".
fn seconds(secs: f64) -> String {
    if (secs - secs.round()).abs() < 0.05 {
        format!("{secs:.0} s")
    } else {
        format!("{secs:.1} s")
    }
}

/// The rows (label, value).
pub(super) fn rows(
    st: &PlayerState,
    health: &StreamHealth,
    view: Option<&LiveView>,
    connections: Option<(u32, u32)>,
) -> Vec<(&'static str, String)> {
    let dash = || "—".to_owned();
    let mut video = match (st.source_width, st.source_height) {
        (Some(w), Some(h)) => format!("{w}×{h}"),
        _ => dash(),
    };
    if let Some(fps) = st.fps {
        if (fps - fps.round()).abs() < 0.01 {
            video.push_str(&format!(" · {fps:.0} fps"));
        } else {
            video.push_str(&format!(" · {fps:.2} fps"));
        }
    }
    if let Some(c) = &st.video_codec {
        video.push_str(&format!(" · {c}"));
    }
    let decoding = match (&st.hwdec_current, st.has_video()) {
        (Some(hw), _) => format!("Hardware ({hw})"),
        (None, true) => "Software".to_owned(),
        (None, false) => dash(),
    };
    let mut audio = st.audio_codec.clone().unwrap_or_else(dash);
    if let Some(ch) = st.selected(TrackKind::Audio).and_then(|t| t.channels) {
        audio.push_str(&format!(" · {ch} channels"));
    }
    let rate = match (st.video_bitrate, st.audio_bitrate) {
        (Some(v), Some(a)) => format!("video {} · audio {}", bitrate(v), bitrate(a)),
        (Some(v), None) => format!("video {}", bitrate(v)),
        (None, Some(a)) => format!("audio {}", bitrate(a)),
        (None, None) => dash(),
    };
    let ahead = st
        .demuxer_cache_duration
        .map_or_else(dash, |s| format!("{} ahead", seconds(s)));
    let buffered = match view {
        Some(v) => format!(
            "{ahead} · {} to rewind",
            text::timecode((v.pos - v.oldest).max(0.0))
        ),
        None => ahead,
    };
    let speed = st.cache_speed.map_or_else(dash, |b| bitrate(b * 8.0));
    let drops = match (st.frame_drops, st.decoder_frame_drops) {
        (None, None) => dash(),
        (vo, dec) => format!("{} (decoder {})", vo.unwrap_or(0), dec.unwrap_or(0)),
    };
    let mut rows = vec![
        ("Video", video),
        ("Decoding", decoding),
        ("Audio", audio),
        ("Bitrate", rate),
        ("Buffered", buffered),
        ("Download speed", speed),
        ("Dropped frames", drops),
        (
            "Reconnects",
            format!(
                "{} · {} this session",
                health.reconnects, health.reconnects_total
            ),
        ),
    ];
    if let Some((used, limit)) = connections {
        rows.push(("Connections", format!("{used} of {limit} in use")));
    }
    let start = health.start_buffer.as_secs_f64();
    rows.push((
        "Start buffer",
        if start > 0.0 {
            seconds(start)
        } else {
            "none".to_owned()
        },
    ));
    rows
}

/// Paints the panel (top left, under the OSD's top bar; it stays when the OSD hides).
pub(super) fn panel(cx: &AppCtx<'_>, painter: &egui::Painter) {
    let st = cx.playback.state();
    let health = cx.playback.health();
    let view = cx.playback.live_view();
    let connections = cx.session().map(|s| {
        let b = s.connections();
        (b.in_use(), b.limit())
    });
    let rows = rows(&st, &health, view.as_ref(), connections);
    let safe = cx.layout.safe_area();
    let left = if cx.layout.tv {
        safe.left() + 24.0
    } else {
        cx.layout.screen.left() + 48.0
    };
    let row_h = 26.0;
    let extra = health.extra_buffering;
    let height = 58.0 + rows.len() as f32 * row_h + if extra { row_h + 8.0 } else { 0.0 };
    let panel = Rect::from_min_size(pos2(left, safe.top() + 104.0), vec2(400.0, height));
    paint::shadow(painter, panel, radius::LG, elevation::E2);
    painter.rect_filled(
        panel,
        CornerRadius::same(radius::LG),
        color::SURFACE_1.gamma_multiply(0.94),
    );
    let x = panel.left() + 20.0;
    let mut y = panel.top() + 28.0;
    text::line(
        painter,
        pos2(x, y),
        Align2::LEFT_CENTER,
        "Stream details",
        font::title_sm(),
        color::TEXT,
        panel.width() - 120.0,
    );
    painter.text(
        pos2(panel.right() - 20.0, y),
        Align2::RIGHT_CENTER,
        "D to close",
        font::caption(),
        color::TEXT_3,
    );
    y += 32.0;
    for (label, value) in &rows {
        painter.text(
            pos2(x, y),
            Align2::LEFT_CENTER,
            *label,
            font::caption(),
            color::TEXT_3,
        );
        text::line(
            painter,
            pos2(x + 124.0, y),
            Align2::LEFT_CENTER,
            value,
            font::caption(),
            color::TEXT,
            panel.width() - 124.0 - 40.0,
        );
        y += row_h;
    }
    if extra {
        y += 4.0;
        icons::paint(
            painter,
            pos2(x + 8.0, y),
            icons::INFO,
            16.0,
            Style::Regular,
            color::INFO,
        );
        text::line(
            painter,
            pos2(x + 24.0, y),
            Align2::LEFT_CENTER,
            "Extra buffering for this channel (it stalled before)",
            font::caption(),
            color::INFO,
            panel.width() - 64.0,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use opensmarters_player::{CacheState, SeekRange};

    use super::*;

    fn value<'a>(rows: &'a [(&'static str, String)], label: &str) -> &'a str {
        rows.iter()
            .find(|(l, _)| *l == label)
            .map_or("<missing>", |(_, v)| v.as_str())
    }

    #[test]
    fn rows_read_like_sentences() {
        let st = PlayerState {
            source_width: Some(1920),
            source_height: Some(1080),
            fps: Some(25.0),
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            video_bitrate: Some(4_200_000.0),
            audio_bitrate: Some(128_000.0),
            cache_speed: Some(640_000.0),
            frame_drops: Some(2),
            decoder_frame_drops: None,
            demuxer_cache_duration: Some(3.24),
            hwdec_current: Some("d3d11va".into()),
            time_pos: Some(100.0),
            cache: CacheState {
                seekable_ranges: vec![SeekRange {
                    start: 0.0,
                    end: 103.0,
                }],
                ..CacheState::default()
            },
            ..PlayerState::default()
        };
        let health = StreamHealth {
            reconnects: 1,
            reconnects_total: 3,
            start_buffer: Duration::from_secs(3),
            extra_buffering: true,
        };
        let view = LiveView {
            behind: 0.0,
            pos: 100.0,
            oldest: 25.0,
            live: 100.0,
            window: 1800.0,
        };
        let r = rows(&st, &health, Some(&view), Some((2, 3)));
        assert_eq!(value(&r, "Video"), "1920×1080 · 25 fps · h264");
        assert_eq!(value(&r, "Decoding"), "Hardware (d3d11va)");
        assert_eq!(value(&r, "Bitrate"), "video 4.2 Mbit/s · audio 128 kbit/s");
        assert_eq!(value(&r, "Buffered"), "3.2 s ahead · 1:15 to rewind");
        assert_eq!(value(&r, "Download speed"), "5.1 Mbit/s");
        assert_eq!(value(&r, "Dropped frames"), "2 (decoder 0)");
        assert_eq!(value(&r, "Reconnects"), "1 · 3 this session");
        assert_eq!(value(&r, "Connections"), "2 of 3 in use");
        assert_eq!(value(&r, "Start buffer"), "3 s");

        let empty = rows(
            &PlayerState::default(),
            &StreamHealth::default(),
            None,
            None,
        );
        assert_eq!(value(&empty, "Video"), "—");
        assert_eq!(value(&empty, "Decoding"), "—");
        assert_eq!(value(&empty, "Start buffer"), "none");
        assert_eq!(value(&empty, "Connections"), "<missing>");
    }
}
