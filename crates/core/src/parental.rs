// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyword matcher for the parental (adult-category) lock.
//!
//! Matching is **whole-word, case-insensitive and Unicode-aware**:
//! - both the text and the keywords are NFKC-normalised and lower-cased (so fullwidth
//!   `ＸＸＸ` and `XXX` match `xxx`);
//! - a keyword matches only where it is not glued to other letters, digits or combining
//!   marks: "Sussex", "Essex", "Middlesex" do **not** match `sex`; "XXX Movies",
//!   "Adult|Night", "adult_hd", "[18+]" do match;
//! - a keyword edge that is itself punctuation (the `+` of `18+`) needs no boundary on
//!   that side.
//!
//! No regex is built from keywords, so user-supplied keywords cannot break matching.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Default keywords.
pub const DEFAULT_KEYWORDS: [&str; 6] = ["adult", "adults", "xxx", "porn", "sex", "18+"];

/// Whole-word keyword matcher. See the [module docs](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordMatcher {
    /// (normalised keyword, original keyword)
    keywords: Vec<(String, String)>,
}

impl Default for KeywordMatcher {
    fn default() -> Self {
        Self::new(DEFAULT_KEYWORDS)
    }
}

fn normalise(s: &str) -> String {
    s.nfkc().collect::<String>().to_lowercase()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || is_combining_mark(c)
}

impl KeywordMatcher {
    /// A matcher for `keywords` (trimmed; empty ones ignored; duplicates removed).
    pub fn new<I, S>(keywords: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out: Vec<(String, String)> = Vec::new();
        for k in keywords {
            let original = k.as_ref().trim();
            let norm = normalise(original);
            if norm.is_empty() || out.iter().any(|(n, _)| *n == norm) {
                continue;
            }
            out.push((norm, original.to_owned()));
        }
        KeywordMatcher { keywords: out }
    }

    /// The keywords, as given.
    pub fn keywords(&self) -> impl Iterator<Item = &str> {
        self.keywords.iter().map(|(_, o)| o.as_str())
    }

    /// `true` if any keyword occurs as a whole word in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        self.matching_keyword(text).is_some()
    }

    /// The first keyword (as given) that occurs as a whole word in `text`.
    pub fn matching_keyword(&self, text: &str) -> Option<&str> {
        if self.keywords.is_empty() || text.is_empty() {
            return None;
        }
        let hay = normalise(text);
        self.keywords
            .iter()
            .find(|(needle, _)| contains_whole_word(&hay, needle))
            .map(|(_, original)| original.as_str())
    }
}

fn contains_whole_word(hay: &str, needle: &str) -> bool {
    let needs_left = needle.chars().next().is_some_and(is_word_char);
    let needs_right = needle.chars().next_back().is_some_and(is_word_char);
    hay.match_indices(needle).any(|(idx, m)| {
        let left_ok = !needs_left || !hay[..idx].chars().next_back().is_some_and(is_word_char);
        let right_ok = !needs_right
            || !hay[idx + m.len()..]
                .chars()
                .next()
                .is_some_and(is_word_char);
        left_ok && right_ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keywords_whole_word() {
        let m = KeywordMatcher::default();
        for yes in [
            "XXX Movies",
            "Adult|Night",
            "ADULTS ONLY",
            "adult_hd",
            "VOD - Porn",
            "[18+] Late",
            "18+",
            "Sex & Drama",
            "ＸＸＸ",
            "Adult\u{00A0}TV",
            "Adultes, adult",
        ] {
            assert!(m.is_match(yes), "{yes:?} should match");
        }
        for no in [
            "Sussex",
            "Essex",
            "Middlesex",
            "Sussex News",
            "BBC Essex",
            "Pornography",
            "Adulthood",
            "118+",
            "Sexy",
            "News",
            "",
            "Sexé",
            "Sex\u{0301}",
        ] {
            assert!(!m.is_match(no), "{no:?} should not match");
        }
        assert_eq!(m.matching_keyword("Late XXX"), Some("xxx"));
    }

    #[test]
    fn custom_keywords() {
        let m = KeywordMatcher::new([" Érotique ", "", "C++", "(", "érotique"]);
        assert_eq!(
            m.keywords().collect::<Vec<_>>(),
            vec!["Érotique", "C++", "("]
        );
        assert!(m.is_match("Films ÉROTIQUE"));
        assert!(!m.is_match("érotiques"));
        assert!(m.is_match("learn c++ now"));
        assert!(m.is_match("x(y")); // punctuation keyword: no boundary needed
        assert!(!KeywordMatcher::new(Vec::<String>::new()).is_match("anything"));
    }
}
