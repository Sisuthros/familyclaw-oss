//! Muistin vaimeneminen: [`DecayPolicy`] ja Ebbinghaus-pohjainen retentio.
//!
//! Muistot eivät katoa tasaisesti. Eternal Thread mallintaa unohtamista
//! Ebbinghausin unohtamiskäyrällä — eksponentiaalinen retentio jonka
//! nopeutta säätää muistille valittu [`DecayPolicy`]. Identiteetti-ankkurit
//! (`ProtectedCore`, λ = 0.0) eivät koskaan vaimene; arkipäiväiset
//! havainnot (`Fast`) haihtuvat nopeasti.
//!
//! ## Ebbinghausin malli
//! Retentio ajanhetkellä `t` (kulunut aika sekunteina):
//!
//! ```text
//! R(t) = e^(-λ · t / S)
//! ```
//!
//! - `λ` (lambda) = politiikan vaimennusvakio (`decay_lambda`),
//! - `S` = muistin **vahvuus** (stability), joka kasvaa tärkeyden ja
//!   vahvistuksen myötä (suuremmat muistot säilyvät pidempään),
//! - `R(t)` ∈ `0.0..=1.0` — jäljellä oleva retentio (1.0 = täysin tuore).
//!
//! Politiikan λ-arvot on poimittu `FamilyClaw` v2 -designista (§2.3, §5):
//! `ProtectedCore = 0.0`, `Slow = 0.02`, `Normal = 0.18`, `Fast = 0.5`.

use serde::{Deserialize, Serialize};

/// Vakausparametrin (stability `S`) yksikköskaala sekunteina.
///
/// Vahvuus `S = 1.0` vastaa noin yhden vuorokauden aikaskaalaa: tällä
/// arvolla retentio noudattaa puhtaasti politiikan λ:aa päivätasolla.
/// Suurempi vahvuus venyttää muistia pidemmälle ajalle.
const STABILITY_TIME_SCALE_SECS: f32 = 86_400.0;

/// Pienin sallittu vahvuus, jottei nollalla jaeta retentiokaavassa.
const MIN_STABILITY: f32 = 0.05;

/// Kuinka nopeasti muisti vaimenee Ebbinghausin käyrällä.
///
/// Jokainen variantti kantaa kiinteän λ-vaimennusvakion (`decay_lambda`).
/// Pienempi λ = hitaampi unohtaminen. `ProtectedCore` (λ = 0.0) ei vaimene
/// koskaan — se on identiteetti-ankkuri (design §2: `ProtectedCore`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecayPolicy {
    /// Identiteetin ydin — ei vaimene koskaan (λ = 0.0).
    ///
    /// Käytetään muistoille jotka muodostavat olennon identiteetin
    /// (esim. nimi, perhe, kantava arvo). Nämä ankkurit pysyvät tuoreina
    /// määräämättömän ajan.
    ProtectedCore,
    /// Hidas vaimeneminen (λ = 0.02) — merkityksellinen, kestävä muisti.
    Slow,
    /// Tavanomainen vaimeneminen (λ = 0.18) — Ebbinghausin perusarvo.
    Normal,
    /// Nopea vaimeneminen (λ = 0.5) — ohimenevä, arkinen havainto.
    Fast,
}

impl DecayPolicy {
    /// Kaikki politiikat hitaimmasta nopeimpaan vaimenemiseen.
    pub const ALL: [DecayPolicy; 4] = [
        DecayPolicy::ProtectedCore,
        DecayPolicy::Slow,
        DecayPolicy::Normal,
        DecayPolicy::Fast,
    ];

    /// Politiikan Ebbinghaus-vaimennusvakio `λ`.
    ///
    /// `0.0` tarkoittaa "ei koskaan vaimene" ([`DecayPolicy::ProtectedCore`]).
    #[must_use]
    pub const fn decay_lambda(self) -> f32 {
        match self {
            DecayPolicy::ProtectedCore => 0.0,
            DecayPolicy::Slow => 0.02,
            DecayPolicy::Normal => 0.18,
            DecayPolicy::Fast => 0.5,
        }
    }

    /// Onko tämä suojattu identiteetti-ankkuri (ei koskaan vaimene).
    #[must_use]
    pub const fn is_protected(self) -> bool {
        matches!(self, DecayPolicy::ProtectedCore)
    }

    /// Vakaa, kone-luettava nimi (`snake_case`) — sama kuin serde-esitys.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DecayPolicy::ProtectedCore => "protected_core",
            DecayPolicy::Slow => "slow",
            DecayPolicy::Normal => "normal",
            DecayPolicy::Fast => "fast",
        }
    }

    /// Ebbinghaus-retentio kuluneen ajan `dt_secs` jälkeen annetulla
    /// muistin vahvuudella `stability`.
    ///
    /// Palauttaa arvon välillä `0.0..=1.0`: `1.0` = täysin tuore,
    /// lähestyy `0.0`:aa kun muisto unohtuu. `ProtectedCore` palauttaa aina
    /// `1.0`. Negatiivinen tai ei-äärellinen `dt_secs` käsitellään nollana
    /// (tuore); ei-positiivinen vahvuus puristetaan turvalliseen minimiin.
    ///
    /// Kaava: `R = e^(-λ · t / (S · TIME_SCALE))`.
    #[must_use]
    pub fn retention(self, dt_secs: f32, stability: f32) -> f32 {
        let lambda = self.decay_lambda();
        // Suojattu ydin tai nolla-λ ei vaimene koskaan.
        if lambda <= 0.0 {
            return 1.0;
        }
        // Kelvoton/negatiivinen aikadelta = muisto on yhä tuore.
        let dt = if dt_secs.is_finite() && dt_secs > 0.0 {
            dt_secs
        } else {
            0.0
        };
        // Vahvuus puristetaan turvalliseen minimiin, jottei jaeta nollalla.
        let s = if stability.is_finite() && stability > MIN_STABILITY {
            stability
        } else {
            MIN_STABILITY
        };
        let exponent = -lambda * dt / (s * STABILITY_TIME_SCALE_SECS);
        let r = exponent.exp();
        // Numeerinen varmistus rajoihin.
        r.clamp(0.0, 1.0)
    }
}

impl std::fmt::Display for DecayPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for DecayPolicy {
    /// Oletuspolitiikka on [`DecayPolicy::Normal`] — Ebbinghausin perusarvo.
    fn default() -> Self {
        DecayPolicy::Normal
    }
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn lambda_values_match_design() {
        assert_eq!(DecayPolicy::ProtectedCore.decay_lambda(), 0.0);
        assert_eq!(DecayPolicy::Slow.decay_lambda(), 0.02);
        assert_eq!(DecayPolicy::Normal.decay_lambda(), 0.18);
        assert_eq!(DecayPolicy::Fast.decay_lambda(), 0.5);
    }

    #[test]
    fn protected_core_never_decays() {
        let p = DecayPolicy::ProtectedCore;
        assert!(p.is_protected());
        // Mielivaltaisen pitkä aika, pieni vahvuus → retentio yhä täysi.
        assert_eq!(p.retention(0.0, 1.0), 1.0);
        assert_eq!(p.retention(1e9, 0.01), 1.0);
        assert_eq!(p.retention(f32::MAX, 0.0), 1.0);
    }

    #[test]
    fn non_protected_are_not_protected() {
        assert!(!DecayPolicy::Slow.is_protected());
        assert!(!DecayPolicy::Normal.is_protected());
        assert!(!DecayPolicy::Fast.is_protected());
    }

    #[test]
    fn fresh_memory_has_full_retention() {
        for p in DecayPolicy::ALL {
            assert!(
                (p.retention(0.0, 1.0) - 1.0).abs() < 1e-6,
                "{p} ei tuore nollalla"
            );
        }
    }

    #[test]
    fn retention_decreases_over_time() {
        let p = DecayPolicy::Normal;
        let r_day = p.retention(STABILITY_TIME_SCALE_SECS, 1.0);
        let r_week = p.retention(STABILITY_TIME_SCALE_SECS * 7.0, 1.0);
        assert!(r_day < 1.0);
        assert!(
            r_week < r_day,
            "viikon retentio {r_week} ei ole alle päivän {r_day}"
        );
        assert!(r_week > 0.0);
    }

    #[test]
    fn faster_policy_decays_faster() {
        let t = STABILITY_TIME_SCALE_SECS * 3.0;
        let slow = DecayPolicy::Slow.retention(t, 1.0);
        let normal = DecayPolicy::Normal.retention(t, 1.0);
        let fast = DecayPolicy::Fast.retention(t, 1.0);
        assert!(fast < normal, "fast {fast} ei alle normal {normal}");
        assert!(normal < slow, "normal {normal} ei alle slow {slow}");
    }

    #[test]
    fn higher_stability_retains_longer() {
        let p = DecayPolicy::Normal;
        let t = STABILITY_TIME_SCALE_SECS * 5.0;
        let weak = p.retention(t, 1.0);
        let strong = p.retention(t, 4.0);
        assert!(
            strong > weak,
            "vahva muisto {strong} ei säily pidempään kuin heikko {weak}"
        );
    }

    #[test]
    fn retention_known_value_at_one_unit() {
        // Normal λ=0.18, dt = 1 päivä, S = 1.0 → R = e^(-0.18) ≈ 0.8353.
        let r = DecayPolicy::Normal.retention(STABILITY_TIME_SCALE_SECS, 1.0);
        let expected = (-0.18_f32).exp();
        assert!(
            (r - expected).abs() < 1e-4,
            "retentio {r} ei vastaa odotettua {expected}"
        );
    }

    #[test]
    fn invalid_dt_is_treated_as_fresh() {
        let p = DecayPolicy::Fast;
        assert_eq!(p.retention(-100.0, 1.0), 1.0);
        assert_eq!(p.retention(f32::NAN, 1.0), 1.0);
        assert_eq!(p.retention(f32::INFINITY, 1.0), 1.0);
    }

    #[test]
    fn nonpositive_stability_does_not_divide_by_zero() {
        let p = DecayPolicy::Normal;
        let r = p.retention(STABILITY_TIME_SCALE_SECS, 0.0);
        assert!(r.is_finite());
        assert!((0.0..=1.0).contains(&r));
        let r_neg = p.retention(STABILITY_TIME_SCALE_SECS, -5.0);
        assert!(r_neg.is_finite());
    }

    #[test]
    fn retention_stays_in_unit_range() {
        for p in DecayPolicy::ALL {
            for &t in &[0.0, 1.0, 1e3, 1e6, 1e9] {
                for &s in &[0.1, 1.0, 10.0] {
                    let r = p.retention(t, s);
                    assert!(
                        (0.0..=1.0).contains(&r),
                        "{p} t={t} s={s} → r={r} ulkona rajoista"
                    );
                }
            }
        }
    }

    #[test]
    fn display_and_as_str_match_serde() {
        for p in DecayPolicy::ALL {
            assert_eq!(p.to_string(), p.as_str());
            let json = serde_json::to_string(&p).expect("serialize policy");
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            let back: DecayPolicy = serde_json::from_str(&json).expect("deserialize policy");
            assert_eq!(back, p);
        }
    }

    #[test]
    fn default_is_normal() {
        assert_eq!(DecayPolicy::default(), DecayPolicy::Normal);
    }
}
