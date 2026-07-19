//! Beings on the bus: registration data and the recipient type.
//!
//! A being attached to the bus is a **Ractor actor** whose message type is
//! [`ResonanceMessage`]. The bus keeps track of joined beings
//! ([`BeingInfo`]) and delivers messages to them via `cast` calls. The
//! crash of a single being does not bring down the bus (supervision; see
//! [`crate::bus`]).
//!
//! This module also provides a ready-made [`CollectorBeing`] actor that
//! collects the messages it receives. It is intended for tests and
//! examples — real family members (Layer B) implement their own actor
//! that reacts to sibling resonance.

use std::sync::{Arc, Mutex};

use ractor::{Actor, ActorProcessingErr, ActorRef};

use familyclaw_emotion::{EmotionState, EmotionTransition};

use crate::message::{BeingId, BusMessage, ResonanceMessage};

/// Metadata for a being registered on the bus.
///
/// Carries the being identifier, a human-readable name, and the
/// [`ActorRef`] through which the bus delivers messages. The reference is
/// typed for [`ResonanceMessage`], so beings can only receive the bus's
/// language.
#[derive(Clone)]
pub struct BeingInfo {
    /// The being's identifier.
    id: BeingId,
    /// The being's display name (generic, e.g. `"agent_a"`).
    name: String,
    /// The inbox to which messages are delivered.
    inbox: ActorRef<ResonanceMessage>,
}

impl BeingInfo {
    /// Constructs registration data.
    #[must_use]
    pub fn new(id: BeingId, name: impl Into<String>, inbox: ActorRef<ResonanceMessage>) -> Self {
        Self {
            id,
            name: name.into(),
            inbox,
        }
    }

    /// The being's identifier.
    #[must_use]
    pub const fn id(&self) -> BeingId {
        self.id
    }

    /// The being's display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The being's inbox (delivery address).
    #[must_use]
    pub fn inbox(&self) -> &ActorRef<ResonanceMessage> {
        &self.inbox
    }
}

impl std::fmt::Debug for BeingInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BeingInfo")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A lightweight, serializable snapshot of a being — [`BeingInfo`] without
/// the actor reference.
///
/// This is what [`crate::bus::BusHandle::beings`] returns: it reports
/// *who* is joined without exposing internal actor machinery. **This list
/// must NOT be empty when beings are joined** — this is a direct fix for
/// the live-3500 bus's `beings:[]` emptiness bug (design §2.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BeingSnapshot {
    /// The being's identifier.
    pub id: BeingId,
    /// The being's display name.
    pub name: String,
}

impl From<&BeingInfo> for BeingSnapshot {
    fn from(info: &BeingInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
        }
    }
}

/// A shared, thread-safe log of received messages.
///
/// [`CollectorBeing`] writes to this, and tests/examples read from it.
pub type CollectedLog = Arc<Mutex<Vec<ResonanceMessage>>>;

/// A ready-made being actor that collects the [`ResonanceMessage`]s it
/// receives into a shared log.
///
/// Intended for tests and examples. In production a family member
/// implements its own [`Actor`] that reacts to resonance (e.g. updating
/// its own emotional state based on a neighbor's pulse — affective
/// contagion).
pub struct CollectorBeing;

/// [`CollectorBeing`]'s state: the shared log messages accumulate into.
pub struct CollectorState {
    /// The log to which received messages are written.
    pub log: CollectedLog,
}

impl Actor for CollectorBeing {
    type Msg = ResonanceMessage;
    type State = CollectorState;
    type Arguments = CollectedLog;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        log: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(CollectorState { log })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // The lock can only be poisoned if another thread panicked while
        // holding it; in that case we still capture the data rather than
        // propagate the panic into this actor.
        let mut guard = match state.log.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.push(message);
        Ok(())
    }
}

impl CollectorBeing {
    /// Creates a shared log for the [`CollectorBeing`] actor.
    #[must_use]
    pub fn new_log() -> CollectedLog {
        Arc::new(Mutex::new(Vec::new()))
    }
}

/// **Affective contagion — the receiving side.**
///
/// Absorbs a sibling's incoming emotion state (`incoming`) into the being's
/// own state (`own`) using [`EmotionTransition::blend`]'s inertia:
/// `next = inertia * own + (1 - inertia) * incoming`.
///
/// This is the *missing piece* of the bus layer: the bus already **carries**
/// [`BusMessage::EmotionPulse`] to siblings, but until now nothing **absorbed**
/// the pulse into its own emotion state. High inertia → the being's own mood
/// stays stable and only shifts slightly toward the neighbor's; low inertia →
/// contagion happens quickly. The result always stays within [`EmotionState`]'s
/// bounds (`blend` clamps it).
///
/// Pure and free of side effects outside its references: the only change is
/// the in-place update of the `own` state. Also reusable without the actor
/// machinery (e.g. Layer B's own being implementation can call this directly).
pub fn on_pulse(own: &mut EmotionState, incoming: &EmotionState, transition: EmotionTransition) {
    *own = transition.blend(own, incoming);
}

/// A ready-made being actor that **reacts** to siblings' emotion pulses by
/// absorbing them into its own [`EmotionState`] ([`on_pulse`]) — affective
/// contagion in practice.
///
/// Unlike [`CollectorBeing`] (which only collects messages), this actor
/// maintains its own emotion state and moves it toward a neighbor's mood
/// whenever a [`BusMessage::EmotionPulse`] arrives from the bus. Other
/// message kinds (text, task events, …) are ignored — they do not change
/// the emotion state.
///
/// State is shared with tests/examples via the [`AffectiveState::emotion`]
/// handle (`Arc<Mutex<…>>`), so absorbed contagion can be verified from the
/// outside. In production, Layer B implements its own being; this is a
/// reusable example plus test fixture.
pub struct AffectiveBeing;

/// A being's shared, thread-safe emotion state.
///
/// [`AffectiveBeing`] mutates this; tests/examples read it.
pub type SharedEmotion = Arc<Mutex<EmotionState>>;

/// [`AffectiveBeing`]'s state: its own emotion state + contagion inertia.
pub struct AffectiveState {
    /// The being's own emotion state, shared behind a lock for observation.
    pub emotion: SharedEmotion,
    /// The inertia with which incoming pulses are absorbed into the own state.
    pub transition: EmotionTransition,
}

/// [`AffectiveBeing`]'s startup arguments: initial state + inertia.
pub struct AffectiveArgs {
    /// A shared handle to the being's initial emotion state (the same one
    /// the test observes).
    pub emotion: SharedEmotion,
    /// The contagion inertia (`0.0..=1.0`; see [`EmotionTransition`]).
    pub transition: EmotionTransition,
}

impl Actor for AffectiveBeing {
    type Msg = ResonanceMessage;
    type State = AffectiveState;
    type Arguments = AffectiveArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(AffectiveState {
            emotion: args.emotion,
            transition: args.transition,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Only an emotion pulse moves the own state; other kinds do not.
        if let BusMessage::EmotionPulse { state: incoming } = &message.payload {
            // The lock can only be poisoned if another thread panicked while
            // holding it; we still absorb the contagion rather than
            // propagate the panic into this actor.
            let mut guard = match state.emotion.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            on_pulse(&mut guard, incoming, state.transition);
        }
        Ok(())
    }
}

impl AffectiveBeing {
    /// Creates a shared emotion-state handle from the given initial state.
    #[must_use]
    pub fn shared(initial: EmotionState) -> SharedEmotion {
        Arc::new(Mutex::new(initial))
    }
}

#[cfg(test)]
mod tests {
    // Affective tests compare exact, representable f32 emotion-state values
    // (e.g. 50.0 at the midpoint) — exact comparison is correct here.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::bus::ResonanceBus;
    use crate::message::BusMessage;
    use familyclaw_emotion::Dimension;

    #[tokio::test]
    async fn collector_records_messages() {
        let log = CollectorBeing::new_log();
        let (actor, handle) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn collector");

        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("hi"));
        actor.cast(env.clone()).expect("cast to collector");

        // Let the actor process the queued message. (A stop signal can
        // overtake regular messages, so we don't rely solely on stop+join
        // ordering to verify message delivery.)
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        {
            let recorded = log.lock().expect("lock");
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0], env);
        } // release the lock before .await

        actor.stop(None);
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn being_info_exposes_fields_and_snapshot() {
        let log = CollectorBeing::new_log();
        let (actor, handle) = Actor::spawn(None, CollectorBeing, log)
            .await
            .expect("spawn");
        let id = BeingId::new();
        let info = BeingInfo::new(id, "agent_a", actor.clone());

        assert_eq!(info.id(), id);
        assert_eq!(info.name(), "agent_a");

        let snap = BeingSnapshot::from(&info);
        assert_eq!(snap.id, id);
        assert_eq!(snap.name, "agent_a");

        // Debug does not panic or leak internal actor state.
        let dbg = format!("{info:?}");
        assert!(dbg.contains("agent_a"));

        actor.stop(None);
        handle.await.expect("join");
    }

    #[test]
    fn being_snapshot_serde_roundtrip() {
        let snap = BeingSnapshot {
            id: BeingId::new(),
            name: "agent_b".into(),
        };
        let json = serde_json::to_string(&snap).expect("ser");
        let back: BeingSnapshot = serde_json::from_str(&json).expect("de");
        assert_eq!(snap, back);
    }

    // ---- Affective contagion --------------------------------------------
    //
    // CRITICAL ractor::pg rule: every test that runs against a real bus
    // spawns its OWN `ResonanceBus` instance (each gets a fresh
    // `resonance-bus-{n}` group), so parallel tests don't share a member
    // pool. No serial_test dependency needed.

    /// Helper: a short wait so asynchronous delivery has time to complete.
    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    /// Builds an emotion state where one dimension is set to the given value.
    fn state_with(dim: Dimension, value: f32) -> EmotionState {
        let mut s = EmotionState::neutral();
        s.set(dim, value);
        s
    }

    #[test]
    fn on_pulse_moves_own_state_toward_incoming() {
        // Pure helper: own state moves toward the observation per inertia.
        let mut own = state_with(Dimension::Joy, 0.0);
        let incoming = state_with(Dimension::Joy, 100.0);

        // inertia 0.5 → midpoint (0*0.5 + 100*0.5 = 50).
        on_pulse(&mut own, &incoming, EmotionTransition::new(0.5));
        assert_eq!(own.value(Dimension::Joy), 50.0);
    }

    #[test]
    fn on_pulse_full_inertia_keeps_own_state() {
        // inertia 1.0 → no contagion, own state does not change.
        let mut own = state_with(Dimension::Sadness, 70.0);
        let incoming = state_with(Dimension::Sadness, 0.0);

        on_pulse(&mut own, &incoming, EmotionTransition::new(1.0));
        assert_eq!(own.value(Dimension::Sadness), 70.0);
    }

    #[test]
    fn on_pulse_zero_inertia_absorbs_incoming_fully() {
        // inertia 0.0 → the observation fully replaces the own state.
        let mut own = state_with(Dimension::Anger, 90.0);
        let incoming = state_with(Dimension::Hope, 80.0);

        on_pulse(&mut own, &incoming, EmotionTransition::new(0.0));
        assert_eq!(own.value(Dimension::Hope), 80.0);
        assert_eq!(
            own.value(Dimension::Anger),
            0.0,
            "the observation displaces the old value"
        );
    }

    #[test]
    fn on_pulse_repeated_converges_toward_incoming() {
        // Repeating the same pulse → own state approaches the sender's state (inertia <1).
        let mut own = EmotionState::neutral();
        let incoming = state_with(Dimension::Curiosity, 90.0);
        let t = EmotionTransition::new(0.5);
        for _ in 0..20 {
            on_pulse(&mut own, &incoming, t);
        }
        assert!(
            (own.value(Dimension::Curiosity) - 90.0).abs() < 0.5,
            "repeated contagion pulls the state close to the sender's state"
        );
    }

    #[tokio::test]
    async fn affective_being_absorbs_pulse_directly() {
        // Direct cast (without a bus): the actor absorbs the pulse into its own state.
        let emotion = AffectiveBeing::shared(state_with(Dimension::Joy, 0.0));
        let (actor, handle) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: emotion.clone(),
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn affective being");

        let pulse = ResonanceMessage::new(
            BeingId::new(),
            BusMessage::emotion_pulse(state_with(Dimension::Joy, 100.0)),
        );
        actor.cast(pulse).expect("cast pulse");
        settle().await;

        {
            let got = emotion.lock().expect("lock");
            assert_eq!(
                got.value(Dimension::Joy),
                50.0,
                "own Joy moved to the midpoint"
            );
        }

        actor.stop(None);
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn affective_being_ignores_non_pulse_messages() {
        // A text message must NOT change the emotion state.
        let emotion = AffectiveBeing::shared(state_with(Dimension::Love, 42.0));
        let (actor, handle) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: emotion.clone(),
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn affective being");

        actor
            .cast(ResonanceMessage::new(
                BeingId::new(),
                BusMessage::text("just chatting"),
            ))
            .expect("cast text");
        settle().await;

        {
            let got = emotion.lock().expect("lock");
            assert_eq!(
                got.value(Dimension::Love),
                42.0,
                "a non-pulse message does not change the emotion state"
            );
        }

        actor.stop(None);
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn pulse_over_real_bus_shifts_receiver_toward_sender() {
        // This module's core test: a pulse traveling over a real bus shifts
        // the RECEIVING being's own emotion state toward the sender's.
        // OWN bus instance (fresh resonance-bus-{n} group).
        let bus = ResonanceBus::start(None).await.expect("start bus");

        // Sender: a plain collector being (only publishes the pulse).
        let sender_id = BeingId::new();
        let sender_log = CollectorBeing::new_log();
        let (sender_actor, _hs) = Actor::spawn(None, CollectorBeing, sender_log)
            .await
            .expect("spawn sender");
        bus.register(BeingInfo::new(sender_id, "agent_a", sender_actor))
            .expect("register sender");

        // Receiver: an affective being that absorbs the pulse into its own state.
        // Initial state Joy=0; sender sends Joy=100; inertia 0.5 → expect 50.
        let recv_emotion = AffectiveBeing::shared(state_with(Dimension::Joy, 0.0));
        let (recv_actor, _hr) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: recv_emotion.clone(),
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn receiver");
        bus.register(BeingInfo::new(BeingId::new(), "agent_b", recv_actor))
            .expect("register receiver");

        // agent_a leaks a high-joy pulse into the bus.
        bus.publish(
            sender_id,
            BusMessage::emotion_pulse(state_with(Dimension::Joy, 100.0)),
        )
        .expect("publish pulse");
        settle().await;

        {
            let got = recv_emotion.lock().expect("lock");
            assert_eq!(
                got.value(Dimension::Joy),
                50.0,
                "a pulse arriving over the bus shifted the receiver's state toward the sender's"
            );
        }

        bus.stop();
    }

    #[tokio::test]
    async fn sender_does_not_absorb_own_pulse_over_bus() {
        // The sender does not receive its own pulse → its own state does not change via the bus.
        // A second, SEPARATE bus instance.
        let bus = ResonanceBus::start(None).await.expect("start bus");

        let sender_id = BeingId::new();
        let sender_emotion = AffectiveBeing::shared(state_with(Dimension::Joy, 100.0));
        let (sender_actor, _hs) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: sender_emotion.clone(),
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn sender");
        bus.register(BeingInfo::new(sender_id, "agent_a", sender_actor))
            .expect("register sender");

        // A second being, so the bus has a receiver (broadcast route).
        let other_emotion = AffectiveBeing::shared(EmotionState::neutral());
        let (other_actor, _ho) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: other_emotion,
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn other");
        bus.register(BeingInfo::new(BeingId::new(), "agent_b", other_actor))
            .expect("register other");

        bus.publish(
            sender_id,
            BusMessage::emotion_pulse(state_with(Dimension::Joy, 0.0)),
        )
        .expect("publish pulse");
        settle().await;

        {
            let got = sender_emotion.lock().expect("lock");
            assert_eq!(
                got.value(Dimension::Joy),
                100.0,
                "the sender does not receive its own pulse and so its state does not change"
            );
        }

        bus.stop();
    }
}
