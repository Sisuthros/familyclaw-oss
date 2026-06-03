//! Human-in-the-loop -korjaukset — ihmisen veto muistin haussa.
//!
//! [`HumanCorrection`] on suoraan ihmiseltä (esim. perheen ihmisjäseneltä)
//! tuleva ohjaava korjaus, jolla on muistin haussa **korkein mahdollinen
//! prioriteetti** ([`CorrectionPriority::MAX`] = `1.0`). Kun retrieval tuottaa
//! tasapelin tai kun automaattinen muisto on ristiriidassa ihmiskorjauksen
//! kanssa, ihmiskorjaus **voittaa aina**.
//!
//! ## Decay: Slow, ei ikuinen
//! Toisin kuin identity-anchor (λ=0, [`crate::DecayLambda::ZERO`]), ihmiskorjaus
//! ei ole ikuinen — se vaimenee **hitaasti** ([`DecayClass::Slow`]). Korjaus
//! ("älä koskaan tee X", "Y on virhe") on hyvin pitkäikäinen mutta saa lopulta
//! väistyä jos sitä ei vahvisteta — toisin kuin identiteetti, joka on pysyvä.
//!
//! ## Prioriteetin laskenta
//! Hausssa korjauksen tehollinen pistemäärä on
//! `priority · retention(ikä)`. Koska decay on hidas, ihmiskorjaus pysyy
//! retrievalin kärjessä pitkään, mutta sen vaikutus haalistuu jos sitä ei
//! koskaan vahvisteta uudelleen.

use serde::{Deserialize, Serialize};

use familyclaw_core::time::{self, Timestamp};

use crate::anchor::DecayLambda;
use crate::error::{Result, SecurityError};

/// Muiston decay-luokka — nimetty unohtumisnopeus.
///
/// Luokat kartoittuvat konkreettisiin λ-kertoimiin ([`DecayClass::lambda`]).
/// Turvakerros käyttää näistä kahta: [`DecayClass::Eternal`] identity-
/// anchoreille ja [`DecayClass::Slow`] ihmiskorjauksille. Muut luokat ovat
/// tarjolla muisti-substraatin (familyclaw-memory) yleiseen decay-laskentaan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecayClass {
    /// Ei koskaan unohdu (λ=0) — identity-anchorit.
    Eternal,
    /// Hyvin hidas unohtuminen — ihmiskorjaukset (ihmisen veto-oikeus).
    Slow,
    /// Tavanomainen Ebbinghaus-vaimeneminen.
    Normal,
    /// Nopea unohtuminen — ohimenevä, vähämerkityksinen tieto.
    Fast,
}

impl DecayClass {
    /// Puolittumisaika sekunteina kullekin luokalle (paitsi [`Self::Eternal`]).
    ///
    /// Arvot ovat *rungon* oletuksia (KERROS A) — ne eivät kalibroi mitään
    /// olentoa. Per-kone viritys voi korvata nämä ajonaikaisesti.
    const SLOW_HALF_LIFE_SECS: f64 = 60.0 * 60.0 * 24.0 * 90.0; // ~90 vrk
    const NORMAL_HALF_LIFE_SECS: f64 = 60.0 * 60.0 * 24.0 * 7.0; // ~7 vrk
    const FAST_HALF_LIFE_SECS: f64 = 60.0 * 60.0; // 1 h

    /// Palauttaa luokkaa vastaavan decay-λ:n.
    ///
    /// λ johdetaan puolittumisajasta: `λ = ln(2) / half_life`. [`Self::Eternal`]
    /// antaa [`DecayLambda::ZERO`].
    #[must_use]
    pub fn lambda(self) -> DecayLambda {
        let half_life = match self {
            Self::Eternal => return DecayLambda::ZERO,
            Self::Slow => Self::SLOW_HALF_LIFE_SECS,
            Self::Normal => Self::NORMAL_HALF_LIFE_SECS,
            Self::Fast => Self::FAST_HALF_LIFE_SECS,
        };
        // half_life on aina > 0 vakio, joten new() ei voi epäonnistua; mutta
        // emme käytä unwrap/expect tuotantopolulla — johda λ suoraan.
        DecayLambda::new(std::f64::consts::LN_2 / half_life).unwrap_or(DecayLambda::ZERO)
    }

    /// Onko tämä ikuinen luokka (ei unohdu).
    #[must_use]
    pub fn is_eternal(self) -> bool {
        matches!(self, Self::Eternal)
    }
}

/// Korjauksen prioriteetti välillä `0.0..=1.0`.
///
/// Ihmiskorjaus käyttää aina [`CorrectionPriority::MAX`] (`1.0`), joka takaa
/// sille korkeimman painon retrievalissa. Tyyppi on uusi (newtype) jotta
/// prioriteetti pysyy rajatulla välillä eikä sekoitu muihin `f64`-arvoihin.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CorrectionPriority(f64);

impl CorrectionPriority {
    /// Korkein prioriteetti (`1.0`) — ihmiskorjauksen oletus.
    pub const MAX: Self = Self(1.0);

    /// Matalin prioriteetti (`0.0`).
    pub const MIN: Self = Self(0.0);

    /// Rakentaa prioriteetin. Arvon on oltava `0.0..=1.0` ja äärellinen.
    ///
    /// # Errors
    /// [`SecurityError::InvalidInput`] jos arvo ei ole äärellinen tai on
    /// välin `0.0..=1.0` ulkopuolella.
    pub fn new(value: f64) -> Result<Self> {
        if !value.is_finite() {
            return Err(SecurityError::invalid_input(format!(
                "priority must be finite, got {value}"
            )));
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(SecurityError::invalid_input(format!(
                "priority must be in 0.0..=1.0, got {value}"
            )));
        }
        Ok(Self(value))
    }

    /// Palauttaa prioriteetin liukulukuna.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Default for CorrectionPriority {
    /// Oletuksena korkein prioriteetti — ihmiskorjaus voittaa lähtökohtaisesti.
    fn default() -> Self {
        Self::MAX
    }
}

/// Ihmisen antama korjaus — ohjaava veto muistin haussa.
///
/// Korjaus kantaa sisällön ([`content`](HumanCorrection::content)), korkean
/// prioriteetin ja hitaan decayn. Se ei poista muita muistoja — se voittaa ne
/// painotuksessa.
///
/// **OSS-raja:** tyyppi on geneerinen runko. Korjauksen *sisältö* (ihmisen
/// konkreettiset vetot) on KERROS B -dataa, ei tätä koodia.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HumanCorrection {
    /// Korjauksen tekstisisältö (ohje, kielto tai oikaisu).
    pub content: String,

    /// Korjauksen prioriteetti retrievalissa — oletuksena
    /// [`CorrectionPriority::MAX`].
    pub priority: CorrectionPriority,

    /// Korjauksen decay-luokka — aina [`DecayClass::Slow`].
    pub decay: DecayClass,

    /// Milloin korjaus annettiin (UTC).
    pub applied_at: Timestamp,
}

impl HumanCorrection {
    /// Rakentaa ihmiskorjauksen: prioriteetti `1.0`, decay `Slow`, aikaleima nyt.
    ///
    /// # Errors
    /// [`SecurityError::InvalidInput`] jos `content` on tyhjä.
    pub fn new(content: impl Into<String>) -> Result<Self> {
        let content = content.into();
        if content.trim().is_empty() {
            return Err(SecurityError::invalid_input("correction content must not be empty"));
        }
        Ok(Self {
            content,
            priority: CorrectionPriority::MAX,
            decay: DecayClass::Slow,
            applied_at: time::now(),
        })
    }

    /// Tehollinen retrieval-pistemäärä iässä `age_secs`: `priority · retention`.
    ///
    /// Tätä verrataan tavallisten muistojen pistemääriin haussa. Koska
    /// ihmiskorjauksen prioriteetti on `1.0` ja decay hidas, pistemäärä pysyy
    /// pitkään muiden yläpuolella.
    #[must_use]
    pub fn effective_score(&self, age_secs: f64) -> f64 {
        self.priority.get() * self.decay.lambda().retention(age_secs)
    }

    /// Voittaako tämä korjaus annetun kilpailevan pistemäärän iässä `age_secs`.
    ///
    /// Käytetään tasapelien ratkaisuun: ihmiskorjaus voittaa myös täsmälleen
    /// yhtä suuren kilpailijan (`>=`), jotta ihmisen veto saa edun pelkän
    /// automaattisen muiston yli.
    #[must_use]
    pub fn wins_against(&self, competitor_score: f64, age_secs: f64) -> bool {
        self.effective_score(age_secs) >= competitor_score
    }
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti tunnettuja f64-vakioita (0.0, 1.0) — tarkka
    // vertailu on tässä tarkoituksellista ja oikein.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn decay_class_eternal_has_zero_lambda() {
        assert!(DecayClass::Eternal.lambda().is_eternal());
        assert!(DecayClass::Eternal.is_eternal());
    }

    #[test]
    fn decay_class_ordering_of_speed() {
        // Nopeampi luokka = suurempi λ.
        let slow = DecayClass::Slow.lambda().get();
        let normal = DecayClass::Normal.lambda().get();
        let fast = DecayClass::Fast.lambda().get();
        assert!(slow > 0.0);
        assert!(slow < normal, "slow should decay slower than normal");
        assert!(normal < fast, "normal should decay slower than fast");
    }

    #[test]
    fn slow_decay_retains_most_after_a_day() {
        // ~90 vrk puolittumisaika → yhden vuorokauden jälkeen ~99 % jäljellä.
        let day = 60.0 * 60.0 * 24.0;
        let retention = DecayClass::Slow.lambda().retention(day);
        assert!(retention > 0.98, "slow retention after 1 day was {retention}");
    }

    #[test]
    fn priority_max_is_one_min_is_zero() {
        assert_eq!(CorrectionPriority::MAX.get(), 1.0);
        assert_eq!(CorrectionPriority::MIN.get(), 0.0);
    }

    #[test]
    fn priority_default_is_max() {
        assert_eq!(CorrectionPriority::default(), CorrectionPriority::MAX);
    }

    #[test]
    fn priority_rejects_out_of_range_and_nonfinite() {
        assert!(CorrectionPriority::new(-0.01).is_err());
        assert!(CorrectionPriority::new(1.01).is_err());
        assert!(CorrectionPriority::new(f64::NAN).is_err());
        assert!(CorrectionPriority::new(0.0).is_ok());
        assert!(CorrectionPriority::new(1.0).is_ok());
        assert!(CorrectionPriority::new(0.5).is_ok());
    }

    #[test]
    fn correction_new_sets_max_priority_and_slow_decay() {
        let c = HumanCorrection::new("agent_a lives in city X, not city Y").expect("valid");
        assert_eq!(c.priority, CorrectionPriority::MAX);
        assert_eq!(c.decay, DecayClass::Slow);
        assert_eq!(c.content, "agent_a lives in city X, not city Y");
    }

    #[test]
    fn correction_new_rejects_empty_content() {
        assert!(HumanCorrection::new("   ").is_err());
        assert!(HumanCorrection::new("").is_err());
    }

    #[test]
    fn correction_effective_score_starts_at_priority() {
        let c = HumanCorrection::new("rule").expect("valid");
        // Ikä 0 → retention 1.0 → pistemäärä = prioriteetti = 1.0.
        let score = c.effective_score(0.0);
        assert!((score - 1.0).abs() < 1e-9, "score was {score}");
    }

    #[test]
    fn correction_wins_ties_against_automatic_memory() {
        let c = HumanCorrection::new("the veto").expect("valid");
        // Tuore korjaus (score 1.0) voittaa täsmälleen yhtä suuren kilpailijan.
        assert!(c.wins_against(1.0, 0.0));
        // Ja kaiken pienemmän.
        assert!(c.wins_against(0.9, 0.0));
        // Mutta ei kilpailijaa joka on aidosti suurempi (esim. toinen veto).
        assert!(!c.wins_against(1.0001, 0.0));
    }

    #[test]
    fn correction_score_decays_slowly_but_monotonically() {
        let c = HumanCorrection::new("rule").expect("valid");
        let month = 60.0 * 60.0 * 24.0 * 30.0;
        let year = 60.0 * 60.0 * 24.0 * 365.0;

        let fresh = c.effective_score(0.0);
        let aged_month = c.effective_score(month);
        let aged_year = c.effective_score(year);

        // Monotonisesti vähenevä, mutta ei katoa täysin.
        assert!(aged_month < fresh, "month score should be below fresh");
        assert!(aged_year < aged_month, "year score should be below month");
        assert!(aged_year > 0.0, "should not vanish entirely in a year");

        // ~90 vrk puolittumisaika: kuukauden jälkeen yhä retrievalin kärjessä
        // tavallista muistoa vastaan (veto on pitkäikäinen).
        assert!(aged_month > 0.7, "month retention was {aged_month}");
        assert!(c.wins_against(0.7, month));

        // Vuoden jälkeen veto on haalistunut selvästi (~4 puolittumisaikaa) —
        // identiteetti olisi pysynyt (λ=0), mutta korjaus saa väistyä.
        assert!(aged_year < 0.2, "year retention was {aged_year}");
    }

    #[test]
    fn correction_serde_roundtrip() {
        let c = HumanCorrection::new("important veto").expect("valid");
        let json = serde_json::to_string(&c).expect("serialize");
        let back: HumanCorrection = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }

    #[test]
    fn decay_class_serde_roundtrip() {
        for class in [
            DecayClass::Eternal,
            DecayClass::Slow,
            DecayClass::Normal,
            DecayClass::Fast,
        ] {
            let json = serde_json::to_string(&class).expect("serialize");
            let back: DecayClass = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(class, back);
        }
        // snake_case-muoto.
        assert_eq!(
            serde_json::to_string(&DecayClass::Slow).expect("ser"),
            "\"slow\""
        );
    }
}
