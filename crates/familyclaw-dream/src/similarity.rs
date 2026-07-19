//! Text similarity for duplicate detection.
//!
//! The `merge_duplicates` phase (design §2.3, Anthropic Dreaming) needs a
//! way to identify *near-identical* memories without an external embedding
//! model. This module provides a dependency-free, deterministic word-set-
//! based similarity measure (Jaccard) — Layer A works without a vector
//! model.
//!
//! Vector-based semantic similarity (cosine / HNSW) will come later behind
//! a feature flag on the same interface, as in `familyclaw-memory` search.

use std::collections::BTreeSet;

/// Minimum word length considered (shorter filler words are skipped).
const MIN_TOKEN_LEN: usize = 2;

/// Splits text into a normalized word set.
///
/// - Lowercased (case-insensitive comparison).
/// - Split on all non-alphanumeric characters.
/// - Words shorter than [`MIN_TOKEN_LEN`] are dropped.
///
/// `BTreeSet` gives a deterministic order and removes duplicates, so
/// Jaccard is stable across runs.
fn token_set(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_TOKEN_LEN)
        .map(str::to_lowercase)
        .collect()
}

/// Jaccard similarity between the word sets of two texts, `0.0..=1.0`.
///
/// `J(A, B) = |A ∩ B| / |A ∪ B|`. Two empty (or filler-word-only) texts are
/// considered identical (`1.0`); if only one is empty, the result is `0.0`.
///
/// The comparison is symmetric and deterministic.
#[must_use]
pub fn jaccard(a: &str, b: &str) -> f32 {
    let sa = token_set(a);
    let sb = token_set(b);
    match (sa.is_empty(), sb.is_empty()) {
        (true, true) => return 1.0,
        (true, false) | (false, true) => return 0.0,
        (false, false) => {}
    }
    let intersection = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = intersection as f32 / union as f32;
    ratio
}

/// Whether two texts are near-identical at the given threshold.
///
/// The threshold is clamped to `0.0..=1.0`. Identical text is always above
/// any threshold (unless the threshold is exactly above 1.0, which isn't
/// possible after clamping).
#[must_use]
pub fn is_near_duplicate(a: &str, b: &str, threshold: f32) -> bool {
    let t = if threshold.is_finite() {
        threshold.clamp(0.0, 1.0)
    } else {
        1.0
    };
    jaccard(a, b) >= t
}

#[cfg(test)]
mod tests {
    // Exact f32 comparison allowed — fixed Jaccard values.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn identical_text_is_fully_similar() {
        assert_eq!(jaccard("hello world", "hello world"), 1.0);
    }

    #[test]
    fn case_and_punctuation_are_normalized() {
        assert_eq!(jaccard("Hello, World!", "hello world"), 1.0);
    }

    #[test]
    fn disjoint_text_is_zero() {
        assert_eq!(jaccard("alpha beta", "gamma delta"), 0.0);
    }

    #[test]
    fn partial_overlap_is_between() {
        // A = {the, cat, sat}, B = {the, cat, ran}
        // ∩ = {the, cat} = 2, ∪ = {the, cat, sat, ran} = 4 → 0.5
        let s = jaccard("the cat sat", "the cat ran");
        assert!((s - 0.5).abs() < 1e-6, "expected 0.5, got {s}");
    }

    #[test]
    fn symmetric() {
        let a = "agent_a built the bridge today";
        let b = "the bridge was built by agent_a";
        assert!((jaccard(a, b) - jaccard(b, a)).abs() < 1e-6);
    }

    #[test]
    fn both_empty_are_identical() {
        assert_eq!(jaccard("", ""), 1.0);
        // Only filler words (1-character) → empty sets → identical.
        assert_eq!(jaccard("a", "x"), 1.0);
    }

    #[test]
    fn one_empty_is_zero() {
        assert_eq!(jaccard("", "something here"), 0.0);
        assert_eq!(jaccard("something here", ""), 0.0);
    }

    #[test]
    fn short_tokens_are_filtered() {
        // "a" gets dropped (1 character), so these sets are equal.
        assert_eq!(jaccard("a big house", "big house"), 1.0);
    }

    #[test]
    fn near_duplicate_respects_threshold() {
        // 0.5-overlap.
        assert!(is_near_duplicate("the cat sat", "the cat ran", 0.5));
        assert!(is_near_duplicate("the cat sat", "the cat ran", 0.4));
        assert!(!is_near_duplicate("the cat sat", "the cat ran", 0.6));
    }

    #[test]
    fn near_duplicate_clamps_invalid_threshold() {
        // Invalid threshold → 1.0 → only identical text qualifies.
        assert!(is_near_duplicate("same words", "same words", f32::NAN));
        assert!(!is_near_duplicate("same words", "other text", f32::NAN));
    }

    #[test]
    fn jaccard_stays_in_unit_range() {
        let pairs = [
            ("rust async runtime", "rust memory model"),
            ("", "x"),
            ("hello", "hello hello hello"),
            ("one two three four", "two three"),
        ];
        for (a, b) in pairs {
            let s = jaccard(a, b);
            assert!((0.0..=1.0).contains(&s), "{a:?} vs {b:?} → {s}");
        }
    }
}
