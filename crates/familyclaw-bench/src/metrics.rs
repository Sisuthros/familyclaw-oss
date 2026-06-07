//! Tyypitetyt mittarit jatkuvuustyökuorman pisteytykseen.
//!
//! Nämä apurit muuntavat raa'at havainnot vertailukelpoisiksi luvuiksi joita
//! skenaariot kirjaavat [`ScenarioResult`](crate::scenario::ScenarioResult):iin
//! ja jotka aggregoidaan scorecardiin. Kaikki funktiot ovat puhtaita ja
//! deterministisiä — sama syöte → sama luku (design §2.2).
//!
//! Mittarit (design §3):
//! - [`resume_correctness`] — S1: jatkuiko työ täsmälleen oikeasta askelesta.
//! - [`recall_at_k`] — S2: kuinka moni odotetuista muistoista löytyi top-k:sta.
//! - [`dedup_precision`] — S3: kuinka tarkasti unijakso poisti duplikaatit.
//! - [`protected_core_intact`] — S3: säilyivätkö identiteetti-ankkurit (1.0/0.0).

use crate::error::Result;

/// Resume-oikeellisuus: `1.0` jos jokainen odotettu askel jatkui oikein ilman
/// uudelleen ajettuja sivuvaikutuksia, muuten suhteellinen osuus.
///
/// `side_effects_reexecuted > 0` pakottaa tuloksen nollaan — sivuvaikutus saa
/// tapahtua täsmälleen kerran (design §3 S1).
///
/// # Errors
/// Palauttaa [`BenchError::Metric`](crate::BenchError::Metric) jos
/// `expected_steps == 0` (jaettaisiin nollalla).
#[must_use = "metric result must be recorded"]
pub fn resume_correctness(
    expected_steps: usize,
    correctly_resumed: usize,
    side_effects_reexecuted: usize,
) -> Result<f64> {
    if expected_steps == 0 {
        return Err(crate::BenchError::metric(
            "resume_correctness: expected_steps must be > 0",
        ));
    }
    if side_effects_reexecuted > 0 {
        return Ok(0.0);
    }
    let ratio = f64::from(u32::try_from(correctly_resumed.min(expected_steps)).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(expected_steps).unwrap_or(u32::MAX));
    Ok(ratio)
}

/// `recall@k`: osuus odotetuista muistoista jotka löytyivät palautettujen
/// top-k joukosta.
///
/// # Errors
/// Palauttaa [`BenchError::Metric`](crate::BenchError::Metric) jos
/// `expected_total == 0`.
#[must_use = "metric result must be recorded"]
pub fn recall_at_k(expected_total: usize, found_in_top_k: usize) -> Result<f64> {
    if expected_total == 0 {
        return Err(crate::BenchError::metric(
            "recall_at_k: expected_total must be > 0",
        ));
    }
    Ok(
        f64::from(u32::try_from(found_in_top_k.min(expected_total)).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(expected_total).unwrap_or(u32::MAX)),
    )
}

/// Dedup-tarkkuus: oikein poistetut duplikaatit suhteessa kaikkiin poistoihin.
///
/// `precision = true_merges / (true_merges + false_merges)`. Jos yhtään poistoa
/// ei tehty, tulos on `1.0` (ei vääriä positiivisia).
///
/// # Errors
/// Tämä funktio ei voi epäonnistua, mutta palauttaa [`Result`] yhtenäisyyden
/// vuoksi muiden mittarien kanssa.
#[must_use = "metric result must be recorded"]
pub fn dedup_precision(true_merges: usize, false_merges: usize) -> Result<f64> {
    let total = true_merges + false_merges;
    if total == 0 {
        return Ok(1.0);
    }
    Ok(f64::from(u32::try_from(true_merges).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total).unwrap_or(u32::MAX)))
}

/// Suojatun ytimen eheys: `1.0` jos yksikään identiteetti-ankkuri ei kadonnut
/// konsolidaatiossa, muuten `0.0` (design §3 S3, hyväksyntäkriteeri 4).
#[must_use]
pub fn protected_core_intact(intact: bool) -> f64 {
    if intact {
        1.0
    } else {
        0.0
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Vakiot 0.0/1.0 ovat tarkkoja float-arvoja testeissä.
mod tests {
    use super::*;

    #[test]
    fn resume_correctness_full_and_partial() {
        assert!((resume_correctness(4, 4, 0).expect("ok") - 1.0).abs() < 1e-9);
        assert!((resume_correctness(4, 2, 0).expect("ok") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn resume_correctness_side_effect_forces_zero() {
        assert_eq!(resume_correctness(4, 4, 1).expect("ok"), 0.0);
    }

    #[test]
    fn resume_correctness_rejects_zero_steps() {
        assert!(resume_correctness(0, 0, 0).is_err());
    }

    #[test]
    fn recall_at_k_basic() {
        assert!((recall_at_k(10, 9).expect("ok") - 0.9).abs() < 1e-9);
        assert!(recall_at_k(0, 0).is_err());
    }

    #[test]
    fn dedup_precision_no_merges_is_one() {
        assert_eq!(dedup_precision(0, 0).expect("ok"), 1.0);
        assert!((dedup_precision(3, 1).expect("ok") - 0.75).abs() < 1e-9);
    }

    #[test]
    fn protected_core_maps_bool() {
        assert_eq!(protected_core_intact(true), 1.0);
        assert_eq!(protected_core_intact(false), 0.0);
    }
}
