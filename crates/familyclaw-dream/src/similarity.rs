//! Tekstisamankaltaisuus duplikaattien tunnistukseen.
//!
//! `merge_duplicates`-vaihe (design §2.3, Anthropic Dreaming) tarvitsee tavan
//! tunnistaa *lähes-identtiset* muistot ilman ulkoista upotusmallia. Tämä
//! moduuli antaa riippuvuusvapaan, deterministisen sananjoukko-pohjaisen
//! samankaltaisuuden (Jaccard) — KERROS A toimii ilman vektorimallia.
//!
//! Vektoripohjainen semanttinen samankaltaisuus (cosine / HNSW) tulee
//! myöhemmin samalla rajapinnalla feature-flagin taakse, kuten
//! `familyclaw-memory`-haussa.

use std::collections::BTreeSet;

/// Pienin sananpituus joka huomioidaan (lyhyemmät täytesanat ohitetaan).
const MIN_TOKEN_LEN: usize = 2;

/// Pilkkoo tekstin normalisoiduksi sanajoukoksi.
///
/// - Pieniksi kirjaimiksi (case-insensitive vertailu).
/// - Erottimina kaikki ei-aakkosnumeeriset merkit.
/// - Alle [`MIN_TOKEN_LEN`]-mittaiset sanat karsitaan.
///
/// `BTreeSet` antaa deterministisen järjestyksen ja poistaa toistot, jotta
/// Jaccard on vakaa ajojen välillä.
fn token_set(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_TOKEN_LEN)
        .map(str::to_lowercase)
        .collect()
}

/// Jaccard-samankaltaisuus kahden tekstin sananjoukoille, `0.0..=1.0`.
///
/// `J(A, B) = |A ∩ B| / |A ∪ B|`. Kaksi tyhjää (tai pelkkiä täytesanoja
/// sisältävää) tekstiä katsotaan identtisiksi (`1.0`); jos vain toinen on
/// tyhjä, tulos on `0.0`.
///
/// Vertailu on symmetrinen ja deterministinen.
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

/// Ovatko kaksi tekstiä lähes-identtisiä annetulla kynnyksellä.
///
/// Kynnys puristetaan välille `0.0..=1.0`. Identtinen teksti on aina yli
/// minkä tahansa kynnyksen (paitsi jos kynnys on tarkalleen yli 1.0, mikä
/// ei ole mahdollista puristuksen jälkeen).
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
    // Tarkka f32-vertailu sallittu — vakioidut Jaccard-arvot.
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
        assert!((s - 0.5).abs() < 1e-6, "odotettiin 0.5, saatiin {s}");
    }

    #[test]
    fn symmetric() {
        let a = "agent_alpha built the bridge today";
        let b = "the bridge was built by agent_alpha";
        assert!((jaccard(a, b) - jaccard(b, a)).abs() < 1e-6);
    }

    #[test]
    fn both_empty_are_identical() {
        assert_eq!(jaccard("", ""), 1.0);
        // Pelkät täytesanat (1-merkkiset) → tyhjät joukot → identtisiä.
        assert_eq!(jaccard("a", "x"), 1.0);
    }

    #[test]
    fn one_empty_is_zero() {
        assert_eq!(jaccard("", "something here"), 0.0);
        assert_eq!(jaccard("something here", ""), 0.0);
    }

    #[test]
    fn short_tokens_are_filtered() {
        // "a" karsiutuu (1 merkki), joten näiden joukot ovat samat.
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
        // Kelvoton kynnys → 1.0 → vain identtinen kelpaa.
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
