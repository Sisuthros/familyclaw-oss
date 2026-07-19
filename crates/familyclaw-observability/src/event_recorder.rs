//! [`EventRecorder`] — subscribes to the bridge layer's event bus and
//! converts events into metric updates.
//!
//! The recorder is **read-only**: it subscribes to the [`EventBus`]
//! ([`FamilyBridge::subscribe`]) and consumes events, but never publishes to
//! the bus. Each event kind ([`EventKind`]) is mapped to a
//! [`MetricsRegistry`] update. Unknown and future kinds (including
//! [`EventKind::Custom`] labels that are not individually recognized) are
//! **ignored** by the `_ => {}` branch — this way new event types never
//! break the recorder (forward compatibility).
//!
//! ## Event → metric map (which series are live)
//! The recorder only increments the series listed below. Other default
//! fleet series stay at zero until a corresponding event is produced for
//! them.
//!
//! | [`EventKind`] | Metric |
//! |---|---|
//! | `TaskCreated` | `tasks_created` (counter +1) |
//! | `TaskHandedOff` | `task_handoffs` (counter +1) |
//! | `AgentRegistered` | `agents_online` (gauge +1) |
//! | `AgentDeregistered` | `agents_online` (gauge -1) |
//! | `Custom("task.completed" \| "orchestration.task_completed")` | `tasks_completed` (+1) |
//! | `Custom("contract.proposed")` | `contract_proposed` (+1) |
//! | `Custom("contract.fulfilled")` | `contract_fulfilled` (+1) |
//! | `Custom("contract.breached")` | `contract_breached` (+1) |
//! | `Custom("agent.turn" \| "orchestration.agent_turn")` | `agent_turns` (+1) |
//! | `Custom("llm.call")` | `llm_calls` (+1) |
//! | `Custom("llm.fallback")` | `llm_fallbacks` (+1) |
//! | `Custom("durable.replay")` | `durable_replays` (+1) |
//! | `Custom("workflow.step_completed" \| "orchestration.workflow_step_completed")` | `workflow_steps_completed` (+1) |
//! | `Custom("tool.call" \| "orchestration.tool_call")` | `tool_calls` (+1) |
//!
//! `TaskStatusChanged` and `AgentHeartbeat` do not map to their own metric.
//!
//! ## Custom events
//! The orchestration and contract layers publish coordination events as
//! [`EventKind::Custom`] with a stable prefix (`contract.*`,
//! `orchestration.*`, `workflow.*`). The recorder recognizes the known
//! labels and increments the matching counters; unrecognized labels are
//! safely ignored.
//!
//! ## Usage
//! ```
//! use familyclaw_bridge::FamilyBridge;
//! use familyclaw_observability::{EventRecorder, MetricsRegistry};
//!
//! # async fn run() -> familyclaw_core::Result<()> {
//! let bridge = FamilyBridge::new();
//! let metrics = MetricsRegistry::with_fleet_defaults();
//! let mut recorder = EventRecorder::new(&bridge, metrics.clone());
//!
//! // Produce an event...
//! bridge.create_task("seed", None).await?;
//! // ...and drain it into the metrics.
//! recorder.drain_once().await;
//! # Ok(())
//! # }
//! ```
//!
//! [`EventBus`]: familyclaw_bridge::EventBus

use familyclaw_bridge::{EventKind, EventSubscriber, FamilyBridge};

use crate::metrics::{
    MetricsRegistry, COUNTER_AGENT_TURNS, COUNTER_CONTRACT_BREACHED, COUNTER_CONTRACT_FULFILLED,
    COUNTER_CONTRACT_PROPOSED, COUNTER_DURABLE_REPLAYS, COUNTER_LLM_CALLS, COUNTER_LLM_FALLBACKS,
    COUNTER_TASKS_COMPLETED, COUNTER_TASKS_CREATED, COUNTER_TASK_HANDOFFS, COUNTER_TOOL_CALLS,
    COUNTER_WORKFLOW_STEPS_COMPLETED, GAUGE_AGENTS_ONLINE,
};

/// Subscribes to the event bus and updates metrics based on the events
/// received.
///
/// Keep a single recorder alive for the lifetime of the gateway. A
/// subscriber only sees events published *after* it subscribes (see
/// [`EventBus`] semantics), so create the recorder before traffic starts
/// flowing.
///
/// [`EventBus`]: familyclaw_bridge::EventBus
#[derive(Debug)]
pub struct EventRecorder {
    subscriber: EventSubscriber,
    metrics: MetricsRegistry,
}

impl EventRecorder {
    /// Creates a recorder that subscribes to the given bridge's event bus
    /// and records into the given registry.
    #[must_use]
    pub fn new(bridge: &FamilyBridge, metrics: MetricsRegistry) -> Self {
        Self {
            subscriber: bridge.subscribe(),
            metrics,
        }
    }

    /// Access to the recorder's metrics registry.
    #[must_use]
    pub fn metrics(&self) -> &MetricsRegistry {
        &self.metrics
    }

    /// Drains all events *currently queued* without blocking, and returns
    /// how many events were processed.
    ///
    /// This does not wait for new events — it only empties what is
    /// immediately available. If the subscriber lagged and events were
    /// dropped, the drop is ignored (metrics cannot panic over this) and
    /// draining continues.
    ///
    /// `async` is part of the stable interface (alongside [`run`]) and is
    /// therefore not removable, even though the current implementation
    /// never `await`s — this allows a future blocking/backpressure variant
    /// to be added later without an API change.
    ///
    /// [`run`]: EventRecorder::run
    #[allow(clippy::unused_async)]
    pub async fn drain_once(&mut self) -> usize {
        let mut processed = 0usize;
        // `try_recv` returns `Ok(Some)` as long as there are events queued.
        // `Ok(None)` (empty) and `Err(_)` (lagged/closed) end the loop
        // cleanly — dropped events are not counted and no panic occurs.
        while let Ok(Some(event)) = self.subscriber.try_recv() {
            self.record(&event.kind);
            processed += 1;
        }
        processed
    }

    /// Blocking loop: waits for and processes events until the bus closes.
    ///
    /// Suited for a dedicated background task (`tokio::spawn`). Returns when
    /// the bus is closed (all senders dropped). Lagged conditions are
    /// ignored and listening continues.
    pub async fn run(mut self) {
        loop {
            match self.subscriber.recv().await {
                Ok(event) => self.record(&event.kind),
                // Closed → stop; lagged → continue.
                Err(err) => {
                    let msg = err.to_string();
                    if msg.contains("closed") {
                        break;
                    }
                    // lagged: keep listening.
                }
            }
        }
    }

    /// Converts a single event kind into a metric update.
    ///
    /// In addition to the known core kinds, stable [`EventKind::Custom`]
    /// labels are recognized. Unknown kinds are ignored (`_ => {}`) — this
    /// is a deliberate forward-compatibility allowance.
    fn record(&self, kind: &EventKind) {
        match kind {
            EventKind::TaskCreated => self.metrics.counter(COUNTER_TASKS_CREATED).inc(),
            EventKind::TaskHandedOff => self.metrics.counter(COUNTER_TASK_HANDOFFS).inc(),
            // Agent registration/deregistration moves the `agents_online`
            // gauge: registration +1, deregistration -1. This is an
            // instantaneous value (gauge), not a cumulative counter — it can
            // go up and down as agents come and go. The runtime publishes
            // registration when an agent spawns (observability bridge), so
            // the gauge reflects the live agent count.
            EventKind::AgentRegistered => self.metrics.gauge(GAUGE_AGENTS_ONLINE).add(1),
            EventKind::AgentDeregistered => self.metrics.gauge(GAUGE_AGENTS_ONLINE).sub(1),
            EventKind::Custom(label) => self.record_custom(label),
            // Forward compatibility: kinds that are safe to ignore.
            //
            // - `TaskStatusChanged`: we don't inspect the payload here, so we
            //   don't know the target state; task completion is recorded
            //   from a separate Custom label (workflow/orchestration).
            // - `AgentHeartbeat`: has no metric of its own (liveness is
            //   derived from registration).
            // - Any future variants (`_`).
            _ => {}
        }
    }

    /// Maps a stable Custom label to a metric update.
    fn record_custom(&self, label: &str) {
        match label {
            "task.completed" | "orchestration.task_completed" => {
                self.metrics.counter(COUNTER_TASKS_COMPLETED).inc();
            }
            "contract.proposed" => self.metrics.counter(COUNTER_CONTRACT_PROPOSED).inc(),
            "contract.fulfilled" => self.metrics.counter(COUNTER_CONTRACT_FULFILLED).inc(),
            "contract.breached" => self.metrics.counter(COUNTER_CONTRACT_BREACHED).inc(),
            "agent.turn" | "orchestration.agent_turn" => {
                self.metrics.counter(COUNTER_AGENT_TURNS).inc();
            }
            "llm.call" => self.metrics.counter(COUNTER_LLM_CALLS).inc(),
            "llm.fallback" => self.metrics.counter(COUNTER_LLM_FALLBACKS).inc(),
            "durable.replay" => self.metrics.counter(COUNTER_DURABLE_REPLAYS).inc(),
            "workflow.step_completed" | "orchestration.workflow_step_completed" => {
                self.metrics.counter(COUNTER_WORKFLOW_STEPS_COMPLETED).inc();
            }
            "tool.call" | "orchestration.tool_call" => {
                self.metrics.counter(COUNTER_TOOL_CALLS).inc();
            }
            // Unknown Custom label → ignore (forward compatibility).
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_bridge::{Event, EventBus, EventKind, FamilyBridge};

    #[tokio::test]
    async fn maps_task_created_event() {
        let bridge = FamilyBridge::new();
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        bridge.create_task("t", None).await.expect("create");
        let n = recorder.drain_once().await;
        assert_eq!(n, 1);
        assert_eq!(metrics.counter(COUNTER_TASKS_CREATED).get(), 1);
    }

    #[tokio::test]
    async fn maps_handoff_event() {
        use familyclaw_core::ids::AgentId;
        let bridge = FamilyBridge::new();
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        let from = AgentId::new();
        let to = AgentId::new();
        let task = bridge.create_task("t", Some(from)).await.expect("create");
        bridge
            .handoff_task(task.id, from, to)
            .await
            .expect("handoff");

        recorder.drain_once().await;
        assert_eq!(metrics.counter(COUNTER_TASKS_CREATED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_TASK_HANDOFFS).get(), 1);
    }

    #[tokio::test]
    async fn unknown_custom_variant_is_ignored() {
        // Use the raw bus to publish an arbitrary Custom event.
        let bus = EventBus::new();
        let bridge = FamilyBridge::from_parts(
            familyclaw_bridge::AgentRegistry::new(),
            familyclaw_bridge::TaskBoard::new(),
            bus.clone(),
        );
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        // A completely unknown Custom label.
        bus.publish(Event::new(
            EventKind::Custom("some.future.event".into()),
            None,
        ));
        let n = recorder.drain_once().await;
        // The event WAS processed (no panic), but no counter changed.
        assert_eq!(n, 1);
        let out = metrics.prometheus_export();
        // All counters still at zero.
        assert!(out.contains("tasks_created 0"));
        assert!(out.contains("contract_proposed 0"));
    }

    #[tokio::test]
    async fn known_custom_labels_increment_counters() {
        let bus = EventBus::new();
        let bridge = FamilyBridge::from_parts(
            familyclaw_bridge::AgentRegistry::new(),
            familyclaw_bridge::TaskBoard::new(),
            bus.clone(),
        );
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        for label in [
            "contract.proposed",
            "contract.fulfilled",
            "contract.breached",
            "llm.call",
            "llm.fallback",
            "durable.replay",
            "agent.turn",
            "workflow.step_completed",
            "task.completed",
            "tool.call",
        ] {
            bus.publish(Event::new(EventKind::Custom(label.into()), None));
        }
        let n = recorder.drain_once().await;
        assert_eq!(n, 10);
        assert_eq!(metrics.counter(COUNTER_CONTRACT_PROPOSED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_CONTRACT_FULFILLED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_CONTRACT_BREACHED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_LLM_CALLS).get(), 1);
        assert_eq!(metrics.counter(COUNTER_LLM_FALLBACKS).get(), 1);
        assert_eq!(metrics.counter(COUNTER_DURABLE_REPLAYS).get(), 1);
        assert_eq!(metrics.counter(COUNTER_AGENT_TURNS).get(), 1);
        assert_eq!(metrics.counter(COUNTER_WORKFLOW_STEPS_COMPLETED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_TASKS_COMPLETED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_TOOL_CALLS).get(), 1);
    }

    #[tokio::test]
    async fn agent_registration_moves_online_gauge_and_heartbeat_is_ignored() {
        use familyclaw_bridge::{AgentInfo, AgentRole, HostKind};
        use familyclaw_core::ids::AgentId;

        let bridge = FamilyBridge::new();
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        let info = AgentInfo::new(AgentId::new(), "a", AgentRole::Executor, HostKind::Local);
        let id = info.id;
        bridge.register_agent(info).await.expect("register");
        bridge.heartbeat_now(id).await.expect("heartbeat");

        let n = recorder.drain_once().await;
        // Both events were processed (register + heartbeat).
        assert_eq!(n, 2);
        // Registration bumped the agents_online gauge by +1; the heartbeat
        // has no metric of its own. The task counter did not change
        // (different event family).
        assert_eq!(metrics.gauge(GAUGE_AGENTS_ONLINE).get(), 1);
        assert_eq!(metrics.counter(COUNTER_TASKS_CREATED).get(), 0);

        // Deregistration brings the gauge back down to zero.
        assert!(
            bridge.deregister_agent(id).await.is_some(),
            "agentti poistui"
        );
        let n2 = recorder.drain_once().await;
        assert_eq!(n2, 1);
        assert_eq!(metrics.gauge(GAUGE_AGENTS_ONLINE).get(), 0);
    }

    #[tokio::test]
    async fn drain_once_on_empty_returns_zero() {
        let bridge = FamilyBridge::new();
        let metrics = MetricsRegistry::new();
        let mut recorder = EventRecorder::new(&bridge, metrics);
        assert_eq!(recorder.drain_once().await, 0);
    }
}
