//! Splitting Discord messages according to Discord's character limit.
//!
//! [`split_message`] splits text into chunks whose Unicode character count
//! (`chars().count()`) does not exceed the given limit. The split point
//! prefers a newline, then a word boundary, and as a last resort a hard
//! character cut.

/// Splits a message into chunks of at most `max_len` characters
/// (`chars().count()`).
///
/// An empty or whitespace-only input returns an empty vector. `max_len == 0`
/// is treated as `1`.
///
/// Split-point priority within the first `max_len`-character window:
/// 1. the last newline (`\n`),
/// 2. the last space (` `),
/// 3. a hard cut at exactly `max_len` characters.
///
/// No chunk is empty or exceeds `max_len` characters.
///
/// # Examples
///
/// ```
/// use familyclaw_channels::discord::split::split_message;
///
/// assert_eq!(split_message("moi", 2000), vec!["moi".to_string()]);
/// assert_eq!(split_message("   ", 2000), Vec::<String>::new());
/// ```
pub fn split_message(body: &str, max_len: usize) -> Vec<String> {
    let max_len = max_len.max(1);
    if body.trim().is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut rest = body;

    while !rest.is_empty() {
        if rest.chars().count() <= max_len {
            chunks.push(rest.to_string());
            break;
        }

        let split_at = find_split_byte(rest, max_len);
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
/// `max_len` characters (exclusive end of chunk).
fn find_split_byte(s: &str, max_len: usize) -> usize {
    debug_assert!(s.chars().count() > max_len);

    let mut last_newline: Option<usize> = None;
    let mut last_space: Option<usize> = None;
    let mut count = 0usize;
    let mut hard_end = s.len();

    for (byte_idx, ch) in s.char_indices() {
        if count >= max_len {
            hard_end = byte_idx;
            break;
        }

        let next = byte_idx + ch.len_utf8();
        match ch {
            '\n' => last_newline = Some(next),
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

    fn roundtrip(original: &str, max_len: usize) {
        let chunks = split_message(original, max_len);
        let joined: String = chunks.iter().cloned().collect();
        assert_eq!(joined, original, "roundtrip failed for max_len={max_len}");
    }

    #[test]
    fn short_message_single_chunk() {
        assert_eq!(split_message("moi", 2000), vec!["moi".to_string()]);
    }

    #[test]
    fn exactly_2000_chars_single_chunk() {
        let body = "a".repeat(2000);
        assert_eq!(split_message(&body, 2000), vec![body]);
    }

    #[test]
    fn two_thousand_one_chars_two_chunks() {
        let body = "a".repeat(2001);
        let chunks = split_message(&body, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), 2000);
        assert_eq!(chunks[1].chars().count(), 1);
        roundtrip(&body, 2000);
    }

    #[test]
    fn multiline_prefers_newline_boundary() {
        let body = format!("{}\n{}", "line".repeat(100), "tail");
        let chunks = split_message(&body, 200);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 200);
            assert!(!chunk.is_empty());
        }
        roundtrip(&body, 200);
    }

    #[test]
    fn five_thousand_char_line_hard_split() {
        let body = "x".repeat(5000);
        let chunks = split_message(&body, 2000);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), 2000);
        assert_eq!(chunks[1].chars().count(), 2000);
        assert_eq!(chunks[2].chars().count(), 1000);
        roundtrip(&body, 2000);
    }

    #[test]
    fn unicode_emoji_respects_char_count() {
        let body = "a🎉b🎊c";
        let chunks = split_message(body, 2);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "a🎉");
        assert_eq!(chunks[1], "b🎊");
        assert_eq!(chunks[2], "c");
        roundtrip(body, 2);
    }

    #[test]
    fn empty_and_whitespace_return_empty_vec() {
        assert!(split_message("", 2000).is_empty());
        assert!(split_message("   ", 2000).is_empty());
        assert!(split_message("\n\t  \n", 2000).is_empty());
    }

    #[test]
    fn max_len_zero_treated_as_one() {
        assert_eq!(
            split_message("ab", 0),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(split_message("a", 0), vec!["a".to_string()]);
    }

    #[test]
    fn max_len_one_splits_every_char() {
        assert_eq!(
            split_message("abc", 1),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn roundtrip_preserves_content() {
        roundtrip("hello world from discord", 8);
        roundtrip("rivi1\nrivi2\nrivi3", 6);
        roundtrip("emoji 🦀 rust", 5);
    }

    #[test]
    fn word_boundary_split() {
        let chunks = split_message("hello world", 8);
        assert_eq!(chunks, vec!["hello ".to_string(), "world".to_string()]);
    }
}
