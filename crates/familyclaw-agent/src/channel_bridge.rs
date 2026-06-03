//! Kanava ↔ Resonance Bus -adapteri (design §2.2 / §3).
//!
//! Tämä moduuli on **puuttunut liitossauma** kahden §3:n puolikkaan välillä:
//! `familyclaw-channels` tuottaa alkuperätietoisia [`InboundEnvelope`]-
//! kirjekuoria, ja `familyclaw-bus` kuluttaa [`BusMessage`]-hyötykuormia.
//! Nämä ovat **eri tyyppejä eri crateissa** (nimitörmäys on tarkoituksella
//! purettu: kanavakerroksen kirjekuori on `InboundEnvelope`, busin sisältö on
//! `BusMessage`). Adapteri elää agent-kerroksessa, koska se on ainoa crate joka
//! riippuu **molemmista** — näin kanavakerros pysyy bus-riippumattomana eikä
//! crate-sykliä synny.
//!
//! ## Mitä adapteri tarjoaa
//! - [`envelope_to_bus_message`] — kanonisoidun kirjekuoren sisältö muunnetaan
//!   busin tekstihyötykuormaksi. (Vapaa funktio eikä `From`-toteutus, koska
//!   orphan-sääntö estää `impl From`:n kahdelle vieraalle tyypille.)
//! - [`publish_envelope`] — julkaisee yhden kirjekuoren busiin annetun olennon
//!   ([`BeingId`]) nimissä.
//! - [`pump_channel_to_bus`] — kuluttaa kanavan koko saapuvan virran ja syöttää
//!   sen busiin. Tämä on se konkreettinen `pump_to`-sulkeuma, joka §3:ssä
//!   luvattiin mutta jota ei aiemmin ollut olemassa.
//!
//! ## OSS-raja (KERROS A)
//! Geneeristä alustakoodia: ei kovakoodattuja olentonimiä, avaimia eikä
//! polkuja. Lähettävän olennon [`BeingId`] annetaan aina ajonaikaisesti.

use familyclaw_bus::{BeingId, BusHandle, BusMessage};
use familyclaw_channels::{pump_to, InboundEnvelope, MessageStream};

use crate::{FamilyClawError, Result};

/// Muuntaa kanavakerroksen [`InboundEnvelope`]-kirjekuoren busin
/// [`BusMessage`]-hyötykuormaksi.
///
/// Kirjekuoren tekstisisältö (`body`) muuttuu [`BusMessage::Text`]:ksi — se on
/// muoto, jonka busin olennot (agentit) käsittelevät vuoroina. Kirjekuoren
/// alkuperätiedot (kanava, lähettäjä, keskustelu) eivät mahdu busin
/// hyötykuormaan; lähettäjä välitetään erikseen julkaistaessa olennon
/// [`BeingId`]:nä (ks. [`publish_envelope`]).
///
/// Tämä on vapaa funktio eikä `impl From<InboundEnvelope> for BusMessage`,
/// koska molemmat tyypit ovat *vieraita* tälle cratelle (orphan-sääntö
/// kieltäisi `From`-toteutuksen täällä, ja kummankaan vieraan craten ei haluta
/// riippuvan toisesta vain muunnoksen takia).
#[must_use]
pub fn envelope_to_bus_message(envelope: InboundEnvelope) -> BusMessage {
    BusMessage::text(envelope.body)
}

/// Julkaisee yhden kanavakirjekuoren Resonance Busiin annetun olennon nimissä.
///
/// `from` on se olento ([`BeingId`]), jonka *postilaatikkona* kanava toimii —
/// esim. kanavan oma bus-seat. Näin saapuva ulkomaailman liikenne saa
/// busin sisällä yksikäsitteisen lähettäjäidentiteetin, jonka muut olennot
/// näkevät [`familyclaw_bus::ResonanceMessage::from`]-kentässä.
///
/// # Errors
/// [`FamilyClawError::Bus`] jos busiin julkaisu epäonnistuu.
pub fn publish_envelope(bus: &BusHandle, from: BeingId, envelope: InboundEnvelope) -> Result<()> {
    bus.publish(from, envelope_to_bus_message(envelope))
}

/// Pumppaa kanavan koko saapuvan virran Resonance Busiin.
///
/// Kuluttaa [`MessageStream`]-virran loppuun ja julkaisee jokaisen
/// kirjekuoren busiin olennon `from` nimissä. Palaa kun virta sulkeutuu tai
/// kun julkaisu epäonnistuu (virhe propagoidaan). Palautusarvona on busiin
/// syötettyjen viestien määrä.
///
/// Tämä on §3:n "yksi kanava syöttää busia" -hyväksynnän konkreettinen
/// toteutus: kanava → [`pump_to`] → adapter → `bus.publish`.
///
/// # Errors
/// [`FamilyClawError::Bus`] jos virran pumppaus tai julkaisu busiin
/// epäonnistuu.
pub async fn pump_channel_to_bus(
    stream: MessageStream,
    bus: BusHandle,
    from: BeingId,
) -> Result<usize> {
    pump_to(stream, move |envelope| {
        // Käännä bus-virhe kanava-craten virhetyypiksi, jonka `pump_to` odottaa
        // — `pump_channel_to_bus` itse palauttaa sen edelleen `FamilyClawError`:nä
        // (`ChannelError: From -> FamilyClawError::Bus`).
        publish_envelope(&bus, from, envelope)
            .map_err(|e| familyclaw_channels::ChannelError::backend("bus", e.to_string()))
    })
    .await
    .map_err(FamilyClawError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_bus::ResonanceBus;
    use familyclaw_channels::{ChannelKind, InboundMessage};

    /// Apuri: rakentaa kanonisoidun kirjekuoren testidatasta.
    fn envelope(body: &str) -> InboundEnvelope {
        InboundMessage::new("user-1", "general", body)
            .expect("valid inbound")
            .into_envelope(ChannelKind::Mock, "mock-1")
    }

    #[test]
    fn envelope_converts_to_text_bus_message() {
        let bus_msg = envelope_to_bus_message(envelope("hello bus"));
        match bus_msg {
            BusMessage::Text { body } => assert_eq!(body, "hello bus"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn publish_envelope_reaches_a_registered_being() {
        use familyclaw_bus::{BeingInfo, CollectorBeing};
        use ractor::Actor;

        let bus = ResonanceBus::start(None).await.expect("bus");

        // Vastaanottava olento, joka kerää saamansa viestit lokiin.
        let log = CollectorBeing::new_log();
        let (inbox, _h) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn");
        let receiver = BeingId::new();
        bus.register(BeingInfo::new(receiver, "agent_b", inbox))
            .expect("register");

        // Kanavan oma bus-seat (lähettäjä).
        let channel_seat = BeingId::new();
        publish_envelope(&bus, channel_seat, envelope("from the channel"))
            .expect("publish");

        // Anna viestin toimittua.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let collected = log.lock().expect("lock");
        assert_eq!(collected.len(), 1, "olennon olisi pitänyt saada viesti");
        assert_eq!(collected[0].from, channel_seat, "lähettäjä = kanavan seat");
        match &collected[0].payload {
            BusMessage::Text { body } => assert_eq!(body, "from the channel"),
            other => panic!("expected Text, got {other:?}"),
        }
        drop(collected);
        bus.stop();
    }

    #[tokio::test]
    async fn pump_channel_to_bus_drives_a_real_channel_into_a_real_bus() {
        use familyclaw_bus::{BeingInfo, CollectorBeing};
        use familyclaw_channels::{Channel, MockChannel};
        use ractor::Actor;

        let bus = ResonanceBus::start(None).await.expect("bus");

        // Vastaanottava olento busissa.
        let log = CollectorBeing::new_log();
        let (inbox, _h) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn");
        let receiver = BeingId::new();
        bus.register(BeingInfo::new(receiver, "agent_b", inbox))
            .expect("register");

        // OIKEA familyclaw-channels-kanava (ei yksityistä duplikaattia).
        let channel = MockChannel::new("mock-feed").expect("channel");
        let stream = channel.receive().expect("stream");

        // Syötä kolme viestiä ja sulje virta, jotta pump päättyy.
        for i in 0..3 {
            channel
                .inject(InboundMessage::new("u", "c", format!("msg{i}")).expect("inbound"))
                .expect("inject");
        }
        channel.close_inbound();

        let channel_seat = BeingId::new();
        let pumped = pump_channel_to_bus(stream, bus.clone(), channel_seat)
            .await
            .expect("pump");
        assert_eq!(pumped, 3, "kolme viestiä pumpattiin busiin");

        // Anna toimituksen valmistua.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        let collected = log.lock().expect("lock");
        assert_eq!(collected.len(), 3, "vastaanottaja sai kaikki kolme");
        assert!(collected.iter().all(|m| m.from == channel_seat));
        match &collected[0].payload {
            BusMessage::Text { body } => assert_eq!(body, "msg0"),
            other => panic!("expected Text, got {other:?}"),
        }
        drop(collected);
        bus.stop();
    }
}
