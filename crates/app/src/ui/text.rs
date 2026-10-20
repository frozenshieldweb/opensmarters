// SPDX-License-Identifier: GPL-3.0-or-later
//! Text helpers: capping provider strings (security rule: cap before layout), laying
//! out truncated/ellipsised text, overline (uppercase + tracking) runs.

use std::borrow::Cow;
use std::sync::Arc;

use eframe::egui::{
    Align, Align2, Color32, FontId, Galley, Painter, Pos2, Rect, text::LayoutJob, text::TextFormat,
    text::TextWrapping,
};

/// Default cap for provider strings shown in one line (names, titles).
pub const MAX_LINE_CHARS: usize = 160;
/// Default cap for provider paragraphs (synopsis, descriptions).
pub const MAX_PARAGRAPH_CHARS: usize = 2_000;

/// Decorative symbols providers put in names that none of our fonts (nor the OS
/// fallback) draw, mapped to glyphs we do have, so they never show as "tofu" boxes.
fn map_symbol(c: char) -> Option<&'static str> {
    Some(match c {
        // Stars → Phosphor's star (reachable through the text fonts' fallback chain).
        '\u{2605}' | '\u{2606}' | '\u{2726}' | '\u{2727}' | '\u{2729}'..='\u{2732}' => {
            crate::ui::icons::STAR
        }
        // Heavy arrows → a triangle that the fallback fonts have.
        '\u{2794}' | '\u{2798}'..='\u{27AF}' | '\u{27B1}'..='\u{27BE}' => "\u{25BA}",
        _ => return None,
    })
}

fn needs_mapping(c: char) -> bool {
    c.is_control() || is_bidi_control(c) || map_symbol(c).is_some()
}

/// Caps a provider string at `max_chars` characters, replaces control characters
/// (including newlines and bidi overrides) with spaces and maps decorative symbols our
/// fonts lack (★, ➤…). Returns the input unchanged when nothing needs fixing.
pub fn cap(s: &str, max_chars: usize) -> Cow<'_, str> {
    let needs_fix = s.chars().any(needs_mapping);
    let too_long = s.len() > max_chars && s.chars().nth(max_chars).is_some();
    if !needs_fix && !too_long {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len().min(max_chars.saturating_mul(4)));
    for c in s.chars().take(max_chars) {
        if c.is_control() || is_bidi_control(c) {
            out.push(' ');
        } else if let Some(m) = map_symbol(c) {
            out.push_str(m);
        } else {
            out.push(c);
        }
    }
    if too_long {
        out.push('…');
    }
    Cow::Owned(out)
}

/// Like [`cap`] but keeps newlines (for multi-line synopses).
pub fn cap_paragraph(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars.saturating_mul(4)));
    for c in s.chars().take(max_chars) {
        if c == '\n' {
            out.push('\n');
        } else if c.is_control() || is_bidi_control(c) {
            out.push(' ');
        } else if let Some(m) = map_symbol(c) {
            out.push_str(m);
        } else {
            out.push(c);
        }
    }
    out
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// A layout job for `text` truncated to `max_rows` lines of `max_width` points, with an
/// ellipsis on overflow. `max_rows == 0` means unlimited.
pub fn job(text: &str, font: FontId, color: Color32, max_width: f32, max_rows: usize) -> LayoutJob {
    let mut job = LayoutJob::single_section(
        cap(text, MAX_PARAGRAPH_CHARS).into_owned(),
        TextFormat::simple(font, color),
    );
    job.wrap = TextWrapping {
        max_width: max_width.max(1.0),
        max_rows: if max_rows == 0 { usize::MAX } else { max_rows },
        break_anywhere: max_rows == 1,
        overflow_character: Some('…'),
    };
    job
}

/// Lays out text (see [`job`]).
pub fn galley(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
    max_rows: usize,
) -> Arc<Galley> {
    painter.layout_job(job(text, font, color, max_width, max_rows))
}

/// Paints one truncated line anchored at `pos` with `align`. Returns the text rect.
pub fn line(
    painter: &Painter,
    pos: Pos2,
    align: Align2,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> Rect {
    let g = galley(painter, text, font, color, max_width, 1);
    let rect = align.anchor_size(pos, g.size());
    painter.galley(rect.min, g, color);
    rect
}

/// Paints wrapped text (at most `max_rows`) with its top-left at `pos`. Returns the rect.
pub fn block(
    painter: &Painter,
    pos: Pos2,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
    max_rows: usize,
) -> Rect {
    let g = galley(painter, text, font, color, max_width, max_rows);
    let rect = Rect::from_min_size(pos, g.size());
    painter.galley(pos, g, color);
    rect
}

/// An overline run (§4.7): UPPERCASE, 12 pt 600, +8% letter-spacing.
pub fn overline_job(text: &str, color: Color32) -> LayoutJob {
    let mut fmt = TextFormat::simple(crate::ui::theme::font::overline(), color);
    fmt.extra_letter_spacing = 1.0;
    let mut job = LayoutJob::single_section(cap(text, MAX_LINE_CHARS).to_uppercase(), fmt);
    job.halign = Align::LEFT;
    job
}

/// Paints an overline anchored at `pos`. Returns its rect.
pub fn overline(painter: &Painter, pos: Pos2, align: Align2, text: &str, color: Color32) -> Rect {
    let g = painter.layout_job(overline_job(text, color));
    let rect = align.anchor_size(pos, g.size());
    painter.galley(rect.min, g, color);
    rect
}

/// Width of one line of text in `font`.
pub fn width(painter: &Painter, text: &str, font: FontId) -> f32 {
    painter
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x
}

/// "1 h 52" / "42 min" from seconds.
pub fn duration_short(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return String::new();
    }
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    if h > 0 {
        format!("{h} h {m:02}")
    } else {
        format!("{} min", m.max(1))
    }
}

/// "42:10" / "1:02:13" timecode.
pub fn timecode(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".to_owned();
    }
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "1 h 06 remaining" / "26 min remaining".
pub fn remaining(secs: f64) -> String {
    let d = duration_short(secs);
    if d.is_empty() {
        String::new()
    } else {
        format!("{d} remaining")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_strips_controls_and_bidi() {
        assert_eq!(cap("Plain", 10), "Plain");
        assert_eq!(cap("a\nb\u{202E}c", 10), "a b c");
        assert_eq!(cap("abcdef", 3), "abc…");
        assert_eq!(
            cap("\u{2605} Top \u{27A4}", 20),
            format!("{} Top \u{25BA}", crate::ui::icons::STAR)
        );
        assert_eq!(
            cap_paragraph("a\nb\u{2606}", 10),
            format!("a\nb{}", crate::ui::icons::STAR)
        );
    }

    #[test]
    fn timecodes() {
        assert_eq!(timecode(2530.0), "42:10");
        assert_eq!(timecode(3733.0), "1:02:13");
        assert_eq!(duration_short(6720.0), "1 h 52");
        assert_eq!(duration_short(1560.0), "26 min");
        assert_eq!(timecode(f64::NAN), "--:--");
    }
}
