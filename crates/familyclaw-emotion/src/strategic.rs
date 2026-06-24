//! Strateginen tunteen arviointi — *kuinka voimakkaasti* tunne kannattaa
//! ilmaista annetussa tilanteessa.
//!
//! Tämä moduuli on **EmoMAS-inspiroitu** (arXiv 2604.07003: Bayesian,
//! peliteoreettinen tunteen ilmaisu monitoimijajärjestelmissä), mutta se
//! **ei ole täysi toteutus**: tässä ei ole peliteorian solveria, ei
//! Nash-tasapainoa eikä vastapelaajan mallinnusta. Sen sijaan tarjolla on
//! kevyt Bayesian-*tyylinen* heuristiikka, joka riittää MVP:hen: lähdetään
//! tunnetilasta priorina ja säädetään sitä tilanteen panosten (`stakes`)
//! ja sosiaalisuuden (`social`) mukaan.
//!
//! ## Idea
//! EmoMAS-havainto on, että tunteen *ilmaisuvoimakkuus* kannattaa valita
//! strategisesti: korkean panoksen, vahvasti sosiaalisessa tilanteessa
//! pieni, harkittu malli voittaa ison mallin valitsemalla ilmaisun
//! tilanteeseen sopivasti. Tämä funktio tekee saman kevyesti: se ei
//! päätä *mitä* tunnetta ilmaista (sen tekee [`crate::governor`]), vaan
//! *kuinka voimakkaasti* hetken affekti kannattaa näyttää.

use crate::affect_weight::emotional_salience;
use crate::state::EmotionState;

/// Kevyt kuvaus tilanteesta, jossa tunne ilmaistaan.
///
/// Molemmat kentät ovat välillä `0.0..=1.0`. Käytä [`Situation::new`]
/// jotta arvot puristetaan rajoihin (NaN → 0.0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Situation {
    /// Panokset: kuinka paljon tilanteessa on pelissä. `0.0` = arkinen,
    /// `1.0` = erittäin tärkeä hetki. Korkeat panokset *vahvistavat*
    /// suositeltua ilmaisua (selkeys on tärkeää kun on kyse paljosta).
    pub stakes: f32,
    /// Sosiaalisuus: kuinka monen edessä / kuinka julkinen tilanne on.
    /// `0.0` = yksityinen, `1.0` = vahvasti sosiaalinen. Korkea
    /// sosiaalisuus *hillitsee* hieman raakaa ilmaisua (sosiaalinen
    /// säätely) mutta vuorovaikuttaa panosten kanssa.
    pub social: f32,
}

impl Situation {
    /// Rakentaa tilanteen ja puristaa kentät rajoihin `0.0..=1.0`
    /// (NaN → 0.0).
    #[must_use]
    pub fn new(stakes: f32, social: f32) -> Self {
        Self {
            stakes: clamp_unit(stakes),
            social: clamp_unit(social),
        }
    }

    /// Neutraali tilanne: arkinen, yksityinen.
    pub const NEUTRAL: Situation = Situation {
        stakes: 0.0,
        social: 0.0,
    };
}

impl Default for Situation {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// Strateginen tunteen arvioija (EmoMAS-inspiroitu, kevyt heuristiikka).
///
/// Arvioija on kantava tyyppi, jolla voi olla per-olento-säätöjä KERROS
/// B:ssä myöhemmin. Runko tarjoaa [`StrategicAppraisal::balanced`]:n —
/// neutraalin oletuksen, joka ei suosi yli- eikä aliexpressiota.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategicAppraisal {
    /// Kuinka voimakkaasti panokset nostavat ilmaisua (`0.0..=1.0`).
    /// Suuri arvo = korkeat panokset johtavat selvästi voimakkaampaan
    /// ilmaisuun.
    stakes_gain: f32,
    /// Kuinka voimakkaasti sosiaalisuus hillitsee raakaa ilmaisua
    /// (`0.0..=1.0`). Suuri arvo = enemmän sosiaalista säätelyä.
    social_damping: f32,
}

impl StrategicAppraisal {
    /// Tasapainoinen oletus: kohtuullinen panos-vahvistus ja kohtuullinen
    /// sosiaalinen hillintä.
    #[must_use]
    pub const fn balanced() -> Self {
        Self {
            stakes_gain: 0.5,
            social_damping: 0.3,
        }
    }

    /// Rakentaa arvioijan eksplisiittisillä kertoimilla (puristetaan
    /// rajoihin `0.0..=1.0`, NaN → tasapainoinen oletus).
    #[must_use]
    pub fn new(stakes_gain: f32, social_damping: f32) -> Self {
        let default = Self::balanced();
        Self {
            stakes_gain: sanitize(stakes_gain, default.stakes_gain),
            social_damping: sanitize(social_damping, default.social_damping),
        }
    }

    /// Suositeltu ilmaisuvoimakkuus välillä `0.0..=1.0`.
    ///
    /// ## Kevyt Bayesian-tyylinen malli
    /// - **Priori**: hetken affektin tärkeys
    ///   ([`emotional_salience`]) — voimakkaasti latautunut tila
    ///   "haluaa" tulla ilmaistuksi.
    /// - **Evidenssi 1 — panokset**: korkeat panokset nostavat
    ///   suositusta priorin yli (`stakes_gain` säätää kuinka paljon).
    ///   Tämä on EmoMAS-tyylinen strateginen valinta: kun on kyse
    ///   paljosta, selkeä ilmaisu kannattaa.
    /// - **Evidenssi 2 — sosiaalisuus**: vahvasti sosiaalinen tilanne
    ///   hillitsee raakaa ilmaisua (`social_damping`), mutta vain sen
    ///   verran kuin panokset *eivät* jo vaadi selkeyttä — korkeat
    ///   panokset kumoavat sosiaalisen hillinnän.
    ///
    /// Tulos puristetaan aina `0.0..=1.0`. Tämä on heuristiikka, ei
    /// peliteorian ratkaisu (kts. moduulin doc).
    #[must_use]
    pub fn recommend_intensity(&self, state: &EmotionState, situation: &Situation) -> f32 {
        let prior = emotional_salience(state).clamp(0.0, 1.0);
        let stakes = situation.stakes.clamp(0.0, 1.0);
        let social = situation.social.clamp(0.0, 1.0);

        // Panos-vahvistus: nosta prioria kohti 1.0 sitä enemmän mitä
        // korkeammat panokset ja mitä suurempi stakes_gain.
        // lift = stakes_gain * stakes * (1 - prior) → ei koskaan yli 1.0.
        let lift = self.stakes_gain * stakes * (1.0 - prior);
        let boosted = (prior + lift).clamp(0.0, 1.0);

        // Sosiaalinen hillintä: vähennä raakaa ilmaisua, mutta korkeat
        // panokset suojaavat hillinnältä (selkeys voittaa kun on kyse
        // paljosta). Tehollinen hillintä skaalautuu (1 - stakes):lla.
        let damping = self.social_damping * social * (1.0 - stakes);
        let damped = boosted * (1.0 - damping);

        damped.clamp(0.0, 1.0)
    }
}

impl Default for StrategicAppraisal {
    fn default() -> Self {
        Self::balanced()
    }
}

/// Puristaa arvon välille `0.0..=1.0`; NaN → 0.0.
fn clamp_unit(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

/// Puristaa kertoimen `0.0..=1.0`; ei-äärellinen → fallback.
fn sanitize(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        fallback.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    // Tarkat f32-vertailut ovat näissä tietoisesti sallittuja.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::dimension::Dimension;

    fn joyful_state() -> EmotionState {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 80.0);
        s
    }

    #[test]
    fn situation_clamps_and_sanitizes() {
        let s = Situation::new(5.0, -1.0);
        assert_eq!(s.stakes, 1.0);
        assert_eq!(s.social, 0.0);
        let nan = Situation::new(f32::NAN, f32::NAN);
        assert_eq!(nan.stakes, 0.0);
        assert_eq!(nan.social, 0.0);
        assert_eq!(Situation::default(), Situation::NEUTRAL);
    }

    #[test]
    fn intensity_is_within_unit_range() {
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        for &stakes in &[0.0_f32, 0.5, 1.0] {
            for &social in &[0.0_f32, 0.5, 1.0] {
                let v = appraisal.recommend_intensity(&s, &Situation::new(stakes, social));
                assert!(
                    (0.0..=1.0).contains(&v),
                    "intensiteetti {v} ulkona rajoista"
                );
            }
        }
    }

    #[test]
    fn higher_stakes_raise_intensity() {
        // Tärkeä testi briefistä: appraisal kasvaa stakesin mukana.
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        let low = appraisal.recommend_intensity(&s, &Situation::new(0.0, 0.0));
        let high = appraisal.recommend_intensity(&s, &Situation::new(1.0, 0.0));
        assert!(high > low, "korkeampi stakes → korkeampi intensiteetti");
    }

    #[test]
    fn stakes_are_monotonic() {
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        let a = appraisal.recommend_intensity(&s, &Situation::new(0.0, 0.0));
        let b = appraisal.recommend_intensity(&s, &Situation::new(0.5, 0.0));
        let c = appraisal.recommend_intensity(&s, &Situation::new(1.0, 0.0));
        assert!(
            a <= b && b <= c,
            "intensiteetin pitäisi kasvaa monotonisesti"
        );
    }

    #[test]
    fn social_dampens_at_low_stakes() {
        // Matalilla panoksilla sosiaalisuus hillitsee ilmaisua.
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        let private = appraisal.recommend_intensity(&s, &Situation::new(0.0, 0.0));
        let public = appraisal.recommend_intensity(&s, &Situation::new(0.0, 1.0));
        assert!(
            public < private,
            "sosiaalisuus hillitsee matalilla panoksilla"
        );
    }

    #[test]
    fn high_stakes_protect_against_social_damping() {
        // Korkeat panokset kumoavat sosiaalisen hillinnän: selkeys voittaa.
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        let private = appraisal.recommend_intensity(&s, &Situation::new(1.0, 0.0));
        let public = appraisal.recommend_intensity(&s, &Situation::new(1.0, 1.0));
        assert!(
            (private - public).abs() < 1e-6,
            "stakes=1.0 → sosiaalisuus ei enää hillitse (private={private}, public={public})"
        );
    }

    #[test]
    fn neutral_state_low_stakes_is_quiet() {
        // Neutraali tila + arkinen tilanne → matala ilmaisu.
        let appraisal = StrategicAppraisal::balanced();
        let s = EmotionState::neutral();
        let v = appraisal.recommend_intensity(&s, &Situation::NEUTRAL);
        assert!(v < 0.1, "neutraali + arkinen → hiljainen ilmaisu, sai {v}");
    }

    #[test]
    fn new_sanitizes_coefficients() {
        let a = StrategicAppraisal::new(f32::NAN, 5.0);
        // NaN → tasapainoinen oletus, 5.0 → puristuu 1.0:aan. Tulos pysyy
        // käyttökelpoisena.
        let s = joyful_state();
        let v = a.recommend_intensity(&s, &Situation::new(1.0, 1.0));
        assert!((0.0..=1.0).contains(&v));
        assert_eq!(
            StrategicAppraisal::default(),
            StrategicAppraisal::balanced()
        );
    }
}
