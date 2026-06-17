//! `LatentChannel`-toteutus Resonance Busille.
//!
//! Tämä moduuli tarjoaa [`BusLatentChannel`], joka toteuttaa [`LatentChannel`]-traitin
//! [`BusHandle`]-tyypille. Se mahdollistaa latent-telepatian sisaruksien välillä
//! käyttämällä Resonance Bus -infrastruktuuria.
//!
//! ## Translate-on-send (P4)
//! Oletuksena (kun [`BusLatentChannel::new`] luo kanavan) lähetyspolku tekee
//! pelkän [`RecursiveLink`]-dimensiosovituksen (pad/truncate/resize) — sama
//! käyttäytyminen kuin ennen. Jos kanavalle annetaan
//! [`VectorTranslator`]
//! ([`with_translator`](BusLatentChannel::with_translator)), lähtevä vektori
//! *käännetään* vastaanottajan avaruuteen ennen toimitusta:
//!
//! 1. Linkki- ja dimensiotarkistukset menevät läpi kuten oletuksessa.
//! 2. Kääntäjä [`translate`](familyclaw_latent::translate::VectorTranslator::translate)
//!    sovittaa vektorin vastaanottajan kokoon.
//! 3. Jos käännös on **häviöllinen**
//!    ([`fallback_reason`](familyclaw_latent::translate::VectorTranslator::fallback_reason)
//!    palauttaa `Some`), siirto putoaa **tekstiin**
//!    ([`FallbackReason::ProjectionFailed`]) — [`deliver`](LatentChannel::deliver)
//!    lähettää tällöin [`BusMessage::text`]:n (säilyttää "NaN → teksti"
//!    -takuun, koska ei-äärelliset arvot tekevät käännöksestä häviöllisen).
//!
//! Vastaanotto-/dekoodauspuoli (`agent.rs`) on tarkoituksella koskematta —
//! se on perherajan taakse lykätty rajapinta.
//!
//! [`LatentChannel`]: familyclaw_latent::channel::LatentChannel
//! [`BusHandle`]: crate::bus::BusHandle

use familyclaw_latent::{
    channel::{FallbackReason, LatentChannel, Transmission, TransmissionMode},
    link::RecursiveLink,
    translate::VectorTranslator,
};

use crate::{
    bus::BusHandle,
    message::{BeingId, BusMessage},
};

/// [`LatentChannel`]-toteutus Resonance Busille.
///
/// Käyttää [`BusHandle`]:a lähettääkseen [`LatentMessage`]:n toiselle olennolle.
/// Tämä mahdollistaa latent-telepatian sisaruksien välillä.
///
/// [`LatentMessage`]: familyclaw_latent::channel::LatentMessage
pub struct BusLatentChannel {
    /// Kanavan käyttäjän tunniste.
    being_id: BeingId,
    /// Lähettäjän mallin tunniste.
    sender_model: String,
    /// Määritellyt siltaukset muihin malleihin.
    links: Vec<RecursiveLink>,
    /// Valinnainen cross-model-kääntäjä lähetyspolulle. `None` =
    /// vain [`RecursiveLink`]-dimensiosovitus (oletus, taaksepäin-yhteensopiva).
    translator: Option<VectorTranslator>,
    /// Viite busiin viestien lähettämistä varten.
    bus: BusHandle,
}

impl BusLatentChannel {
    /// Luo uuden [`BusLatentChannel`]-instanssin.
    ///
    /// Lähetyspolku käyttää oletuksena pelkkää [`RecursiveLink`]-dimensiosovitusta
    /// (pad/truncate/resize). Lisää [`with_translator`](Self::with_translator):lla
    /// cross-model-käännös.
    ///
    /// # Arguments
    /// * `being_id` - Kanavan käyttäjän (olennon) tunniste.
    /// * `sender_model` - Lähettäjän mallin tunniste (esim. `agent_a/v1`).
    /// * `bus` - [`BusHandle`] jota käytetään viestien lähettämiseen.
    pub fn new(being_id: BeingId, sender_model: String, bus: BusHandle) -> Self {
        Self {
            being_id,
            sender_model,
            links: Vec::new(), // Alustetaan tyhjä. Linkit lisätään erikseen.
            translator: None,  // Oletus: ei käännöstä — vain dimensiosovitus.
            bus,
        }
    }

    /// Asettaa lähetyspolulle [`VectorTranslator`]:n ja palauttaa `self`in
    /// ketjutusta varten.
    ///
    /// Kun kääntäjä on annettu, [`plan`](LatentChannel::plan) sovittaa lähtevän
    /// vektorin vastaanottajan avaruuteen kääntäjällä linkki- ja
    /// dimensiotarkistusten jälkeen. Häviöllinen käännös → teksti-fallback
    /// ([`FallbackReason::ProjectionFailed`]). Olemassa olevat [`new`](Self::new)
    /// -kutsujat säilyttävät pad/truncate-käyttäytymisen (ei rikkova muutos).
    #[must_use]
    pub fn with_translator(mut self, translator: VectorTranslator) -> Self {
        self.translator = Some(translator);
        self
    }

    /// Lisää uuden [`RecursiveLink`]:n kanavalle.
    ///
    /// Tätä käytetään määrittämään, miten piilotila voidaan muuntaa toisen mallin vastaanottamaan muotoon.
    pub fn add_link(&mut self, link: RecursiveLink) {
        self.links.push(link);
    }

    /// Rakentaa onnistuneen latent-siirron tuloksen.
    ///
    /// [`Transmission`]:n kenttäkonstruktorit ovat craten sisäisiä, joten
    /// rakennamme tuloksen julkisista kentistä (`projected` = `Some`,
    /// `fallback_reason` = `None`).
    fn latent_transmission(
        projected: familyclaw_latent::link::ProjectedLatent,
        text: String,
    ) -> Transmission {
        Transmission {
            mode: TransmissionMode::Latent,
            text,
            projected: Some(projected),
            fallback_reason: None,
        }
    }

    /// Rakentaa teksti-fallback-tuloksen annetulla syyllä.
    fn text_transmission(reason: FallbackReason, text: String) -> Transmission {
        Transmission {
            mode: TransmissionMode::Text,
            text,
            projected: None,
            fallback_reason: Some(reason),
        }
    }
}

impl LatentChannel for BusLatentChannel {
    fn sender_model(&self) -> &str {
        &self.sender_model
    }

    fn link_to(&self, target_model: &str) -> Option<RecursiveLink> {
        // Etsitään ensimmäinen linkki, joka vastaa kohdemallia.
        self.links
            .iter()
            .find(|link| link.target_model() == target_model)
            .cloned()
    }

    /// Lähetyspolun [`plan`](LatentChannel::plan)-yliajo.
    ///
    /// Jos kanavalla ei ole kääntäjää, käyttäytyminen vastaa trait-oletusta
    /// (pelkkä [`RecursiveLink`]-dimensiosovitus). Jos kääntäjä on annettu,
    /// lähtevä vektori käännetään vastaanottajan avaruuteen linkki- ja
    /// dimensiotarkistusten jälkeen; häviöllinen käännös → teksti-fallback.
    ///
    /// Fallback-järjestys (sama kuin oletuksessa):
    /// 1. Vastaanottaja ei tue latenttia → teksti ([`FallbackReason::ReceiverTextOnly`]).
    /// 2. Viestissä ei ole piilotilaa → teksti ([`FallbackReason::NoLatentAvailable`]).
    /// 3. Ei siltaa kohde-malliin → teksti ([`FallbackReason::NoLink`]).
    /// 4. Sillan kohde-dimensio ≠ vastaanottajan dimensio → teksti ([`FallbackReason::NoLink`]).
    /// 5. Kääntäjä annettu → käännä; häviöllinen → teksti ([`FallbackReason::ProjectionFailed`]), muutoin latent.
    /// 6. Ei kääntäjää → projisoi linkillä; virhe → teksti ([`FallbackReason::ProjectionFailed`]), muutoin latent.
    fn plan(
        &self,
        message: &familyclaw_latent::channel::LatentMessage,
        receiver: &familyclaw_latent::channel::ReceiverProfile,
    ) -> Transmission {
        let text = message.text.clone();

        // 1. Vastaanottaja ei tue latenttia.
        if !receiver.accepts_latent {
            return Self::text_transmission(FallbackReason::ReceiverTextOnly, text);
        }

        // 2. Viestissä ei ole piilotilaa.
        let Some(latent) = &message.latent else {
            return Self::text_transmission(FallbackReason::NoLatentAvailable, text);
        };

        // 3. Ei siltaa kohde-malliin.
        let Some(link) = self.link_to(&receiver.model_id) else {
            return Self::text_transmission(FallbackReason::NoLink, text);
        };

        // 4. Sillan kohde-dimensio ei vastaa vastaanottajaa → kuin ei siltaa.
        if link.target_dims() != receiver.dims {
            return Self::text_transmission(FallbackReason::NoLink, text);
        }

        // 5. Projektio / käännös.
        match &self.translator {
            // 5a. Cross-model-käännös: aina ProjectedLatent, häviöllisyys ratkaisee.
            Some(translator) => {
                let projected = translator.translate(latent, receiver);
                match VectorTranslator::fallback_reason(&projected) {
                    Some(reason) => Self::text_transmission(reason, text),
                    None => Self::latent_transmission(projected, text),
                }
            }
            // 5b. Oletus: pelkkä dimensiosovitus linkillä. Virhe → teksti.
            None => match link.project(latent) {
                Ok(projected) => Self::latent_transmission(projected, text),
                Err(_) => Self::text_transmission(FallbackReason::ProjectionFailed, text),
            },
        }
    }

    fn deliver(&mut self, transmission: &Transmission) -> familyclaw_latent::Result<()> {
        // Muunnetaan `Transmission` `BusMessage`ksi ja lähetetään busin kautta.
        let bus_message = if transmission.mode.is_latent() {
            // Käytetään latenttia, jos se on saatavilla.
            if let Some(projected) = &transmission.projected {
                BusMessage::latent(
                    projected.vector.clone(),  // Käytetään projisoidun mallin latenttia
                    transmission.text.clone(), // Tekstivarjo aina mukana
                )
            } else {
                // Tämä ei pitäisi tapahtua, koska mode on Latent.
                return Err(familyclaw_latent::FamilyClawError::bus(
                    "Internal error: Latent mode but missing projected data",
                ));
            }
        } else {
            // Fallback tekstiin.
            BusMessage::text(transmission.text.clone())
        };

        // Lähetä viesti busin kautta.
        self.bus.publish(self.being_id, bus_message).map_err(|e| {
            familyclaw_latent::FamilyClawError::bus(format!("Failed to deliver via bus: {e}"))
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::being::{BeingInfo, CollectedLog, CollectorBeing};
    use crate::bus::ResonanceBus;
    use crate::message::ResonanceMessage;
    use familyclaw_latent::channel::{LatentMessage, ReceiverProfile};
    use familyclaw_latent::vector::LatentVector;
    use ractor::{Actor, ActorRef};
    use std::time::Duration as StdDuration;

    fn latent_vec(dims: Vec<f32>) -> LatentVector {
        LatentVector::new(dims, "agent_a/v1")
    }

    // Rakentaa kanavan OMALLA ResonanceBusilla (ei jaettua, ei serial_test).
    // plan on &self-metodi joka ei kosketa busia, mutta BusHandle vaaditaan
    // rakenteeseen.
    async fn channel_with(
        sender_model: &str,
        translator: Option<VectorTranslator>,
        links: Vec<RecursiveLink>,
    ) -> BusLatentChannel {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let mut ch = BusLatentChannel::new(BeingId::new(), sender_model.to_string(), bus);
        for link in links {
            ch.add_link(link);
        }
        if let Some(tr) = translator {
            ch = ch.with_translator(tr);
        }
        ch
    }

    #[tokio::test]
    async fn identity_translator_round_trips_lossless() {
        // Identiteettikääntäjä sama-malli-käännökselle → häviötön latent.
        let tr = VectorTranslator::identity("agent_a/v1", 3);
        let link = RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3);
        let ch = channel_with("agent_a/v1", Some(tr), vec![link]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "hi");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Latent);
        assert!(t.fallback_reason.is_none());
        let projected = t.projected.expect("latent carries projection");
        // Identiteetti säilyttää arvot; vektori on käännetty vastaanottajan malliin.
        assert_eq!(projected.vector.dims, vec![1.0, 2.0, 3.0]);
        assert_eq!(projected.vector.model_id, "agent_b/v1");
        assert!(projected.lossless);
        assert_eq!(t.text, "hi");

        ch.bus.stop();
    }

    #[tokio::test]
    async fn lossy_translation_gives_text_fallback() {
        // Truncate (4 → 2) on häviöllinen → teksti-fallback.
        let tr = VectorTranslator::identity("agent_a/v1", 4);
        let link = RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 2);
        let ch = channel_with("agent_a/v1", Some(tr), vec![link]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0, 4.0]), "lossy");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));
        assert!(t.projected.is_none());
        assert_eq!(t.text, "lossy");

        ch.bus.stop();
    }

    #[tokio::test]
    async fn nan_input_gives_text_fallback() {
        // Ei-äärellinen syöte tekee käännöksestä häviöllisen → teksti.
        let tr = VectorTranslator::identity("agent_a/v1", 2);
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 2);
        let ch = channel_with("agent_a/v1", Some(tr), vec![link]).await;

        let bad = LatentVector::new(vec![1.0, f32::NAN], "agent_a/v1");
        let msg = LatentMessage::with_latent(bad, "nan me");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));
        assert_eq!(t.text, "nan me");

        ch.bus.stop();
    }

    #[tokio::test]
    async fn without_translator_keeps_pad_truncate_behavior() {
        // Ilman kääntäjää: linkki tekee pad (2 → 4), häviötön latent.
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 4);
        let ch = channel_with("agent_a/v1", None, vec![link]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![9.0, 8.0]), "pad");
        let rx = ReceiverProfile::latent("agent_b/v1", 4);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Latent);
        let projected = t.projected.expect("has projection");
        assert_eq!(projected.vector.dims, vec![9.0, 8.0, 0.0, 0.0]);

        ch.bus.stop();
    }

    #[tokio::test]
    async fn without_translator_nan_still_falls_back_to_text() {
        // NaN → teksti -takuu pätee myös oletuspolulla (linkki hylkää NaN:n).
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 2);
        let ch = channel_with("agent_a/v1", None, vec![link]).await;

        let bad = LatentVector::new(vec![1.0, f32::NAN], "agent_a/v1");
        let msg = LatentMessage::with_latent(bad, "fallback me");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));

        ch.bus.stop();
    }

    #[tokio::test]
    async fn plan_falls_back_when_receiver_text_only() {
        let tr = VectorTranslator::identity("agent_a/v1", 3);
        let link = RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3);
        let ch = channel_with("agent_a/v1", Some(tr), vec![link]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "txt");
        let rx = ReceiverProfile::text_only("agent_b/v1");

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ReceiverTextOnly));

        ch.bus.stop();
    }

    #[tokio::test]
    async fn plan_falls_back_when_no_link() {
        // Kääntäjä on, mutta vastaanottajalle ei ole siltaa.
        let tr = VectorTranslator::identity("agent_a/v1", 3);
        let ch = channel_with("agent_a/v1", Some(tr), vec![]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "x");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::NoLink));

        ch.bus.stop();
    }

    /// Apuri: spawnaa keräävän olennon ja rekisteröi sen busiin.
    async fn join_being(
        bus: &BusHandle,
        name: &str,
    ) -> (BeingId, ActorRef<ResonanceMessage>, CollectedLog) {
        let log = CollectorBeing::new_log();
        let (actor, _h) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn being");
        let id = BeingId::new();
        bus.register(BeingInfo::new(id, name, actor.clone()))
            .expect("register");
        (id, actor, log)
    }

    async fn settle() {
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn transmit_lossless_delivers_latent_over_bus() {
        // Oma ResonanceBus (ei serial_test): lähettäjä + vastaanottaja.
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (_rx_id, _rx_actor, rx_log) = join_being(&bus, "agent_b").await;

        let sender_id = BeingId::new();
        let mut ch = BusLatentChannel::new(sender_id, "agent_a/v1".to_string(), bus.clone())
            .with_translator(VectorTranslator::identity("agent_a/v1", 3));
        ch.add_link(RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3));

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "telepathy");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("transmit ok");
        assert_eq!(t.mode, TransmissionMode::Latent);
        settle().await;

        let received = rx_log.lock().expect("lock");
        assert_eq!(received.len(), 1);
        match &received[0].payload {
            BusMessage::Latent {
                vector,
                text_shadow,
            } => {
                assert_eq!(vector.dims, vec![1.0, 2.0, 3.0]);
                assert_eq!(vector.model_id, "agent_b/v1");
                assert_eq!(text_shadow, "telepathy");
            }
            other => panic!("expected Latent, got {other:?}"),
        }

        bus.stop();
    }

    #[tokio::test]
    async fn transmit_lossy_delivers_text_over_bus() {
        // Häviöllinen käännös → vastaanottaja saa BusMessage::Text.
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (_rx_id, _rx_actor, rx_log) = join_being(&bus, "agent_b").await;

        let sender_id = BeingId::new();
        let mut ch = BusLatentChannel::new(sender_id, "agent_a/v1".to_string(), bus.clone())
            .with_translator(VectorTranslator::identity("agent_a/v1", 4));
        ch.add_link(RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 2));

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0, 4.0]), "shadow only");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.transmit(&msg, &rx).expect("transmit ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        settle().await;

        let received = rx_log.lock().expect("lock");
        assert_eq!(received.len(), 1);
        match &received[0].payload {
            BusMessage::Text { body } => assert_eq!(body, "shadow only"),
            other => panic!("expected Text fallback, got {other:?}"),
        }

        bus.stop();
    }
}
