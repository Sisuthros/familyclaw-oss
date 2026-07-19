//! Absolutization of relative dates.
//!
//! The `absolutize_dates` phase (design §2.3, Anthropic Dreaming) solves a
//! concrete family pain point: the memory "`agent_a` left **yesterday**"
//! becomes meaningless the very next day unless "yesterday" is anchored to
//! a calendar date. This module replaces relative date words with absolute
//! ISO dates (`YYYY-MM-DD`) relative to the dream cycle's reference instant.
//!
//! The matching is intentionally **conservative**: only clear,
//! unambiguous date words are replaced, and the replacement only applies to
//! whole words (not parts of words). Unknown ⇒ the text is left as-is
//! (CLAUDE.md core value: don't guess).

use chrono::{Datelike, Duration};
use familyclaw_core::Timestamp;

/// One known relative date word and its offset from the reference date.
struct RelativeWord {
    /// The word as it appears in text (lowercased).
    word: &'static str,
    /// Offset in days from the reference date (`-1` = yesterday, `+1` = tomorrow).
    offset_days: i64,
}

/// Known relative date words (Finnish + English).
///
/// The list is intentionally narrow and unambiguous — "today/tänään"
/// doesn't change meaning when anchored to a calendar date, so it's
/// included for completeness (offset 0).
const RELATIVE_WORDS: &[RelativeWord] = &[
    RelativeWord {
        word: "eilen",
        offset_days: -1,
    },
    RelativeWord {
        word: "yesterday",
        offset_days: -1,
    },
    RelativeWord {
        word: "tänään",
        offset_days: 0,
    },
    RelativeWord {
        word: "today",
        offset_days: 0,
    },
    RelativeWord {
        word: "huomenna",
        offset_days: 1,
    },
    RelativeWord {
        word: "tomorrow",
        offset_days: 1,
    },
    RelativeWord {
        word: "toissapäivänä",
        offset_days: -2,
    },
    RelativeWord {
        word: "ylihuomenna",
        offset_days: 2,
    },
];

/// Formats a date in ISO form (`YYYY-MM-DD`), shifted from the reference instant.
///
/// The shift is applied at day granularity from the reference instant's
/// calendar date. If the shift would overflow the date range chrono can
/// represent, the reference date is returned unchanged (panic-free fallback).
fn shifted_iso(reference: Timestamp, offset_days: i64) -> String {
    let base = reference.date_naive();
    let shifted = base
        .checked_add_signed(Duration::days(offset_days))
        .unwrap_or(base);
    format!(
        "{:04}-{:02}-{:02}",
        shifted.year(),
        shifted.month(),
        shifted.day()
    )
}

/// The result of absolutizing a single piece of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsolutizeResult {
    /// The possibly rewritten text.
    pub text: String,
    /// How many date words were replaced.
    pub replacements: usize,
}

impl AbsolutizeResult {
    /// Whether the text changed.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.replacements > 0
    }
}

/// Whether a character is a word boundary (not alphanumeric).
///
/// Used to detect whole words, so that e.g. "todays" or "yesterdays" don't
/// match partially. Unicode-aware, so ä/ö work correctly.
fn is_word_boundary(c: char) -> bool {
    !c.is_alphanumeric()
}

/// A lowercased copy of a single character, when the conversion is 1:1.
///
/// Nearly all recognized letters (ASCII a-z + Finnish ä/ö/å) convert to a
/// single character. In rare cases where the conversion would expand
/// (e.g. certain special characters), the original character is returned —
/// in that case it simply won't match a lowercase date word, which is a
/// safe outcome.
fn lower_char(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(first), None) => first,
        _ => c,
    }
}

/// Replaces whole occurrences of the word `needle` (case-insensitive) with
/// the string `replacement`. Returns (text, replacement count).
///
/// Only replaces occurrences surrounded by word boundaries, so partial
/// matches (e.g. "todays" for "today") are left untouched. The comparison
/// is done character-by-character in lowercase, but **the preserved
/// characters are taken from the original text** — so the original letter
/// case and other text (e.g. placeholders) remain unchanged.
fn replace_whole_word(haystack: &str, needle: &str, replacement: &str) -> (String, usize) {
    if needle.is_empty() {
        return (haystack.to_string(), 0);
    }
    // Original characters (preserved) + their lowercase versions (for comparison).
    let orig: Vec<char> = haystack.chars().collect();
    let lower: Vec<char> = orig.iter().map(|&c| lower_char(c)).collect();
    let needle_chars: Vec<char> = needle.chars().map(lower_char).collect();
    let n = needle_chars.len();

    let mut out = String::with_capacity(haystack.len() + replacement.len());
    let mut count = 0_usize;
    let mut i = 0_usize;
    while i < orig.len() {
        let window_matches = i + n <= orig.len() && lower[i..i + n] == needle_chars[..];
        if window_matches {
            let left_ok = i == 0 || is_word_boundary(lower[i - 1]);
            let right_ok = i + n == orig.len() || is_word_boundary(lower[i + n]);
            if left_ok && right_ok {
                out.push_str(replacement);
                count += 1;
                i += n;
                continue;
            }
        }
        out.push(orig[i]);
        i += 1;
    }
    (out, count)
}

/// Replaces all known relative date words with absolute ISO dates relative
/// to the `reference` instant.
///
/// The replacement form is `<word> (YYYY-MM-DD)`, so the original phrasing
/// is preserved for readability while the absolute date is pinned down.
/// E.g. `"lähti eilen"` → `"lähti eilen (2026-06-03)"`.
///
/// A word that's already absolutized (immediately followed by
/// `(YYYY-MM-DD)`) is skipped, so a repeated dream cycle is idempotent —
/// the same memory doesn't accumulate dates.
#[must_use]
pub fn absolutize(text: &str, reference: Timestamp) -> AbsolutizeResult {
    let mut current = text.to_string();
    let mut total = 0_usize;

    for rw in RELATIVE_WORDS {
        let iso = shifted_iso(reference, rw.offset_days);
        let replacement = format!("{} ({iso})", rw.word);
        // Idempotence: if the word is already followed by exactly this
        // annotation, don't replace it again. Achieved by first replacing
        // the finished form with a placeholder, replacing the rest, and
        // then restoring the placeholder.
        let sentinel = "\u{0}DREAM_DATE\u{0}";
        let already = format!("{} ({iso})", rw.word);
        let guarded = current.replace(already.as_str(), sentinel);
        let (replaced, count) = replace_whole_word(&guarded, rw.word, &replacement);
        current = replaced.replace(sentinel, already.as_str());
        total += count;
    }

    AbsolutizeResult {
        text: current,
        replacements: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// Fixed reference instant for tests: 2026-06-04 (UTC).
    fn reference() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
            .single()
            .expect("valid reference instant")
    }

    #[test]
    fn shifted_iso_handles_offsets() {
        let r = reference();
        assert_eq!(shifted_iso(r, 0), "2026-06-04");
        assert_eq!(shifted_iso(r, -1), "2026-06-03");
        assert_eq!(shifted_iso(r, 1), "2026-06-05");
        assert_eq!(shifted_iso(r, -2), "2026-06-02");
        assert_eq!(shifted_iso(r, 2), "2026-06-06");
    }

    #[test]
    fn shifted_iso_crosses_month_boundary() {
        let r = Utc
            .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
            .single()
            .expect("valid");
        assert_eq!(shifted_iso(r, -1), "2026-05-31");
    }

    #[test]
    fn absolutize_finnish_yesterday() {
        let r = reference();
        let res = absolutize("agent_a lähti eilen kotiin", r);
        assert!(res.changed());
        assert_eq!(res.replacements, 1);
        assert_eq!(res.text, "agent_a lähti eilen (2026-06-03) kotiin");
    }

    #[test]
    fn absolutize_english_tomorrow() {
        let r = reference();
        let res = absolutize("the deploy ships tomorrow", r);
        assert_eq!(res.replacements, 1);
        assert_eq!(res.text, "the deploy ships tomorrow (2026-06-05)");
    }

    #[test]
    fn absolutize_today_offset_zero() {
        let r = reference();
        let res = absolutize("we shipped it today", r);
        assert_eq!(res.text, "we shipped it today (2026-06-04)");
    }

    #[test]
    fn absolutize_multiple_words() {
        let r = reference();
        let res = absolutize("started yesterday, finishing tomorrow", r);
        assert_eq!(res.replacements, 2);
        assert!(res.text.contains("yesterday (2026-06-03)"));
        assert!(res.text.contains("tomorrow (2026-06-05)"));
    }

    #[test]
    fn absolutize_is_case_insensitive() {
        let r = reference();
        let res = absolutize("Yesterday it rained", r);
        assert_eq!(res.replacements, 1);
        // The word is normalized to lowercase in the replacement.
        assert!(res.text.contains("yesterday (2026-06-03)"));
    }

    #[test]
    fn absolutize_does_not_touch_partial_words() {
        let r = reference();
        // "yesterdays" is not a whole "yesterday" → left untouched.
        let res = absolutize("yesterdays news", r);
        assert_eq!(res.replacements, 0);
        assert_eq!(res.text, "yesterdays news");
    }

    #[test]
    fn absolutize_no_relative_word_is_unchanged() {
        let r = reference();
        let res = absolutize("a plain factual statement", r);
        assert!(!res.changed());
        assert_eq!(res.text, "a plain factual statement");
    }

    #[test]
    fn absolutize_is_idempotent() {
        let r = reference();
        let once = absolutize("left eilen", r);
        assert_eq!(once.replacements, 1);
        let twice = absolutize(&once.text, r);
        // A second run does not add a new date.
        assert_eq!(twice.replacements, 0);
        assert_eq!(twice.text, once.text);
    }

    #[test]
    fn absolutize_result_changed_helper() {
        let unchanged = AbsolutizeResult {
            text: "x".to_string(),
            replacements: 0,
        };
        assert!(!unchanged.changed());
        let changed = AbsolutizeResult {
            text: "x".to_string(),
            replacements: 1,
        };
        assert!(changed.changed());
    }
}
