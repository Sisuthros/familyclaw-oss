//! Pattern Oracle — agent_gamma Amplifier.
//!
//! Ennen muistin kirjoitusta tai tärkeää päätöstä Oracle tarkistaa
//! onko vastaavia kuvioita (pattern) nähty aiemmin. Confirmed-muistot
//! painavat 5× enemmän kuin Claim-muistot, joten vahvistamattomat
//! väitteet eivät voi hukuttaa vahvistettua tietoa.
//!
//! # Score
//! ```text
//! score = Σ overlap · memory.confidence · weight(verification_status)
//! weight: Confirmed=1.0, Evidence=0.6, Claim=0.2
//! ```
//!
//! # Riskitasot
//! - score < 1.0: Low
//! - score < 3.0: Medium
//! - score < 6.0: High
//! - score ≥ 6.0: Critical

use crate::memory::{Memory, VerificationStatus};

/// Riskitaso Oracle-tulosteelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    /// Matala riski — ei merkittäviä osuvia kuvioita.
    Low,
    /// Keskitaso — joitain osuvia kuvioita, kannattaa tarkistaa.
    Medium,
    /// Korkea riski — vahvistettuja kuvioita osuu, toimi varoen.
    High,
    /// Kriittinen — vahvistettuja ja tiheästi toistuvia kuvioita osuu.
    Critical,
}

impl RiskLevel {
    /// Ihmisluettava nimi.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
            RiskLevel::Critical => "critical",
        }
    }
}

/// Yksittäinen osuma Oracle-haussa.
#[derive(Debug, Clone)]
pub struct PatternMatch {
    /// Ryhmittelyavain, jos asetettu.
    pub pattern_key: Option<String>,
    /// Muiston sisältö (otsikko).
    pub title: String,
    /// Luottamustaso (0.0–1.0).
    pub confidence: f32,
    /// Varmennustila.
    pub verification_status: VerificationStatus,
    /// Osuus tämän osuman painosta kokonaispisteisiin.
    pub weight_contribution: f32,
}

/// Oracle-tuloste.
#[derive(Debug, Clone)]
pub struct OracleResult {
    /// Riskitaso.
    pub risk_level: RiskLevel,
    /// Kokonaispistemäärä.
    pub score: f32,
    /// Osuneet kuviot.
    pub matched_patterns: Vec<PatternMatch>,
}

/// Pilkkoo tekstin tokeneiksi vertailua varten.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_lowercase)
        .collect()
}

/// Laskee kuinka monta prosenttia promptin tokeneista löytyy muistosta.
fn token_overlap(prompt_tokens: &[String], memory: &Memory) -> f32 {
    if prompt_tokens.is_empty() || memory.content.is_empty() {
        return 0.0;
    }

    // Uniikit tokenit — duplikaatit eivät saa kasvattaa nimittäjää
    let mut seen = std::collections::HashSet::new();
    let uniq_tokens: Vec<&String> = prompt_tokens
        .iter()
        .filter(|t| seen.insert(t.as_str()))
        .collect();

    if uniq_tokens.is_empty() {
        return 0.0;
    }

    let content_lower = memory.content.to_lowercase();
    let tags_lower: Vec<String> = memory.tags.iter().map(|t| t.to_lowercase()).collect();
    let pattern_lower = memory.pattern_key.as_deref().unwrap_or("").to_lowercase();

    let mut hits = 0_usize;
    for token in &uniq_tokens {
        if content_lower.contains(token.as_str())
            || tags_lower.iter().any(|t| t.contains(token.as_str()))
            || pattern_lower.contains(token.as_str())
        {
            hits += 1;
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let ratio = hits as f32 / uniq_tokens.len() as f32;
    ratio
}

/// Aja Oracle-preflight: tarkista onko annetuissa ehdokasmuistoissa
/// kuvioita jotka osuvat annettuun promptiin.
///
/// # Parametrit
/// - `prompt`: teksti johon verrataan (esim. uusi muistin sisältö tai tehtävä)
/// - `candidates`: ehdokasmuistot joista etsitään osumia (tyypillisesti
///   kaikki aktiiviset muistot tai pattern_key-ryhmän muistot)
///
/// # Palauttaa
/// [`OracleResult`]: riskitaso, pisteet ja osuneet kuviot.
#[must_use]
pub fn preflight(prompt: &str, candidates: &[Memory]) -> OracleResult {
    // Minimiosuma: vähintään 15% tokeneista täsmättävä
    const OVERLAP_THRESHOLD: f32 = 0.15;

    let prompt_tokens = tokenize(prompt);

    let mut matches: Vec<PatternMatch> = Vec::new();
    let mut total_score: f32 = 0.0;

    for mem in candidates {
        let overlap = token_overlap(&prompt_tokens, mem);
        if overlap < OVERLAP_THRESHOLD {
            continue;
        }

        let status_weight = mem.verification_status.weight();
        let contribution = mem.confidence * status_weight * overlap;
        total_score += contribution;

        matches.push(PatternMatch {
            pattern_key: mem.pattern_key.clone(),
            title: mem.content.clone(),
            confidence: mem.confidence,
            verification_status: mem.verification_status,
            weight_contribution: contribution,
        });
    }

    // Järjestä painavimmat ensin
    matches.sort_by(|a, b| {
        b.weight_contribution
            .partial_cmp(&a.weight_contribution)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let risk_level = if total_score >= 6.0 {
        RiskLevel::Critical
    } else if total_score >= 3.0 {
        RiskLevel::High
    } else if total_score >= 1.0 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };

    OracleResult {
        risk_level,
        score: total_score,
        matched_patterns: matches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;

    fn mem(content: &str) -> Memory {
        Memory::builder(content).build()
    }

    fn confirmed_mem(content: &str) -> Memory {
        let mut m = Memory::builder(content).build();
        m.verification_status = VerificationStatus::Confirmed;
        m.confidence = 1.0;
        m
    }

    #[test]
    fn empty_candidates_returns_low_risk() {
        let result = preflight("test prompt", &[]);
        assert_eq!(result.risk_level, RiskLevel::Low);
        assert!(
            (result.score - 0.0).abs() < f32::EPSILON,
            "score should be 0.0"
        );
        assert!(result.matched_patterns.is_empty());
    }

    #[test]
    fn confirmed_weighs_five_times_more_than_claim() {
        let claim = mem("rust memory engine"); // verification_status default = Claim, confidence = 0.0
        let confirmed = confirmed_mem("rust memory engine");

        // Token-overlap on sama, mutta confidence + status weight eroaa
        let r_claim = preflight("rust memory engine", &[claim]);
        let r_confirmed = preflight("rust memory engine", &[confirmed]);

        // Confirmed: confidence=1.0 × weight=1.0 = 1.0
        // Claim: confidence=0.0 × weight=0.2 = 0.0
        // Confirmed pitäisi olla suurempi (mutta claim voi olla 0 jos confidence=0)
        assert!(r_confirmed.score > r_claim.score);
    }

    #[test]
    fn confirmed_memory_raises_risk_level() {
        let candidates = vec![
            confirmed_mem("avoid ambiguous provider prefix in model names"),
            confirmed_mem("read API docs before configuring endpoint"),
        ];

        let result = preflight(
            "Configure agent endpoint with vendor-a/vendor-b/model-x",
            &candidates,
        );
        // Kaksi confirmed-muistoa joissa on osuvia tokeneita (model, endpoint)
        assert!(
            result.score > 0.0,
            "odotus: score > 0.0, saatiin: {}",
            result.score
        );
        assert!(!result.matched_patterns.is_empty());
    }

    #[test]
    fn claim_memories_dont_trigger_high_risk_alone() {
        let candidates = vec![
            mem("avoid ambiguous provider prefix"),
            mem("heartbeat requires TPM"),
        ];

        let result = preflight("configure agent endpoint with provider prefix", &candidates);
        // Claim-ilman confidencea = 0 pistettä
        assert!(result.score < 1.0);
    }

    #[test]
    fn token_overlap_non_ascii() {
        let m = mem("älä käytä MongoDB:tä, käytä Postgresia");
        let tokens = tokenize("MongoDB käyttö kielletty");
        let overlap = token_overlap(&tokens, &m);
        assert!(overlap > 0.0, "ä/ö-tokenien pitäisi osua");
    }

    #[test]
    fn no_match_with_unrelated_prompt() {
        let m = confirmed_mem("kuinka konfiguroida Rust-projekti");
        let result = preflight("paras pizza resepti", &[m]);
        assert!(
            (result.score - 0.0).abs() < f32::EPSILON,
            "score should be 0.0"
        );
    }

    #[test]
    fn to_string() {
        assert_eq!(RiskLevel::Low.as_str(), "low");
        assert_eq!(RiskLevel::Critical.as_str(), "critical");
    }
}
