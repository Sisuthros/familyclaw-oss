//! Events and a lightweight publish/subscribe bus for the bridge layer.
//!
//! This module defines the [`Event`] type (kind + payload + metadata) and the
//! [`EventBus`] type, which provides fan-out delivery to multiple subscribers
//! on top of a [`tokio::sync::broadcast`] channel.
//!
//! **Important boundary:** this is an *internal, in-process* publish/subscribe
//! mechanism for the bridge layer — it is NOT the Resonance Bus / Ractor layer
//! (`familyclaw-bus`). The full external event-routing layer is wired in later
//! via an adapter; this type provides a clean Rust interface that an adapter
//! can bridge to it.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use familyclaw_core::ids::{AgentId, MessageId};
use familyclaw_core::time::{self, Timestamp};
use familyclaw_core::{FamilyClawError, Result};

/// The kind of an event.
///
/// `Custom` allows adapters and applications to define their own event types
/// without having to change the core type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// An agent was registered.
    AgentRegistered,
    /// An agent was removed from the registry.
    AgentDeregistered,
    /// A heartbeat was received from an agent.
    AgentHeartbeat,
    /// A task was created.
    TaskCreated,
    /// A task's status changed.
    TaskStatusChanged,
    /// A task was handed off from one agent to another.
    TaskHandedOff,
    /// An application-/adapter-specific event with the given name.
    Custom(String),
}

impl EventKind {
    /// Returns the kind's stable identifier as a string (suitable for
    /// logging and routing).
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

/// An event on the bridge layer.
///
/// The payload is a `serde_json::Value` so that events of different types can
/// share the same channel without a separate type for each. Adapters can
/// parse the payload into a more specific type as needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// The event's stable identifier.
    pub id: MessageId,

    /// The event's kind.
    pub kind: EventKind,

    /// The event's source agent, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AgentId>,

    /// Payload (free-form JSON).
    #[serde(default)]
    pub payload: serde_json::Value,

    /// The event's creation time (UTC).
    pub created_at: Timestamp,
}

impl Event {
    /// Builds an event with an empty (`null`) payload.
    pub fn new(kind: EventKind, source: Option<AgentId>) -> Self {
        Self {
            id: MessageId::new(),
            kind,
            source,
            payload: serde_json::Value::Null,
            created_at: time::now(),
        }
    }

    /// Builds an event from a serde-serializable payload.
    ///
    /// # Errors
    /// [`FamilyClawError::Serde`] if serializing the payload fails.
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

    /// Sets the raw JSON payload (builder style).
    #[must_use]
    pub fn payload_value(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

/// Default capacity for the event channel (number of buffered events per
/// subscriber before the slowest subscriber starts dropping the oldest ones).
const DEFAULT_BUS_CAPACITY: usize = 256;

/// An in-process publish/subscribe bus for [`Event`]s.
///
/// Built on top of a [`tokio::sync::broadcast`] channel: every subscriber
/// receives a copy of every published event (fan-out). If a subscriber falls
/// too far behind, the oldest events are dropped for it
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
    /// Creates a bus with the default capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUS_CAPACITY)
    }

    /// Creates a bus with the given capacity.
    ///
    /// The capacity is normalized to at least one, since
    /// [`broadcast::channel`] does not allow zero capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _rx) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Number of subscribers (active receivers).
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Creates a new subscriber. A subscriber only receives events published
    /// *after* it subscribes.
    #[must_use]
    pub fn subscribe(&self) -> EventSubscriber {
        EventSubscriber {
            receiver: self.sender.subscribe(),
        }
    }

    /// Publishes an event to all subscribers. Returns how many subscribers
    /// received the event.
    ///
    /// If there are no subscribers, the event is silently dropped without an
    /// error — publishing is "fire-and-forget".
    pub fn publish(&self, event: Event) -> usize {
        self.sender.send(event).unwrap_or(0)
    }
}

/// A subscriber to the event bus.
///
/// Wraps a [`broadcast::Receiver`] and provides an awaitable [`recv`] method.
///
/// [`recv`]: EventSubscriber::recv
#[derive(Debug)]
pub struct EventSubscriber {
    receiver: broadcast::Receiver<Event>,
}

impl EventSubscriber {
    /// Waits for the next event.
    ///
    /// # Errors
    /// - [`FamilyClawError::Bus`] if the bus is closed (all senders dropped).
    /// - [`FamilyClawError::Bus`] if the subscriber fell behind and events
    ///   were dropped (the message includes the number dropped).
    pub async fn recv(&mut self) -> Result<Event> {
        match self.receiver.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Closed) => {
                Err(FamilyClawError::bus("event bus closed"))
            }
            Err(broadcast::error::RecvError::Lagged(n)) => Err(FamilyClawError::bus(format!(
                "event bus lagged by {n} events"
            ))),
        }
    }

    /// Tries to receive an event without blocking.
    ///
    /// Returns `Ok(None)` if no event is currently available.
    ///
    /// # Errors
    /// - [`FamilyClawError::Bus`] if the bus is closed.
    /// - [`FamilyClawError::Bus`] if the subscriber fell behind.
    pub fn try_recv(&mut self) -> Result<Option<Event>> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(broadcast::error::TryRecvError::Closed) => {
                Err(FamilyClawError::bus("event bus closed"))
            }
            Err(broadcast::error::TryRecvError::Lagged(n)) => Err(FamilyClawError::bus(format!(
                "event bus lagged by {n} events"
            ))),
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
        assert_eq!(EventKind::Custom("my_event".into()).as_label(), "my_event");
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
        // No panic; the bus works.
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
        // Fill past capacity → the oldest events are dropped for this subscriber.
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
