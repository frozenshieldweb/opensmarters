// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider text hygiene: everything a provider sends is returned as **plain text**,
//! capped, with control characters, bidi overrides and zero-width tricks removed (the
//! UI renders it as text, never as markup).

/// Cap for names and titles.
pub const MAX_NAME: usize = 200;
/// Cap for descriptions and plots.
pub const MAX_PARAGRAPH: usize = 2_000;
/// Cap for short fields (genre, cast, director).
pub const MAX_FIELD: usize = 400;

fn is_bidi_or_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// One line: controls (including newlines) become spaces, runs of spaces collapse,
/// trimmed, at most `max` characters (an ellipsis marks a cut).
pub fn line(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut count = 0;
    let mut space = false;
    for c in s.chars() {
        if is_bidi_or_invisible(c) {
            continue;
        }
        let c = if c.is_control() || c.is_whitespace() {
            ' '
        } else {
            c
        };
        if c == ' ' {
            if space || out.is_empty() {
                continue;
            }
            space = true;
        } else {
            space = false;
        }
        if count == max {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('…');
            return out;
        }
        out.push(c);
        count += 1;
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// A paragraph: like [`line`] but newlines are kept (at most two in a row).
pub fn paragraph(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut count = 0;
    let mut newlines = 0;
    for c in s.replace("\r\n", "\n").chars() {
        if is_bidi_or_invisible(c) {
            continue;
        }
        let c = if c == '\n' {
            '\n'
        } else if c.is_control() {
            ' '
        } else {
            c
        };
        if c == '\n' {
            newlines += 1;
            if newlines > 2 || out.is_empty() {
                continue;
            }
        } else {
            newlines = 0;
        }
        if count == max {
            out.push('…');
            return out.trim().to_owned();
        }
        out.push(c);
        count += 1;
    }
    out.trim().to_owned()
}

/// `None` for empty results.
pub fn opt_line(s: Option<&str>, max: usize) -> Option<String> {
    s.map(|s| line(s, max)).filter(|s| !s.is_empty())
}

/// `None` for empty results.
pub fn opt_paragraph(s: Option<&str>, max: usize) -> Option<String> {
    s.map(|s| paragraph(s, max)).filter(|s| !s.is_empty())
}

/// A four-digit year from a release date or a title like `"Name (2003)"`.
pub fn year(release_date: Option<&str>, name: &str) -> Option<i32> {
    let from_date = release_date.and_then(|d| {
        let d = d.trim();
        d.get(..4)
            .filter(|y| y.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|y| y.parse().ok())
    });
    from_date
        .or_else(|| {
            let n = name.trim_end();
            let inner = n.strip_suffix(')')?;
            let open = inner.rfind('(')?;
            let y = inner[open + 1..].trim();
            (y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit()))
                .then(|| y.parse().ok())
                .flatten()
        })
        .filter(|y| (1880..=2200).contains(y))
}

/// A rating on a 0–10 scale, rounded to one decimal; `None` for missing or nonsense.
pub fn rating10(rating: Option<f64>, rating5: Option<f64>) -> Option<f64> {
    let r = rating.filter(|r| r.is_finite() && *r > 0.0).or_else(|| {
        rating5
            .filter(|r| r.is_finite() && *r > 0.0)
            .map(|r| r * 2.0)
    })?;
    (r <= 10.0).then(|| (r * 10.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        assert_eq!(line("  a\n\tb  c ", 50), "a b c");
        assert_eq!(line("abc\u{202E}def", 50), "abcdef");
        assert_eq!(line("abcdef", 3), "abc…");
        assert_eq!(line("<b>x</b>", 50), "<b>x</b>", "markup stays text");
        assert_eq!(line("\u{0}\u{7}x", 50), "x");
    }

    #[test]
    fn paragraphs() {
        assert_eq!(paragraph("a\r\nb\n\n\n\nc", 100), "a\nb\n\nc");
        assert_eq!(paragraph("abcdef", 3), "abc…");
    }

    #[test]
    fn years_and_ratings() {
        assert_eq!(year(Some("2003-05-01"), "x"), Some(2003));
        assert_eq!(year(None, "Title (1999)"), Some(1999));
        assert_eq!(year(None, "Title"), None);
        assert_eq!(rating10(Some(7.25), None), Some(7.3));
        assert_eq!(rating10(None, Some(4.0)), Some(8.0));
        assert_eq!(rating10(Some(0.0), None), None);
        assert_eq!(rating10(Some(f64::NAN), None), None);
    }
}
