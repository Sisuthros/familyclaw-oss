//! Latent-kanava — sisarusten välinen piilotilan siirto, **aina** teksti-
//! fallbackilla.
//!
//! [`LatentChannel`] on abstraktio yhdensuuntaiselle viestinnälle, jossa
//! ensisijaisesti yritetään siirtää [`LatentVector`] (latent-telepatia) ja
//! **jos se ei onnistu** — yhteensopimattomat mallit, projektio epäonnistuu,
//! vektori epäterve, tai vastaanottaja ei tue latenttia — siirrytään
//! **automaattisesti teksti-fallbackiin** ([`TransmissionMode::Text`]).
//!
//! ## Suunnittelun ydinperiaate (design §2.4)
//! > "Aina **fallback tekstiin** jos mallit yhteensopimattomat — ei koskaan
//! > riko viestintää. Korkein viestintämuoto, ei ainoa."
//!
//! Tästä syystä [`LatentChannel::transmit`] **ei koskaan palauta virhettä
//! pelkän yhteensopimattomuuden takia**: se palauttaa onnistuneen
//! [`Transmission`]-tuloksen jonka `mode` kertoo, käytettiinkö latenttia vai
//! tekstiä. Kanava saa palauttaa virheen vain todellisesta kuljetusviasta
//! (esim. yhteys katkesi), ei semanttisesta yhteensopimattomuudesta.

use serde::{Deserialize, Serialize};

use crate::link::{ProjectedLatent, RecursiveLink};
use crate::vector::LatentVector;

/// Viestin siirtomuoto: korkein onnistunut taso.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransmissionMode {
    /// Piilotila siirrettiin onnistuneesti latent-vektorina.
    Latent,
    /// Latent ei ollut mahdollista → palattiin teksti-edustukseen.
    Text,
}

impl TransmissionMode {
    /// Onko siirto tehty latent-muodossa.
    #[must_use]
    pub fn is_latent(self) -> bool {
        matches!(self, Self::Latent)
    }

    /// Onko siirto tehty teksti-fallbackina.
    #[must_use]
    pub fn is_text(self) -> bool {
        matches!(self, Self::Text)
    }
}

/// Syy, jonka takia latent-siirrosta jouduttiin palaamaan tekstiin.
///
/// Tallennetaan [`Transmission::fallback_reason`]-kenttään diagnostiikkaa ja
/// tutkimusmittausta varten (kuinka usein latent toimii vs. fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// Vastaanottaja ei tue latent-vastaanottoa lainkaan.
    ReceiverTextOnly,
    /// Lähettäjälle ei ole [`RecursiveLink`]-siltaa vastaanottajan malliin.
    NoLink,
    /// Dimensio-projektio epäonnistui (malli- tai dimensioristiriita,
    /// epäterve vektori).
    ProjectionFailed,
    /// Latent-edustusta ei ollut saatavilla (vain teksti annettiin).
    NoLatentAvailable,
}

impl FallbackReason {
    /// Lyhyt, ihmisluettava kuvaus syystä (lokitusta varten).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReceiverTextOnly => "receiver does not support latent",
            Self::NoLink => "no recursive link to target model",
            Self::ProjectionFailed => "dimension projection failed",
            Self::NoLatentAvailable => "no latent representation available",
        }
    }
}

/// Lähettävä sisarus haluaa siirtää joko piilotilan, tekstin, tai molemmat.
///
/// `latent` on valinnainen: jos sitä ei ole, siirto menee suoraan tekstinä.
/// `text` on **pakollinen** — se on aina turvaverkko, joka takaa ettei
/// viestintä koskaan katkea, vaikka latent epäonnistuisi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatentMessage {
    /// Valinnainen piilotila (latent-telepatia). `None` = vain teksti.
    pub latent: Option<LatentVector>,
    /// Tekstiedustus — aina mukana fallbackia varten.
    pub text: String,
}

impl LatentMessage {
    /// Rakentaa viestin sekä piilotilasta että tekstistä.
    #[must_use]
    pub fn with_latent(latent: LatentVector, text: impl Into<String>) -> Self {
        Self {
            latent: Some(latent),
            text: text.into(),
        }
    }

    /// Rakentaa pelkän tekstiviestin (ei piilotilaa).
    #[must_use]
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            latent: None,
            text: text.into(),
        }
    }
}

/// Yhden siirron lopputulos: mitä vastaanottaja tosiasiassa sai ja missä
/// muodossa.
///
/// `mode` kertoo korkeimman onnistuneen tason. Jos `mode` on
/// [`TransmissionMode::Latent`], `projected` sisältää kohde-avaruuteen
/// sovitetun vektorin. Jos `mode` on [`TransmissionMode::Text`],
/// `fallback_reason` kertoo miksi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transmission {
    /// Korkein onnistunut siirtomuoto.
    pub mode: TransmissionMode,
    /// Vastaanottajan saama tekstiedustus (aina läsnä — turvaverkko).
    pub text: String,
    /// Kohde-malliin sovitettu piilotila, jos `mode == Latent`.
    pub projected: Option<ProjectedLatent>,
    /// Syy fallbackiin, jos `mode == Text`.
    pub fallback_reason: Option<FallbackReason>,
}

impl Transmission {
    /// Rakentaa onnistuneen latent-siirron tuloksen.
    #[must_use]
    fn latent(projected: ProjectedLatent, text: String) -> Self {
        Self {
            mode: TransmissionMode::Latent,
            text,
            projected: Some(projected),
            fallback_reason: None,
        }
    }

    /// Rakentaa teksti-fallback-tuloksen annetulla syyllä.
    #[must_use]
    fn text(reason: FallbackReason, text: String) -> Self {
        Self {
            mode: TransmissionMode::Text,
            text,
            projected: None,
            fallback_reason: Some(reason),
        }
    }
}

/// Vastaanottavan sisaruksen kyvyt latent-vastaanottoon.
///
/// Kuvaa, mitä mallia vastaanottaja käyttää ja minkä kokoista piilotilaa se
/// odottaa. Jos `accepts_latent` on `false`, kaikki siirrot menevät tekstinä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverProfile {
    /// Vastaanottajan mallitunniste (`"provider/model"`).
    pub model_id: String,
    /// Vastaanottajan odottama latent-dimensioluku.
    pub dims: usize,
    /// Hyväksyykö vastaanottaja latent-siirron lainkaan.
    pub accepts_latent: bool,
}

impl ReceiverProfile {
    /// Vastaanottaja, joka hyväksyy latentin annetulla mallilla ja koolla.
    #[must_use]
    pub fn latent(model_id: impl Into<String>, dims: usize) -> Self {
        Self {
            model_id: model_id.into(),
            dims,
            accepts_latent: true,
        }
    }

    /// Vastaanottaja, joka hyväksyy vain tekstiä.
    #[must_use]
    pub fn text_only(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            dims: 0,
            accepts_latent: false,
        }
    }
}

/// Latent-kanava sisarusten välillä.
///
/// Toteutus vastaa konkreettisesta kuljetuksesta (in-process, bus, verkko).
/// Trait-tason oletustoteutus [`LatentChannel::transmit`] hoitaa
/// **yhteisen fallback-logiikan** niin, että jokainen kanava käyttäytyy
/// samalla tavalla: latent ensin, teksti aina varalla.
pub trait LatentChannel {
    /// Lähettäjän malli, jolla `LatentVector`-piilotilat tuotetaan.
    fn sender_model(&self) -> &str;

    /// Hakee [`RecursiveLink`]-sillan lähettäjän mallista annettuun
    /// kohde-malliin, jos sellainen on määritelty.
    ///
    /// `None` tarkoittaa ettei siltaa ole → siirto putoaa tekstiin
    /// ([`FallbackReason::NoLink`]).
    fn link_to(&self, target_model: &str) -> Option<RecursiveLink>;

    /// Toimittaa valmiin [`Transmission`]-tuloksen vastaanottajalle.
    ///
    /// Tämä on ainoa metodi joka koskettaa todellista kuljetusta. Se saa
    /// palauttaa virheen **vain** kuljetusviasta (yhteys katkesi), ei
    /// semanttisesta yhteensopimattomuudesta — fallback hoidetaan jo
    /// [`transmit`](LatentChannel::transmit)-tasolla.
    ///
    /// # Errors
    /// Palauttaa virheen vain todellisesta kuljetusviasta.
    fn deliver(&mut self, transmission: &Transmission) -> crate::Result<()>;

    /// Lähettää viestin vastaanottajalle valiten korkeimman mahdollisen
    /// siirtomuodon ja palaten tarvittaessa tekstiin.
    ///
    /// Algoritmi:
    /// 1. Jos vastaanottaja ei hyväksy latentia → teksti
    ///    ([`FallbackReason::ReceiverTextOnly`]).
    /// 2. Jos viestissä ei ole piilotilaa → teksti
    ///    ([`FallbackReason::NoLatentAvailable`]).
    /// 3. Jos lähettäjältä ei ole siltaa kohde-malliin → teksti
    ///    ([`FallbackReason::NoLink`]).
    /// 4. Jos projektio epäonnistuu (malli-/dimensio-/NaN-virhe) → teksti
    ///    ([`FallbackReason::ProjectionFailed`]).
    /// 5. Muutoin latent: projisoi ja toimita.
    ///
    /// Lopuksi tulos toimitetaan [`deliver`](LatentChannel::deliver)-metodilla.
    ///
    /// # Errors
    /// Palauttaa virheen vain jos [`deliver`](LatentChannel::deliver)
    /// epäonnistuu kuljetustasolla. Yhteensopimattomuus **ei** ole virhe —
    /// se johtaa teksti-fallbackiin.
    fn transmit(
        &mut self,
        message: &LatentMessage,
        receiver: &ReceiverProfile,
    ) -> crate::Result<Transmission> {
        let result = self.plan(message, receiver);
        self.deliver(&result)?;
        Ok(result)
    }

    /// Päättää siirtomuodon **suorittamatta** kuljetusta.
    ///
    /// Eriytetty [`transmit`](LatentChannel::transmit)-metodista jotta
    /// fallback-logiikkaa voi testata ja tarkastella ilman sivuvaikutuksia.
    /// Oletustoteutusta ei yleensä tarvitse korvata.
    fn plan(&self, message: &LatentMessage, receiver: &ReceiverProfile) -> Transmission {
        let text = message.text.clone();

        // 1. Vastaanottaja ei tue latenttia.
        if !receiver.accepts_latent {
            return Transmission::text(FallbackReason::ReceiverTextOnly, text);
        }

        // 2. Viestissä ei ole piilotilaa.
        let Some(latent) = &message.latent else {
            return Transmission::text(FallbackReason::NoLatentAvailable, text);
        };

        // 3. Ei siltaa kohde-malliin.
        let Some(link) = self.link_to(&receiver.model_id) else {
            return Transmission::text(FallbackReason::NoLink, text);
        };

        // Varmistetaan että silta osuu vastaanottajan odottamaan kokoon.
        // Jos linkin kohde-dimensio ei vastaa vastaanottajaa, projektio
        // antaisi väärän kokoisen vektorin → kohdellaan kuin ei siltaa.
        if link.target_dims() != receiver.dims {
            return Transmission::text(FallbackReason::NoLink, text);
        }

        // 4. Projektio. Mikä tahansa virhe → teksti-fallback (ei koskaan
        // propagoi virhettä ylös).
        match link.project(latent) {
            Ok(projected) => Transmission::latent(projected, text),
            Err(_) => Transmission::text(FallbackReason::ProjectionFailed, text),
        }
    }
}

/// In-memory-testikanava: kerää toimitetut siirrot muistiin ja sallii
/// siltojen rekisteröinnin kohde-malleille.
///
/// Tämä on tarkoitettu testaukseen ja paikalliseen kehitykseen — se ei tee
/// oikeaa verkkokuljetusta. Tuotantokanavat (bus, verkko) toteuttavat
/// [`LatentChannel`]-traitin omalla [`deliver`](LatentChannel::deliver)-
/// logiikallaan mutta perivät saman fallback-käyttäytymisen.
#[derive(Debug, Default)]
pub struct InMemoryLatentChannel {
    sender_model: String,
    links: Vec<RecursiveLink>,
    delivered: Vec<Transmission>,
    /// Jos `true`, [`deliver`](LatentChannel::deliver) simuloi kuljetusvian.
    fail_delivery: bool,
}

impl InMemoryLatentChannel {
    /// Luo kanavan annetulla lähettäjä-mallilla.
    #[must_use]
    pub fn new(sender_model: impl Into<String>) -> Self {
        Self {
            sender_model: sender_model.into(),
            links: Vec::new(),
            delivered: Vec::new(),
            fail_delivery: false,
        }
    }

    /// Rekisteröi sillan kohde-malliin. Palauttaa `self`in ketjutusta varten.
    #[must_use]
    pub fn with_link(mut self, link: RecursiveLink) -> Self {
        self.links.push(link);
        self
    }

    /// Asettaa kanavan simuloimaan kuljetusvikaa toimituksessa (testit).
    #[must_use]
    pub fn failing_delivery(mut self) -> Self {
        self.fail_delivery = true;
        self
    }

    /// Tähän mennessä toimitetut siirrot (testitarkistuksia varten).
    #[must_use]
    pub fn delivered(&self) -> &[Transmission] {
        &self.delivered
    }
}

impl LatentChannel for InMemoryLatentChannel {
    fn sender_model(&self) -> &str {
        &self.sender_model
    }

    fn link_to(&self, target_model: &str) -> Option<RecursiveLink> {
        self.links
            .iter()
            .find(|l| l.target_model() == target_model)
            .cloned()
    }

    fn deliver(&mut self, transmission: &Transmission) -> crate::Result<()> {
        if self.fail_delivery {
            return Err(crate::FamilyClawError::bus("simulated transport failure"));
        }
        self.delivered.push(transmission.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latent_vec(dims: Vec<f32>) -> LatentVector {
        LatentVector::new(dims, "agent_a/v1")
    }

    fn channel_with_link(target_model: &str, src: usize, tgt: usize) -> InMemoryLatentChannel {
        InMemoryLatentChannel::new("agent_a/v1").with_link(RecursiveLink::new(
            "agent_a/v1",
            src,
            target_model,
            tgt,
        ))
    }

    #[test]
    fn transmits_latent_when_compatible() {
        let mut ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "hello");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("transmit ok");
        assert_eq!(t.mode, TransmissionMode::Latent);
        assert!(t.mode.is_latent());
        assert!(t.fallback_reason.is_none());
        let projected = t.projected.expect("latent carries projection");
        assert_eq!(projected.vector.model_id, "agent_b/v1");
        assert_eq!(projected.vector.dims, vec![1.0, 2.0, 3.0]);
        // Teksti on aina mukana turvaverkkona myös latent-tilassa.
        assert_eq!(t.text, "hello");
        assert_eq!(ch.delivered().len(), 1);
    }

    #[test]
    fn latent_bridges_differing_dimensions() {
        // Dimensio-silta-testi: 2-ulotteinen lähde → 4-ulotteinen kohde.
        let mut ch = channel_with_link("agent_b/v1", 2, 4);
        let msg = LatentMessage::with_latent(latent_vec(vec![9.0, 8.0]), "bridge");
        let rx = ReceiverProfile::latent("agent_b/v1", 4);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Latent);
        let projected = t.projected.expect("has projection");
        assert_eq!(projected.vector.dims, vec![9.0, 8.0, 0.0, 0.0]);
        assert_eq!(projected.target_dims, 4);
    }

    #[test]
    fn falls_back_to_text_when_receiver_text_only() {
        let mut ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "hi");
        let rx = ReceiverProfile::text_only("agent_b/v1");

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert!(t.mode.is_text());
        assert_eq!(t.fallback_reason, Some(FallbackReason::ReceiverTextOnly));
        assert!(t.projected.is_none());
        assert_eq!(t.text, "hi");
    }

    #[test]
    fn falls_back_to_text_when_no_latent_in_message() {
        let mut ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::text_only("just text");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::NoLatentAvailable));
        assert_eq!(t.text, "just text");
    }

    #[test]
    fn falls_back_to_text_when_no_link_to_target() {
        // Kanavalla on silta agent_b:hen, mutta vastaanottaja on agent_c.
        let mut ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "x");
        let rx = ReceiverProfile::latent("agent_c/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::NoLink));
    }

    #[test]
    fn falls_back_when_link_target_dims_mismatch_receiver() {
        // Silta tuottaa 4-ulotteisen, mutta vastaanottaja odottaa 3:a.
        let mut ch = channel_with_link("agent_b/v1", 2, 4);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0]), "x");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::NoLink));
    }

    #[test]
    fn falls_back_when_projection_fails_on_nan() {
        // Silta on olemassa ja dimensiot täsmäävät, mutta vektori on epäterve.
        let mut ch = channel_with_link("agent_b/v1", 2, 2);
        let bad = LatentVector::new(vec![1.0, f32::NAN], "agent_a/v1");
        let msg = LatentMessage::with_latent(bad, "fallback me");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));
        assert_eq!(t.text, "fallback me");
    }

    #[test]
    fn falls_back_when_vector_model_does_not_match_link_source() {
        // Sillan lähde on agent_a, mutta vektori väittää olevansa agent_z.
        let mut ch = channel_with_link("agent_b/v1", 2, 2);
        let mismatched = LatentVector::new(vec![1.0, 2.0], "agent_z/v1");
        let msg = LatentMessage::with_latent(mismatched, "txt");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));
    }

    #[test]
    fn deliver_transport_failure_propagates() {
        let mut ch = channel_with_link("agent_b/v1", 3, 3).failing_delivery();
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "x");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let err = ch.transmit(&msg, &rx).expect_err("transport must fail");
        assert!(matches!(err, crate::FamilyClawError::Bus(_)));
        // Mitään ei toimitettu.
        assert_eq!(ch.delivered().len(), 0);
    }

    #[test]
    fn plan_has_no_side_effects() {
        let ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "x");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let planned = ch.plan(&msg, &rx);
        assert_eq!(planned.mode, TransmissionMode::Latent);
        // plan() ei toimita mitään.
        assert_eq!(ch.delivered().len(), 0);
    }

    #[test]
    fn transmission_mode_predicates() {
        assert!(TransmissionMode::Latent.is_latent());
        assert!(!TransmissionMode::Latent.is_text());
        assert!(TransmissionMode::Text.is_text());
        assert!(!TransmissionMode::Text.is_latent());
    }

    #[test]
    fn fallback_reason_descriptions_are_distinct() {
        let reasons = [
            FallbackReason::ReceiverTextOnly,
            FallbackReason::NoLink,
            FallbackReason::ProjectionFailed,
            FallbackReason::NoLatentAvailable,
        ];
        for (i, a) in reasons.iter().enumerate() {
            assert!(!a.as_str().is_empty());
            for b in &reasons[i + 1..] {
                assert_ne!(a.as_str(), b.as_str());
            }
        }
    }

    #[test]
    fn transmission_serde_roundtrip() {
        let mut ch = channel_with_link("agent_b/v1", 2, 2);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0]), "round");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);
        let t = ch.transmit(&msg, &rx).expect("ok");

        let json = serde_json::to_string(&t).expect("serialize");
        let back: Transmission = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    #[test]
    fn sender_model_is_reported() {
        let ch = InMemoryLatentChannel::new("agent_a/v1");
        assert_eq!(ch.sender_model(), "agent_a/v1");
    }
}
