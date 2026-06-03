//! Tapahtumat ja kevyt publish/subscribe-väylä siltakerrokselle.
//!
//! Tämä moduuli määrittelee [`Event`]-tyypin (laji + hyötykuorma + metatiedot)
//! ja [`EventBus`]-tyypin, joka tarjoaa fan-out-jakelun usealle tilaajalle
//! [`tokio::sync::broadcast`]-kanavan päällä.
//!
//! **Tärkeä rajaus:** tämä on *siltakerroksen* sisäinen, in-process
//! publish/subscribe — EI Resonance Bus / Ractor -kerros (`familyclaw-bus`).
//! Varsinainen affektiivinen hermosto kytketään myöhemmin adapterilla; tämä
//! tyyppi tarjoaa puhtaan Rust-rajapinnan jonka adapteri voi sillata.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use familyclaw_core::ids::{AgentId, MessageId};
use familyclaw_core::time::{self, Timestamp};
use familyclaw_core::{FamilyClawError, Result};

/// Tapahtuman laji.
///
/// `Custom` mahdollistaa adapterien ja sovellusten omat tapahtumatyypit ilman
/// että ydintyyppiä tarvitsee muuttaa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Agentti rekisteröitiin.
    AgentRegistered,
    /// Agentti poistettiin rekisteristä.
    AgentDeregistered,
    /// Agentilta saatiin heartbeat.
    AgentHeartbeat,
    /// Tehtävä luotiin.
    TaskCreated,
    /// Tehtävän tila vaihtui.
    TaskStatusChanged,
    /// Tehtävä luovutettiin agentilta toiselle.
    TaskHandedOff,
    /// Sovellus-/adapterikohtainen tapahtuma annetulla nimellä.
    Custom(String),
}

impl EventKind {
    /// Palauttaa lajin vakaan tunnisteen merkkijonona (sopii lokitukseen ja
    /// reititykseen).
    #[must_use]
    pub fn as_label(&self) -> &str {
        match self {
            EventKind::AgentRegistered => "agent_registered",
            EventKind::AgentDeregistered => "agent_deregistered",
            EventKind::AgentHeartbeat => "agent_heartbeat",
            EventKind::TaskCreated => "task_created",
            EventKind::TaskStatusChanged => "task_status_changed",
            EventKind::TaskHandedOff => "task_handed_off",
            EventKind::Custom(name) => name.as_str(),
        }
    }
}

/// Siltakerroksen tapahtuma.
///
/// Hyötykuorma on `serde_json::Value` jotta erityyppiset tapahtumat mahtuvat
/// samaan kanavaan ilman tyyppierittelyä jokaiselle. Adapterit voivat
/// jäsentää hyötykuorman tarkemmaksi tyypiksi tarpeen mukaan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Tapahtuman vakaa tunniste.
    pub id: MessageId,

    /// Tapahtuman laji.
    pub kind: EventKind,

    /// Tapahtuman lähdeagentti, jos tiedossa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AgentId>,

    /// Hyötykuorma (vapaamuotoinen JSON).
    #[serde(default)]
    pub payload: serde_json::Value,

    /// Tapahtuman syntyhetki (UTC).
    pub created_at: Timestamp,
}

impl Event {
    /// Rakentaa tapahtuman tyhjällä (`null`) hyötykuormalla.
    pub fn new(kind: EventKind, source: Option<AgentId>) -> Self {
        Self {
            id: MessageId::new(),
            kind,
            source,
            payload: serde_json::Value::Null,
            created_at: time::now(),
        }
    }

    /// Rakentaa tapahtuman serde-sarjallistuvasta hyötykuormasta.
    ///
    /// # Errors
    /// [`FamilyClawError::Serde`] jos hyötykuorman sarjallistus epäonnistuu.
    pub fn with_payload<T: Serialize>(
        kind: EventKind,
        source: Option<AgentId>,
        payload: &T,
    ) -> Result<Self> {
        let payload = serde_json::to_value(payload).map_err(FamilyClawError::from)?;
        Ok(Self {
            id: MessageId::new(),
            kind,
            source,
            payload,
            created_at: time::now(),
        })
    }

    /// Asettaa raa'an JSON-hyötykuorman (builder-tyyli).
    #[must_use]
    pub fn payload_value(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

/// Vakiokapasiteetti tapahtumakanavalle (puskuroitujen tapahtumien määrä per
/// tilaaja ennen kuin hitain tilaaja alkaa pudottaa vanhimpia).
const DEFAULT_BUS_CAPACITY: usize = 256;

/// In-process publish/subscribe-väylä [`Event`]eille.
///
/// Rakentuu [`tokio::sync::broadcast`]-kanavan päälle: jokainen tilaaja saa
/// kopion jokaisesta julkaistusta tapahtumasta (fan-out). Jos tilaaja jää
/// liian jälkeen, vanhimmat tapahtumat pudotetaan sen osalta
/// ([`broadcast::error::RecvError::Lagged`]).
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// Luo väylän vakiokapasiteetilla.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUS_CAPACITY)
    }

    /// Luo väylän annetulla kapasiteetilla.
    ///
    /// Kapasiteetti normalisoidaan vähintään yhteen, koska
    /// [`broadcast::channel`] ei salli nollakapasiteettia.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _rx) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Tilaajien (aktiivisten vastaanottajien) lukumäärä.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Luo uuden tilaajan. Tilaaja saa vain tilauksen *jälkeen* julkaistut
    /// tapahtumat.
    #[must_use]
    pub fn subscribe(&self) -> EventSubscriber {
        EventSubscriber {
            receiver: self.sender.subscribe(),
        }
    }

    /// Julkaisee tapahtuman kaikille tilaajille. Palauttaa montako tilaajaa
    /// tapahtuman vastaanotti.
    ///
    /// Jos tilaajia ei ole, tapahtuma jätetään hiljaisesti pudottamatta
    /// virhettä — julkaisu on "fire-and-forget".
    pub fn publish(&self, event: Event) -> usize {
        self.sender.send(event).unwrap_or(0)
    }
}

/// Tapahtumaväylän tilaaja.
///
/// Kääri [`broadcast::Receiver`]in ja tarjoaa odottavan [`recv`]-metodin.
///
/// [`recv`]: EventSubscriber::recv
#[derive(Debug)]
pub struct EventSubscriber {
    receiver: broadcast::Receiver<Event>,
}

impl EventSubscriber {
    /// Odottaa seuraavaa tapahtumaa.
    ///
    /// # Errors
    /// - [`FamilyClawError::Bus`] jos väylä on suljettu (kaikki lähettäjät
    ///   pudotettu).
    /// - [`FamilyClawError::Bus`] jos tilaaja jäi jälkeen ja tapahtumia
    ///   pudotettiin (viesti sisältää pudotettujen määrän).
    pub async fn recv(&mut self) -> Result<Event> {
        match self.receiver.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Closed) => {
                Err(FamilyClawError::bus("event bus closed"))
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                Err(FamilyClawError::bus(format!("event bus lagged by {n} events")))
            }
        }
    }

    /// Yrittää vastaanottaa tapahtuman estämättä.
    ///
    /// Palauttaa `Ok(None)` jos tällä hetkellä ei ole tapahtumaa saatavilla.
    ///
    /// # Errors
    /// - [`FamilyClawError::Bus`] jos väylä on suljettu.
    /// - [`FamilyClawError::Bus`] jos tilaaja jäi jälkeen.
    pub fn try_recv(&mut self) -> Result<Option<Event>> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(broadcast::error::TryRecvError::Closed) => {
                Err(FamilyClawError::bus("event bus closed"))
            }
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                Err(FamilyClawError::bus(format!("event bus lagged by {n} events")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_labels() {
        assert_eq!(EventKind::AgentRegistered.as_label(), "agent_registered");
        assert_eq!(EventKind::TaskHandedOff.as_label(), "task_handed_off");
        assert_eq!(
            EventKind::Custom("my_event".into()).as_label(),
            "my_event"
        );
    }

    #[test]
    fn event_new_has_null_payload() {
        let e = Event::new(EventKind::TaskCreated, None);
        assert_eq!(e.payload, serde_json::Value::Null);
        assert!(e.source.is_none());
    }

    #[test]
    fn event_with_payload_serializes() {
        #[derive(Serialize)]
        struct P {
            task: String,
        }
        let e = Event::with_payload(
            EventKind::TaskCreated,
            Some(AgentId::new()),
            &P { task: "t".into() },
        )
        .expect("serialize payload");
        assert_eq!(e.payload["task"], serde_json::json!("t"));
    }

    #[test]
    fn event_serde_roundtrip() {
        let e = Event::new(EventKind::AgentHeartbeat, Some(AgentId::new()))
            .payload_value(serde_json::json!({ "n": 1 }));
        let json = serde_json::to_string(&e).expect("serialize");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(e, back);
    }

    #[test]
    fn event_kind_serde_custom_roundtrip() {
        let kind = EventKind::Custom("x".into());
        let json = serde_json::to_string(&kind).expect("serialize");
        let back: EventKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(kind, back);
    }

    #[test]
    fn bus_capacity_is_normalized_to_at_least_one() {
        let bus = EventBus::with_capacity(0);
        // Ei paniikkia; väylä toimii.
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_returns_zero() {
        let bus = EventBus::new();
        let received = bus.publish(Event::new(EventKind::TaskCreated, None));
        assert_eq!(received, 0);
    }

    #[tokio::test]
    async fn single_subscriber_receives_event() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);

        let sent = Event::new(EventKind::TaskCreated, None);
        let count = bus.publish(sent.clone());
        assert_eq!(count, 1);

        let got = sub.recv().await.expect("receive");
        assert_eq!(got, sent);
    }

    #[tokio::test]
    async fn fan_out_to_multiple_subscribers() {
        let bus = EventBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);

        let sent = Event::new(EventKind::AgentRegistered, Some(AgentId::new()));
        assert_eq!(bus.publish(sent.clone()), 2);

        assert_eq!(a.recv().await.expect("a"), sent);
        assert_eq!(b.recv().await.expect("b"), sent);
    }

    #[tokio::test]
    async fn try_recv_empty_then_value() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();
        assert!(sub.try_recv().expect("empty ok").is_none());

        let sent = Event::new(EventKind::TaskStatusChanged, None);
        bus.publish(sent.clone());
        assert_eq!(sub.try_recv().expect("value ok"), Some(sent));
    }

    #[tokio::test]
    async fn subscriber_only_sees_events_after_subscribe() {
        let bus = EventBus::new();
        // Julkaistu ennen tilausta — ei tilaajia, katoaa.
        bus.publish(Event::new(EventKind::TaskCreated, None));

        let mut sub = bus.subscribe();
        let after = Event::new(EventKind::TaskHandedOff, None);
        bus.publish(after.clone());
        assert_eq!(sub.recv().await.expect("after"), after);
    }

    #[tokio::test]
    async fn recv_errors_when_bus_dropped() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();
        drop(bus);
        let err = sub.recv().await.expect_err("closed");
        assert!(matches!(err, FamilyClawError::Bus(_)));
    }

    #[tokio::test]
    async fn lagging_subscriber_reports_bus_error() {
        let bus = EventBus::with_capacity(2);
        let mut sub = bus.subscribe();
        // Täytä yli kapasiteetin → vanhimmat pudotetaan tälle tilaajalle.
        for _ in 0..5 {
            bus.publish(Event::new(EventKind::TaskCreated, None));
        }
        let err = sub.recv().await.expect_err("lagged");
        match err {
            FamilyClawError::Bus(msg) => assert!(msg.contains("lagged")),
            other => panic!("expected Bus error, got {other:?}"),
        }
    }
}
