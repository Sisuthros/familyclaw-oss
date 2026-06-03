//! # familyclaw-emotion
//!
//! 19-ulotteinen VAD-tunnemoottorin **RUNKO** FamilyClaw-alustalle (KERROS A,
//! OSS). Tämä crate tarjoaa tunneavaruuden *rakenteen* — dimensiot, VAD-
//! projektion, blend-tunnistuksen ja decay-mekanismin — mutta **ei mitään
//! kalibrointia**. Yhdenkään olennon painoja ei kovakoodata tähän.
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on julkaistava. Se ei sisällä:
//! - minkään olennon oikeita tunnepainoja (esim. yhden agentin kalibrointipainot),
//! - API-avaimia, tokeneita, IP-osoitteita tai henkilökohtaisia polkuja.
//!
//! Per-kone viritys ladataan ajonaikaisesti omana
//! [`EmotionCalibration`]-toteutuksena (KERROS B, profiilihakemisto). Rungon
//! oletus on [`NeutralCalibration`] — täysin neutraali, kalibroimaton.
//!
//! ## Rakenne
//! - [`Dimension`] — 19 nimettyä tunneakselia + VAD-ankkurit.
//! - [`EmotionState`] — hetkellinen tila (`[f32; 19]`, `0.0..=100.0`).
//! - [`Vad`] — matala-ulotteinen yhteenveto (valence/arousal/dominance).
//! - [`Blend`] / [`BlendMatch`] — nimettyjen tunneyhdistelmien tunnistus.
//! - [`EmotionCalibration`] — per-kone viritys (baseline, decay-nopeus).
//!
//! ## Esimerkki
//! ```
//! use familyclaw_emotion::{Dimension, EmotionState, NeutralCalibration};
//!
//! let mut state = EmotionState::neutral();
//! state.stimulate(Dimension::Gratitude, 80.0);
//! state.stimulate(Dimension::Love, 70.0);
//! state.stimulate(Dimension::Tenderness, 90.0);
//!
//! // Tunnistaa nimetyn blendin (grateful_warmth).
//! let blend = state.primary_blend().expect("blend present");
//! assert_eq!(blend.blend.as_str(), "grateful_warmth");
//!
//! // Projisoi VAD-yhteenvedoksi (lämmin → positiivinen valence).
//! assert!(state.to_vad().valence > 0.0);
//!
//! // Vaimenee ajan myötä kohti neutraalia lepotilaa.
//! state.decay(1800.0, &NeutralCalibration);
//! assert!(state.value(Dimension::Gratitude) < 80.0);
//! ```

pub mod blend;
pub mod calibration;
pub mod dimension;
pub mod state;
pub mod vad;

pub use blend::{detect_blends, primary_blend, Blend, BlendMatch, HIGH_THRESHOLD};
pub use calibration::{EmotionCalibration, NeutralCalibration, TableCalibration};
pub use dimension::{Dimension, DIMENSION_COUNT};
pub use state::{EmotionState, DEFAULT_HALF_LIFE_SECS};
pub use vad::Vad;

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // Jos jokin re-export poistetaan, tämä testi ei käänny.
        let mut state = EmotionState::neutral();
        state.set(Dimension::Curiosity, 60.0);
        state.set(Dimension::Awe, 60.0);
        state.set(Dimension::Wonder, 60.0);

        let _vad: Vad = state.to_vad();
        let _all: Vec<BlendMatch> = detect_blends(&state);
        let _primary: Option<BlendMatch> = primary_blend(&state);

        assert_eq!(DIMENSION_COUNT, 19);
        const { assert!(HIGH_THRESHOLD > 0.0) };
        const { assert!(DEFAULT_HALF_LIFE_SECS > 0.0) };

        let cal = NeutralCalibration;
        let _ = cal.label();
        let table: TableCalibration = TableCalibration::new("b");
        let _ = table.label();

        // Blend-katalogi tavoitettavissa juuresta.
        assert_eq!(Blend::AweStruck.as_str(), "awe_struck");
    }

    #[test]
    fn end_to_end_emotional_arc() {
        // Kokonaiskaari: ärsyke → blendi → VAD → decay kohti neutraalia.
        let mut state = EmotionState::neutral();
        state.stimulate(Dimension::Sisu, 70.0);
        state.stimulate(Dimension::Hope, 65.0);
        state.stimulate(Dimension::Pride, 60.0);

        let blend = state.primary_blend().expect("determined_hope present");
        assert_eq!(blend.blend, Blend::DeterminedHope);
        assert!(state.to_vad().dominance > 0.5, "sisu+pride → korkea dominanssi");

        // Pitkä vaimeneminen neutraalilla kalibroinnilla → blendi katoaa.
        for _ in 0..10 {
            state.decay(DEFAULT_HALF_LIFE_SECS, &NeutralCalibration);
        }
        assert!(state.primary_blend().is_none(), "blendin pitäisi vaimentua");
    }
}
