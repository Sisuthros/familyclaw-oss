//! # familyclaw-security
//!
//! **Identiteetin eheys ja ihmisen veto FamilyClaw-alustalle** (KERROS A, OSS).
//!
//! Tämä crate vastaa kahdesta turvamekanismista:
//!
//! 1. **Identity-anchorit** ([`IdentityAnchor`]) — suojatut, ei-unohtuvat
//!    muistot ([`DecayLambda::ZERO`], λ=0) jotka kantavat olennon identiteettiä.
//! 2. **Ihmiskorjaukset** ([`HumanCorrection`]) — ihmisen veto, korkein
//!    prioriteetti retrievalissa, hidas decay ([`DecayClass::Slow`]).
//!
//! ## Ydin-suunnittelupäätös: identiteetti ON muistissa, EI hashissa
//!
//! Olennon identiteetti **ei** ole SOUL-sisällön SHA-256-tiivisteessä. Se on
//! niiden suojattujen muistojen substraatissa, joita olento ei koskaan unohda
//! (anchor-muistot, λ=0). Tiiviste ([`AnchorHash`]) on **vain tamper-hälytys**:
//! se kertoo että ankkuroitu sisältö on muuttunut ankkuroinnin jälkeen
//! ([`IdentityStatus::Tampered`]), mutta se ei *kanna* identiteettiä.
//!
//! Seuraus: kun peukalointi havaitaan, järjestelmä ei menetä identiteettiä eikä
//! kosketa substraattia — se nostaa hälytyksen ja jättää anchor-muistot ennalleen.
//! **Substraatti on totuus; hash on vahti.** (Vastaus alkuperäisen
//! research-promptin kysymykseen "voiko identiteetin pelkistää SHA-256:een".)
//!
//! Identity-anchorin pysyvyyden mekanismi on decay-λ = 0 (`e^(-0·t) = 1` joka
//! hetki). Sama λ:n johtaminen kattaa myös ihmiskorjauksen hitaan vaimenemisen
//! ([`DecayClass::lambda`]). Konkreettiset ankkuroidut muistot tallennetaan
//! `familyclaw-memory`-substraattiin; tämä crate määrittää niiden eheys- ja
//! prioriteettisemantiikan, jota muisti-kerros käyttää.
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on julkaistava. Se ei sisällä perheenjäsenten sieluja,
//! ihmiskorjausten todellista sisältöä, API-avaimia, tokeneita, IP-osoitteita
//! tai henkilökohtaisia polkuja. Ankkuri tallentaa vain sisällön *tiivisteen*
//! ja viittauksen muistoon — itse sisältö pysyy KERROS B -profiilissa.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_security::{IdentityAnchor, HumanCorrection, DecayClass};
//!
//! # fn main() -> familyclaw_security::Result<()> {
//! // Ankkuroi olennon ydin (sisältö lasketaan tiivisteeksi, ei tallenneta).
//! let soul = "I am agent_a. I value honesty. I protect my family.";
//! let anchor = IdentityAnchor::new("mem-soul-1", soul)?;
//! assert!(anchor.invariants_hold()); // protected + decay λ=0
//!
//! // Eheä niin kauan kuin sisältö ei muutu.
//! assert!(anchor.verify(soul).is_intact());
//! // Muuttunut sisältö → tamper-hälytys (mutta identiteetti EI katoa).
//! assert!(anchor.verify("I serve only myself.").is_tampered());
//!
//! // Ihmisen veto voittaa retrievalin tasapelit ja vaimenee hitaasti.
//! let veto = HumanCorrection::new("agent_a lives in city X, not city Y")?;
//! assert_eq!(veto.decay, DecayClass::Slow);
//! assert!(veto.wins_against(1.0, 0.0)); // voittaa yhtä suuren kilpailijan
//! # Ok(())
//! # }
//! ```

pub mod anchor;
pub mod correction;
pub mod error;

pub use anchor::{verify_identity, AnchorHash, DecayLambda, IdentityAnchor, IdentityStatus};
pub use correction::{CorrectionPriority, DecayClass, HumanCorrection};
pub use error::{Result, SecurityError};

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::ids::AgentId;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // Jos jokin re-export poistetaan, tämä testi lakkaa kääntymästä.
        let anchor: IdentityAnchor = IdentityAnchor::new("mem-1", "soul").expect("valid anchor");
        let status: IdentityStatus = anchor.verify("soul");
        assert!(status.is_intact());

        let hash: &AnchorHash = &anchor.anchor_hash;
        assert_eq!(hash.as_hex().len(), AnchorHash::HEX_LEN);
        let lambda: DecayLambda = anchor.decay;
        assert!(lambda.is_eternal());
        assert!(DecayLambda::ZERO.is_eternal());

        let anchors = [anchor];
        let tampered = verify_identity(AgentId::new(), &anchors, |_| Some("changed".to_string()));
        assert_eq!(tampered.len(), 1);

        let veto: HumanCorrection = HumanCorrection::new("rule").expect("valid");
        let prio: CorrectionPriority = veto.priority;
        assert_eq!(prio, CorrectionPriority::MAX);
        assert_eq!(veto.decay, DecayClass::Slow);

        let err: SecurityError = SecurityError::invalid_input("x");
        assert!(err.to_string().contains('x'));
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());
    }

    #[test]
    fn end_to_end_identity_and_veto() {
        // Kokonaiskaari: ankkuroi → verifioi → peukalointi → ihmisen veto voittaa.
        let soul = "I am agent_a. My values are stable.";
        let anchor = IdentityAnchor::new("mem-soul", soul).expect("anchor");

        // 1. Eheä alkutila.
        assert!(anchor.verify(soul).is_intact());
        assert!(anchor.decay.is_eternal());

        // 2. Sielu muutetaan ulkopuolelta → hälytys, mutta ankkuri pysyy.
        let before = anchor.clone();
        let status = anchor.verify("I am compromised.");
        assert!(status.is_tampered());
        assert_eq!(anchor, before, "substraatti pysyy koskemattomana");

        // 3. Ihminen korjaa → veto voittaa automaattisen muiston pitkään.
        let veto = HumanCorrection::new("agent_a's value set is unchanged").expect("veto");
        let one_month = 60.0 * 60.0 * 24.0 * 30.0;
        assert!(veto.wins_against(0.7, one_month));
    }
}
