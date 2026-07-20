//! Splitting Discord messages according to Discord's character limit and
//! its client-side "Show more" line-height fold.
//!
//! [`split_message`] splits text into chunks whose Unicode character count
//! (`chars().count()`) does not exceed `max_len`, AND whose newline (`\n`)
//! count does not exceed `max_newlines`. The two triggers are independent:
//! either one can force a split. A message well under the character limit
//! but with many lines (bullet lists, headers) still gets split, matching
//! the fact that Discord's client collapses tall messages behind "Show
//! more" based on rendered height, not the 2000-character API limit. The
//! split point prefers a newline, then a word boundary, and as a last
//! resort a hard character cut.

/// Splits a message into chunks of at most `max_len` characters
/// (`chars().count()`) and at most `max_newlines` newline (`\n`) characters
/// each.
///
/// An empty or whitespace-only input returns an empty vector. `max_len == 0`
/// is treated as `1`; `max_newlines` is not adjusted (`0` means a chunk may
/// not contain any newline at all).
///
/// Split-point priority within the first candidate window (bounded by
/// whichever of `max_len`/`max_newlines` is hit first):
/// 1. the last newline (`\n`),
/// 2. the last space (` `),
/// 3. a hard cut at the window boundary.
///
/// No chunk is empty, exceeds `max_len` characters, or exceeds `max_newlines`
/// newlines.
///
/// # Examples
///
/// ```
/// use familyclaw_channels::discord::split::split_message;
///
/// assert_eq!(split_message("moi", 2000, 15), vec!["moi".to_string()]);
/// assert_eq!(split_message("   ", 2000, 15), Vec::<String>::new());
/// ```
pub fn split_message(body: &str, max_len: usize, max_newlines: usize) -> Vec<String> {
    let max_len = max_len.max(1);
    if body.trim().is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut rest = body;

    while !rest.is_empty() {
        let within_char_limit = rest.chars().count() <= max_len;
        let within_line_limit = rest.matches('\n').count() <= max_newlines;
        if within_char_limit && within_line_limit {
            chunks.push(rest.to_string());
            break;
        }

        let split_at = find_split_byte(rest, max_len, max_newlines);
        let split_at = split_at.max(min_non_empty_split(rest));
        let (chunk, remaining) = rest.split_at(split_at);
        debug_assert!(
            !chunk.is_empty(),
            "split_message must not emit empty chunks"
        );
        chunks.push(chunk.to_string());
        rest = remaining;
    }

    chunks
}

/// Returns a length of at least one character for the first chunk.
fn min_non_empty_split(s: &str) -> usize {
    s.char_indices()
        .nth(1)
        .map_or(s.len(), |(byte_idx, _)| byte_idx)
}

/// Finds the byte offset at which `s` is cut for the first chunk of at most
/// `max_len` characters and at most `max_newlines` newlines (exclusive end
/// of chunk).
fn find_split_byte(s: &str, max_len: usize, max_newlines: usize) -> usize {
    debug_assert!(s.chars().count() > max_len || s.matches('\n').count() > max_newlines);

    let mut last_newline: Option<usize> = None;
    let mut last_space: Option<usize> = None;
    let mut count = 0usize;
    let mut newline_count = 0usize;
    let mut hard_end = s.len();

    for (byte_idx, ch) in s.char_indices() {
        if count >= max_len || (ch == '\n' && newline_count >= max_newlines) {
            hard_end = byte_idx;
            break;
        }

        let next = byte_idx + ch.len_utf8();
        match ch {
            '\n' => {
                newline_count += 1;
                last_newline = Some(next);
            }
            ' ' => last_space = Some(next),
            _ => {}
        }
        count += 1;
        if count == max_len {
            hard_end = next;
            break;
        }
    }

    if let Some(nl) = last_newline.filter(|&end| end > 0 && end <= hard_end) {
        return nl;
    }
    if let Some(sp) = last_space.filter(|&end| end > 0 && end <= hard_end) {
        return sp;
    }
    hard_end
}

#[cfg(test)]
mod tests {
    use super::split_message;

    /// A newline budget large enough to be a no-op for tests that are only
    /// exercising the character-count trigger.
    const NL_UNLIMITED: usize = usize::MAX;

    fn roundtrip(original: &str, max_len: usize, max_newlines: usize) {
        let chunks = split_message(original, max_len, max_newlines);
        let joined: String = chunks.iter().cloned().collect();
        assert_eq!(joined, original, "roundtrip failed for max_len={max_len}");
    }

    #[test]
    fn short_message_single_chunk() {
        assert_eq!(
            split_message("moi", 2000, NL_UNLIMITED),
            vec!["moi".to_string()]
        );
    }

    #[test]
    fn exactly_2000_chars_single_chunk() {
        let body = "a".repeat(2000);
        assert_eq!(split_message(&body, 2000, NL_UNLIMITED), vec![body]);
    }

    #[test]
    fn two_thousand_one_chars_two_chunks() {
        let body = "a".repeat(2001);
        let chunks = split_message(&body, 2000, NL_UNLIMITED);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), 2000);
        assert_eq!(chunks[1].chars().count(), 1);
        roundtrip(&body, 2000, NL_UNLIMITED);
    }

    #[test]
    fn multiline_prefers_newline_boundary() {
        let body = format!("{}\n{}", "line".repeat(100), "tail");
        let chunks = split_message(&body, 200, NL_UNLIMITED);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 200);
            assert!(!chunk.is_empty());
        }
        roundtrip(&body, 200, NL_UNLIMITED);
    }

    #[test]
    fn five_thousand_char_line_hard_split() {
        let body = "x".repeat(5000);
        let chunks = split_message(&body, 2000, NL_UNLIMITED);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), 2000);
        assert_eq!(chunks[1].chars().count(), 2000);
        assert_eq!(chunks[2].chars().count(), 1000);
        roundtrip(&body, 2000, NL_UNLIMITED);
    }

    #[test]
    fn unicode_emoji_respects_char_count() {
        let body = "a🎉b🎊c";
        let chunks = split_message(body, 2, NL_UNLIMITED);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "a🎉");
        assert_eq!(chunks[1], "b🎊");
        assert_eq!(chunks[2], "c");
        roundtrip(body, 2, NL_UNLIMITED);
    }

    #[test]
    fn empty_and_whitespace_return_empty_vec() {
        assert!(split_message("", 2000, NL_UNLIMITED).is_empty());
        assert!(split_message("   ", 2000, NL_UNLIMITED).is_empty());
        assert!(split_message("\n\t  \n", 2000, NL_UNLIMITED).is_empty());
    }

    #[test]
    fn max_len_zero_treated_as_one() {
        assert_eq!(
            split_message("ab", 0, NL_UNLIMITED),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(split_message("a", 0, NL_UNLIMITED), vec!["a".to_string()]);
    }

    #[test]
    fn max_len_one_splits_every_char() {
        assert_eq!(
            split_message("abc", 1, NL_UNLIMITED),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn roundtrip_preserves_content() {
        roundtrip("hello world from discord", 8, NL_UNLIMITED);
        roundtrip("rivi1\nrivi2\nrivi3", 6, NL_UNLIMITED);
        roundtrip("emoji 🦀 rust", 5, NL_UNLIMITED);
    }

    #[test]
    fn word_boundary_split() {
        let chunks = split_message("hello world", 8, NL_UNLIMITED);
        assert_eq!(chunks, vec!["hello ".to_string(), "world".to_string()]);
    }

    // --- max_newlines: the "Show more" fold trigger ---

    #[test]
    fn many_short_lines_split_on_newline_budget_before_char_limit() {
        // 20 short lines ("line0".."line19" joined by \n) is well under the
        // 2000-char limit, but exceeds a newline budget of 5 — this is the
        // "Show more" collapse case: visually tall, not character-heavy.
        let body: String = (0..20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.chars().count() < 2000);

        let chunks = split_message(&body, 2000, 5);
        assert!(
            chunks.len() > 1,
            "must split on newline count even though char limit is not reached"
        );
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 2000);
            assert!(
                chunk.matches('\n').count() <= 5,
                "chunk exceeds newline budget: {chunk:?}"
            );
            assert!(!chunk.is_empty());
        }
        roundtrip(&body, 2000, 5);
    }

    #[test]
    fn newlines_within_budget_stay_a_single_chunk() {
        let body = "a\nb\nc\nd\ne";
        // 4 newlines, budget of 4 → must NOT split.
        assert_eq!(split_message(body, 2000, 4), vec![body.to_string()]);
    }

    #[test]
    fn max_newlines_zero_never_panics_and_preserves_content() {
        // Degenerate edge case (never used in production —
        // DISCORD_CHUNK_MAX_NEWLINES is 15 — but must not panic and must
        // still preserve every character via roundtrip).
        let body = "a\nb\nc";
        let chunks = split_message(body, 2000, 0);
        assert!(chunks.len() > 1, "must split when the newline budget is 0");
        for chunk in &chunks {
            assert!(
                !chunk.is_empty(),
                "split_message must not emit empty chunks"
            );
        }
        roundtrip(body, 2000, 0);
    }

    #[test]
    fn char_limit_still_applies_when_newline_budget_is_generous() {
        // Regression guard: raising max_newlines must not disable the
        // existing char-count trigger.
        let body = "a".repeat(2001);
        let chunks = split_message(&body, 2000, NL_UNLIMITED);
        assert_eq!(chunks.len(), 2);
        roundtrip(&body, 2000, NL_UNLIMITED);
    }
}
