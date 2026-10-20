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

/// Phosphor glyphs live in the Unicode Private Use Area.
fn is_icon_char(c: char) -> bool {
    ('\u{E000}'..='\u{F8FF}').contains(&c)
}

/// Appends `s` to `job`, drawing Phosphor glyphs (e.g. the rating star) with the icon
/// font explicitly rather than through the text font's fallback chain, which can resolve
/// a Private Use Area code point to another font's glyph or to a "tofu" box. The star is
/// drawn filled (it stands for ★).
pub fn append_with_icons(job: &mut LayoutJob, s: &str, font: &FontId, color: Color32) {
    let text_fmt = || TextFormat::simple(font.clone(), color);
    if !s.chars().any(is_icon_char) {
        job.append(s, 0.0, text_fmt());
        return;
    }
    let mut run_start = 0;
    for (i, c) in s.char_indices() {
        if !is_icon_char(c) {
            continue;
        }
        if i > run_start {
            job.append(&s[run_start..i], 0.0, text_fmt());
        }
        let end = i + c.len_utf8();
        let glyph = &s[i..end];
        let icon_font = if glyph == crate::ui::icons::STAR {
            crate::ui::icons::fill(font.size * 0.92)
        } else {
            crate::ui::icons::regular(font.size)
        };
        let mut fmt = TextFormat::simple(icon_font, color);
        // Centre the glyph on the text line (Phosphor sits low next to Source Sans).
        fmt.valign = Align::Center;
        job.append(glyph, 0.0, fmt);
        run_start = end;
    }
    if run_start < s.len() {
        job.append(&s[run_start..], 0.0, text_fmt());
    }
}

/// A layout job for `text` truncated to `max_rows` lines of `max_width` points, with an
/// ellipsis on overflow. `max_rows == 0` means unlimited. Phosphor glyphs in the text
/// (ratings ★) are drawn with the icon font ([`append_with_icons`]).
pub fn job(text: &str, font: FontId, color: Color32, max_width: f32, max_rows: usize) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_with_icons(&mut job, &cap(text, MAX_PARAGRAPH_CHARS), &font, color);
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
    fn icon_glyphs_use_the_icon_font() {
        let font = FontId::proportional(13.0);
        let s = format!("2004 · {} 7.4", crate::ui::icons::STAR);
        let j = job(&s, font.clone(), Color32::WHITE, 200.0, 1);
        assert_eq!(j.sections.len(), 3);
        assert_eq!(j.sections[0].format.font_id, font);
        assert_eq!(
            j.sections[1].format.font_id,
            crate::ui::icons::fill(13.0 * 0.92),
            "the rating star is drawn with Phosphor Fill, never via fallback"
        );
        assert_eq!(j.text, s);
        let plain = job("No icons", font, Color32::WHITE, 200.0, 1);
        assert_eq!(plain.sections.len(), 1);
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
