//! The Resonance Bus actor — the core of the affective nervous system.
//!
//! [`ResonanceBus`] is a Ractor actor that:
//! 1. **registers beings** ([`BusOp::Register`]) and removes them
//!    ([`BusOp::Deregister`]),
//! 2. **sends messages to all** other beings ([`BusOp::Publish`]) — the
//!    affective nervous system's "blood",
//! 3. **spreads emotion as a pulse** (affective contagion): when a being
//!    publishes a [`BusMessage::EmotionPulse`], all *other* beings receive
//!    it and can react to each other's mood,
//! 4. **lists joined beings** ([`BusOp::ListBeings`]) — the returned list
//!    must NOT be empty when beings have joined (fixes the live-3500
//!    `beings:[]` bug, design §2.2),
//! 5. **survives crashes** (supervision): a single being's death does not
//!    bring down the bus, it only removes that being from the registry.
//!
//! The ergonomic interface is [`BusHandle`], which wraps the raw
//! [`ActorRef`] reference into a safe (no `unwrap`) API.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ractor::pg;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, SupervisionEvent};
use tracing::{debug, warn};

use familyclaw_core::{FamilyClawError, Result};

use crate::being::{BeingInfo, BeingSnapshot};
use crate::message::{BeingId, BusMessage, ResonanceMessage};

/// Default timeout for synchronous `call` queries (e.g. the being list).
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// The bus's control protocol — messages the [`ResonanceBus`] actor handles.
///
/// This is the bus's *internal* message type (control), as opposed to the
/// [`ResonanceMessage`] traffic (payload) between beings. In most cases it's
/// better to use the [`BusHandle`] interface instead of a direct `cast`/`call`.
pub enum BusOp {
    /// Register a being on the bus. If the same identifier already exists,
    /// its data is replaced (re-registration).
    Register(BeingInfo),

    /// Remove a being from the bus by identifier.
    Deregister(BeingId),

    /// Publish a message: the envelope is delivered to all beings **other**
    /// than the sender. An emotion pulse spreads via the same route
    /// (affective contagion).
    Publish(ResonanceMessage),

    /// Request a snapshot of joined beings. The reply is returned via the
    /// [`RpcReplyPort`].
    ListBeings(RpcReplyPort<Vec<BeingSnapshot>>),

    /// Request the count of joined beings.
    Count(RpcReplyPort<usize>),
}

/// The shared prefix for `ractor::pg` process group names. Each bus instance
/// gets a **unique** group name derived from this (see [`BUS_SEQ`]), so
/// concurrent buses do not share the same member pool.
const PG_GROUP_PREFIX: &str = "resonance-bus";

/// A process-unique running counter for bus instances' pg group names.
///
/// Each [`ResonanceBus`] gets its own group in `pre_start`
/// (`resonance-bus-{n}`), so two different buses' beings never show up in
/// each other's [`pg::get_members`] results. `ractor::pg` is a
/// process-global namespace, so plain process-internal uniqueness is
/// enough — no clock or random number is needed.
static BUS_SEQ: AtomicU64 = AtomicU64::new(0);

/// [`ResonanceBus`] actor's internal state: registered beings.
///
/// Keeps a `HashMap` of metadata (names, `BeingInfo`) for `ListBeings` queries,
/// and uses a `ractor::pg` process group (`pg_group`) for member management
/// and distribution (broadcast).
pub struct BusState {
    /// Joined beings indexed by identifier (metadata).
    beings: HashMap<BeingId, BeingInfo>,
    /// This bus instance's own, process-unique `ractor::pg` group name.
    /// Isolates this bus's member pool from every other bus in the process.
    pg_group: String,
}

impl BusState {
    /// Delivers the envelope to everyone other than the sender, using the
    /// `ractor::pg` process group.
    fn broadcast(&self, envelope: &ResonanceMessage, _myself: &ActorRef<BusOp>) -> usize {
        let cells = pg::get_members(&self.pg_group);
        let mut delivered = 0;
        if let Some(sender_info) = self.beings.get(&envelope.from) {
            let sender_cell = sender_info.inbox().get_cell();
            for cell in cells {
                // Skip sender by comparing ActorCell (PartialEq compares ActorId)
                if cell == sender_cell {
                    continue;
                }
                let inbox_ref: ActorRef<ResonanceMessage> = cell.clone().into();
                match inbox_ref.cast(envelope.clone()) {
                    Ok(()) => delivered += 1,
                    Err(err) => {
                        warn!(
                            being = %cell.get_id(),
                            error = %err,
                            "failed to deliver message to being (mailbox closed?)"
                        );
                    }
                }
            }
        } else {
            // Sender not in our map (shouldn't happen), send to all
            for cell in cells {
                let inbox_ref: ActorRef<ResonanceMessage> = cell.clone().into();
                match inbox_ref.cast(envelope.clone()) {
                    Ok(()) => delivered += 1,
                    Err(err) => {
                        warn!(
                            being = %cell.get_id(),
                            error = %err,
                            "failed to deliver message to being (mailbox closed?)"
                        );
                    }
                }
            }
        }
        delivered
    }
}

/// The Resonance Bus actor.
///
/// Spawned via [`ResonanceBus::start`], which returns an ergonomic
/// [`BusHandle`]. The actor takes no constructor arguments — state starts as
/// an empty being registry.
pub struct ResonanceBus;

impl Actor for ResonanceBus {
    type Msg = BusOp;
    type State = BusState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (): Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        // Mint a unique, process-unique pg group name for this instance.
        let seq = BUS_SEQ.fetch_add(1, Ordering::Relaxed);
        let pg_group = format!("{PG_GROUP_PREFIX}-{seq}");
        Ok(BusState {
            beings: HashMap::new(),
            pg_group,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match message {
            BusOp::Register(info) => {
                let id = info.id();
                // Handle reregister: leave old inbox from process group if exists
                if let Some(old_info) = state.beings.get(&id) {
                    pg::leave(state.pg_group.clone(), vec![old_info.inbox().get_cell()]);
                    old_info.inbox().get_cell().unlink(myself.get_cell());
                }
                // Link the being as a child of the bus (bus = supervisor)
                info.inbox().get_cell().link(myself.get_cell());
                // Join the process group for distribution
                pg::join(state.pg_group.clone(), vec![info.inbox().get_cell()]);
                debug!(being = %id, name = info.name(), "being registered on bus");
                state.beings.insert(id, info);
            }
            BusOp::Deregister(id) => {
                if let Some(info) = state.beings.remove(&id) {
                    info.inbox().get_cell().unlink(myself.get_cell());
                    // Leave the process group
                    pg::leave(state.pg_group.clone(), vec![info.inbox().get_cell()]);
                    debug!(being = %id, "being removed from bus");
                }
            }
            BusOp::Publish(envelope) => {
                let kind = envelope.payload.kind_label();
                let n = state.broadcast(&envelope, &myself);
                debug!(
                    from = %envelope.from,
                    kind,
                    recipients = n,
                    "message published to bus"
                );
            }
            BusOp::ListBeings(reply) => {
                let snapshot: Vec<BeingSnapshot> =
                    state.beings.values().map(BeingSnapshot::from).collect();
                // The receiver may have given up (timeout) — don't panic if
                // the port is already closed.
                if reply.send(snapshot).is_err() {
                    warn!("being-list reply dropped (receiver gone)");
                }
            }
            BusOp::Count(reply) => {
                if reply.send(state.beings.len()).is_err() {
                    warn!("being-count reply dropped (receiver gone)");
                }
            }
        }
        Ok(())
    }

    /// Supervision: if a linked being crashes or terminates, remove it from
    /// the registry — **the bus stays alive**. This overrides Ractor's
    /// default (which would stop the supervisor when a child crashes).
    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match &event {
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let removed = remove_by_cell_id(state, cell.get_id());
                if let Some(id) = removed {
                    debug!(being = %id, ?reason, "being terminated — removed from registry");
                }
            }
            SupervisionEvent::ActorFailed(cell, err) => {
                let removed = remove_by_cell_id(state, cell.get_id());
                if let Some(id) = removed {
                    warn!(being = %id, error = %err, "being crashed — removed from registry, bus continues");
                }
            }
            // Other events (ActorStarted, group changes) require no action.
            _ => {}
        }
        Ok(())
    }
}

/// Removes from the registry the being whose mailbox actor has the given
/// [`ractor::ActorId`]. Returns the removed being's identifier if found.
fn remove_by_cell_id(state: &mut BusState, cell_id: ractor::ActorId) -> Option<BeingId> {
    let found = state
        .beings
        .iter()
        .find(|(_, info)| info.inbox().get_id() == cell_id)
        .map(|(id, _)| *id);
    if let Some(id) = found {
        state.beings.remove(&id);
    }
    found
}

/// An ergonomic handle to the [`ResonanceBus`] actor.
///
/// Wraps the raw [`ActorRef<BusOp>`] reference into an API that:
/// - does not use `unwrap`/`expect`/`panic!` on the production path,
/// - converts Ractor errors into [`FamilyClawError::Bus`] variants,
/// - provides clear methods ([`register`](BusHandle::register),
///   [`publish`](BusHandle::publish), [`beings`](BusHandle::beings), …).
///
/// `BusHandle` is `Clone` — the same bus can be shared with multiple beings.
#[derive(Clone)]
pub struct BusHandle {
    actor: ActorRef<BusOp>,
}

impl BusHandle {
    /// Wraps an existing actor reference into a handle.
    #[must_use]
    pub fn from_ref(actor: ActorRef<BusOp>) -> Self {
        Self { actor }
    }

    /// Returns the underlying actor reference (e.g. for linking beings).
    #[must_use]
    pub fn actor_ref(&self) -> &ActorRef<BusOp> {
        &self.actor
    }

    /// Registers a being on the bus.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if sending the message to the actor fails.
    pub fn register(&self, info: BeingInfo) -> Result<()> {
        self.actor
            .cast(BusOp::Register(info))
            .map_err(|e| FamilyClawError::bus(format!("register failed: {e}")))
    }

    /// Removes a being from the bus by identifier.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if sending the message to the actor fails.
    pub fn deregister(&self, id: BeingId) -> Result<()> {
        self.actor
            .cast(BusOp::Deregister(id))
            .map_err(|e| FamilyClawError::bus(format!("deregister failed: {e}")))
    }

    /// Publishes a ready-made envelope to the bus.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if sending the message to the actor fails.
    pub fn publish_envelope(&self, envelope: ResonanceMessage) -> Result<()> {
        self.actor
            .cast(BusOp::Publish(envelope))
            .map_err(|e| FamilyClawError::bus(format!("publish failed: {e}")))
    }

    /// Publishes a payload on the sender's behalf (builds the envelope).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if sending the message to the actor fails.
    pub fn publish(&self, from: BeingId, payload: BusMessage) -> Result<()> {
        self.publish_envelope(ResonanceMessage::new(from, payload))
    }

    /// Publishes a payload with a **per-message origin** (F2): the envelope
    /// carries a [`crate::message::MessageOrigin`], from which the
    /// receiving agent derives the reply target per message. Used in the
    /// channel-layer bridge when an incoming message comes from the outside
    /// world in a known conversation.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if sending the message to the actor fails.
    pub fn publish_with_origin(
        &self,
        from: BeingId,
        payload: BusMessage,
        origin: crate::message::MessageOrigin,
    ) -> Result<()> {
        self.publish_envelope(ResonanceMessage::new(from, payload).with_origin(origin))
    }

    /// Returns a snapshot of joined beings.
    ///
    /// **This list is not empty when beings have joined** (design §2.2).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if the query fails or times out.
    pub async fn beings(&self) -> Result<Vec<BeingSnapshot>> {
        self.actor
            .call(BusOp::ListBeings, Some(DEFAULT_CALL_TIMEOUT))
            .await
            .map_err(|e| FamilyClawError::bus(format!("list beings failed: {e}")))?
            .success_or_else(|| FamilyClawError::bus("list beings: no reply (timeout)"))
    }

    /// Returns the count of joined beings.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if the query fails or times out.
    pub async fn count(&self) -> Result<usize> {
        self.actor
            .call(BusOp::Count, Some(DEFAULT_CALL_TIMEOUT))
            .await
            .map_err(|e| FamilyClawError::bus(format!("count failed: {e}")))?
            .success_or_else(|| FamilyClawError::bus("count: no reply (timeout)"))
    }

    /// Stops the bus cleanly.
    pub fn stop(&self) {
        self.actor.stop(None);
    }
}

impl ResonanceBus {
    /// Spawns the Resonance Bus actor and returns an ergonomic [`BusHandle`].
    ///
    /// `name` is an optional registration name for global lookup
    /// ([`ractor::registry`]).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if starting the actor fails.
    pub async fn start(name: Option<String>) -> Result<BusHandle> {
        let (actor, _join) = Actor::spawn(name, ResonanceBus, ())
            .await
            .map_err(|e| FamilyClawError::bus(format!("bus spawn failed: {e}")))?;
        Ok(BusHandle::from_ref(actor))
    }
}

#[cfg(test)]
mod tests {
    // Tests compare exact, representable f32 emotion-state values (e.g. 80.0)
    // that travel through the bus unchanged — exact comparison is correct here.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::being::{CollectedLog, CollectorBeing};
    use crate::message::TaskEventKind;
    use familyclaw_emotion::{Dimension, EmotionState};
    use ractor::Actor;
    use std::time::Duration as StdDuration;

    /// Helper: spawns a collector being and registers it on the bus.
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

    /// Helper: a short wait so asynchronous delivery has time to complete.
    async fn settle() {
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }

    fn log_len(log: &CollectedLog) -> usize {
        log.lock().expect("lock").len()
    }

    #[tokio::test]
    async fn beings_list_is_not_empty_after_join() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        assert_eq!(bus.count().await.expect("count"), 0);
        assert!(bus.beings().await.expect("beings").is_empty());

        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (id_b, _b, _lb) = join_being(&bus, "agent_b").await;

        let beings = bus.beings().await.expect("beings");
        assert_eq!(
            beings.len(),
            2,
            "beings[] must NOT be empty when beings have joined"
        );
        assert_eq!(bus.count().await.expect("count"), 2);

        let ids: Vec<BeingId> = beings.iter().map(|b| b.id).collect();
        assert!(ids.contains(&id_a));
        assert!(ids.contains(&id_b));

        bus.stop();
    }

    #[tokio::test]
    async fn broadcast_reaches_others_not_sender() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, log_a) = join_being(&bus, "agent_a").await;
        let (_id_b, _b, log_b) = join_being(&bus, "agent_b").await;
        let (_id_c, _c, log_c) = join_being(&bus, "agent_c").await;

        bus.publish(id_a, BusMessage::text("hello everyone"))
            .expect("publish");
        settle().await;

        // The sender does not receive its own message as an echo.
        assert_eq!(log_len(&log_a), 0);
        // The others receive it.
        assert_eq!(log_len(&log_b), 1);
        assert_eq!(log_len(&log_c), 1);

        let received = log_b.lock().expect("lock")[0].clone();
        assert_eq!(received.from, id_a);
        assert!(matches!(received.payload, BusMessage::Text { .. }));

        bus.stop();
    }

    #[tokio::test]
    async fn emotion_pulse_spreads_to_siblings_affective_contagion() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (_id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        // agent_a "in a creative flow" → the pulse leaks into the bus.
        let mut state = EmotionState::neutral();
        state.stimulate(Dimension::Curiosity, 80.0);
        state.stimulate(Dimension::Joy, 60.0);

        bus.publish(id_a, BusMessage::emotion_pulse(state))
            .expect("publish pulse");
        settle().await;

        let received = log_b.lock().expect("lock");
        assert_eq!(received.len(), 1);
        assert!(
            received[0].is_emotion_pulse(),
            "the sibling senses the emotion pulse"
        );
        if let BusMessage::EmotionPulse { state: got } = &received[0].payload {
            // The state the sibling received matches what was sent — the
            // contagion data is intact, so the receiver can react to it.
            assert_eq!(got.value(Dimension::Curiosity), 80.0);
        } else {
            panic!("expected EmotionPulse");
        }

        bus.stop();
    }

    #[tokio::test]
    async fn deregister_stops_delivery() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        bus.deregister(id_b).expect("deregister");
        settle().await;
        assert_eq!(bus.count().await.expect("count"), 1);

        bus.publish(id_a, BusMessage::text("still there?"))
            .expect("publish");
        settle().await;
        assert_eq!(
            log_len(&log_b),
            0,
            "a removed being does not receive messages"
        );

        bus.stop();
    }

    #[tokio::test]
    async fn task_event_broadcasts() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (_id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        bus.publish(
            id_a,
            BusMessage::task_event(TaskEventKind::Completed, "task-7"),
        )
        .expect("publish task event");
        settle().await;

        let received = log_b.lock().expect("lock");
        assert_eq!(received.len(), 1);
        match &received[0].payload {
            BusMessage::TaskEvent { event, task_id, .. } => {
                assert_eq!(event, &TaskEventKind::Completed);
                assert_eq!(task_id, "task-7");
            }
            other => panic!("expected TaskEvent, got {other:?}"),
        }

        bus.stop();
    }

    #[tokio::test]
    async fn crashing_being_is_removed_but_bus_survives() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, log_a) = join_being(&bus, "agent_a").await;
        let (_id_b, actor_b, _lb) = join_being(&bus, "agent_b").await;

        assert_eq!(bus.count().await.expect("count"), 2);

        // Kill the second being "hard" — simulates a crash.
        actor_b.kill();

        // Let the supervision event propagate and clean up the registry.
        settle().await;

        // The bus is still alive and serves queries.
        let count = bus.count().await.expect("bus survives crash");
        assert_eq!(count, 1, "the crashed being was removed, the bus continues");

        // The remaining being still receives messages.
        bus.publish(BeingId::new(), BusMessage::text("bus alive?"))
            .expect("publish after crash");
        settle().await;
        assert_eq!(log_len(&log_a), 1);

        // Sender id_a's entry is still in the registry.
        let beings = bus.beings().await.expect("beings");
        assert_eq!(beings.len(), 1);
        assert_eq!(beings[0].id, id_a);

        bus.stop();
    }

    #[tokio::test]
    async fn reregister_replaces_inbox() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let id = BeingId::new();

        // First mailbox.
        let log1 = CollectorBeing::new_log();
        let (actor1, _h1) = Actor::spawn(None, CollectorBeing, log1.clone())
            .await
            .expect("spawn 1");
        bus.register(BeingInfo::new(id, "agent_a", actor1.clone()))
            .expect("register 1");

        // Same identifier, new mailbox (re-registration).
        let log2 = CollectorBeing::new_log();
        let (actor2, _h2) = Actor::spawn(None, CollectorBeing, log2.clone())
            .await
            .expect("spawn 2");
        bus.register(BeingInfo::new(id, "agent_a", actor2.clone()))
            .expect("register 2");
        settle().await;

        assert_eq!(bus.count().await.expect("count"), 1, "no duplicate");

        // Another being sends — only the newest mailbox receives the message.
        bus.publish(BeingId::new(), BusMessage::text("which one gets it?"))
            .expect("publish");
        settle().await;
        assert_eq!(log_len(&log1), 0, "the old mailbox no longer receives");
        assert_eq!(log_len(&log2), 1, "the new mailbox receives");

        bus.stop();
    }

    #[tokio::test]
    async fn from_ref_and_actor_ref_roundtrip() {
        let bus = ResonanceBus::start(Some("named-bus".into()))
            .await
            .expect("start named bus");
        let cloned = BusHandle::from_ref(bus.actor_ref().clone());
        assert_eq!(cloned.count().await.expect("count via clone"), 0);
        bus.stop();
    }

    /// Regression test: two concurrent buses do NOT share a pg member pool.
    ///
    /// The old global `const PG_GROUP = "resonance-bus"` model made test A's
    /// beings show up in test B's `pg::get_members` result → broadcast counts
    /// leaked across tests (parallel flakiness). With a per-instance group
    /// name, each bus's view is strictly its own. This test would fail
    /// against the old code.
    #[tokio::test]
    async fn two_buses_have_isolated_member_pools() {
        let bus1 = ResonanceBus::start(None).await.expect("start bus1");
        let bus2 = ResonanceBus::start(None).await.expect("start bus2");

        // Join two beings to bus 1 and three to bus 2.
        let (_a1, _ra1, log1_a) = join_being(&bus1, "b1_agent_a").await;
        let (id1_b, _rb1, log1_b) = join_being(&bus1, "b1_agent_b").await;
        let (_a2, _ra2, log2_a) = join_being(&bus2, "b2_agent_a").await;
        let (_b2, _rb2, log2_b) = join_being(&bus2, "b2_agent_b").await;
        let (_c2, _rc2, log2_c) = join_being(&bus2, "b2_agent_c").await;

        // Counts are per-instance — not shared.
        assert_eq!(bus1.count().await.expect("count bus1"), 2);
        assert_eq!(bus2.count().await.expect("count bus2"), 3);

        // A broadcast on bus 1 reaches ONLY bus 1's other beings (agent_a),
        // NOT bus 2's three beings. This is the test's core: the old global
        // `PG_GROUP` would leak the message to bus 2's beings
        // (`pg::get_members` would return them too) → the `== 0` assertions
        // below would fail.
        bus1.publish(id1_b, BusMessage::text("family 1 only"))
            .expect("publish bus1");
        settle().await;

        // The sender does not receive its own message as an echo.
        assert_eq!(
            log_len(&log1_b),
            0,
            "the sender does not receive its own message"
        );
        // Bus 1's other being receives it (broadcast works within the bus).
        assert_eq!(log_len(&log1_a), 1, "bus 1's sibling receives the message");
        // Bus 2's beings receive NOTHING — this fails against the old global
        // member pool (a guard against cross-leak regression).
        assert_eq!(
            log_len(&log2_a),
            0,
            "bus 2's being does not receive bus 1's message"
        );
        assert_eq!(
            log_len(&log2_b),
            0,
            "bus 2's being does not receive bus 1's message"
        );
        assert_eq!(
            log_len(&log2_c),
            0,
            "bus 2's being does not receive bus 1's message"
        );

        bus1.stop();
        bus2.stop();
    }
}
