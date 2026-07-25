//! # familyclaw-observability
//!
//! An observability and access-control layer for a multi-agent fleet:
//! **metrics**, **event recording**, and **role-based access control**
//! (RBAC) — all dependency-light (only `familyclaw-core`,
//! `familyclaw-bridge`, `serde`, `serde_json`). No heavy
//! `metrics`/`opentelemetry` stacks: the Prometheus text export is
//! hand-written so the binary stays small (`FamilyClaw`'s 2-8 MB target).
//!
//! ## Parts
//! - [`metrics`] — [`MetricsRegistry`] and its types ([`Counter`], [`Gauge`],
//!   [`Histogram`]), plus deterministic [`MetricsRegistry::prometheus_export`].
//! - [`event_recorder`] — [`EventRecorder`] subscribes to the bridge
//!   layer's event bus ([`FamilyBridge`]) and converts events into
//!   metric updates (read-only, forward-compatible).
//! - [`rbac`] — [`RbacPolicy`] per-role capability grants (defense in
//!   depth on top of the sandbox).
//!
//! ## Design principles
//! - **Dependency-light.** No network or HTTP layer in this crate:
//!   [`MetricsRegistry::prometheus_export`] returns a plain `String`, which
//!   the gateway can serve (e.g. `GET /metrics`).
//! - **Deterministic export.** Metrics are ordered by name; the output is
//!   stable (golden-string testable).
//! - **Forward-compatible.** [`EventRecorder`] skips unknown
//!   ([`EventKind`]) variants in a `_ => {}` branch, so new event types
//!   don't break it.
//! - **Transport-independent.** Like [`familyclaw_bridge`], this crate
//!   consumes only the bridge's public interface — no binding to the
//!   Resonance Bus or other moving parts.
//!
//! [`EventKind`]: familyclaw_bridge::EventKind
//! [`FamilyBridge`]: familyclaw_bridge::FamilyBridge
//!
//! ## Example
//! ```
//! use familyclaw_bridge::{AgentRole, FamilyBridge};
//! use familyclaw_observability::{EventRecorder, MetricsRegistry, RbacPolicy};
//!
//! # async fn run() -> familyclaw_core::Result<()> {
//! let bridge = FamilyBridge::new();
//! let metrics = MetricsRegistry::with_fleet_defaults();
//! let mut recorder = EventRecorder::new(&bridge, metrics.clone());
//!
//! // Access control: Executor may run commands, Scout may not.
//! let policy = RbacPolicy::new().allow(AgentRole::Executor, "system.run");
//! assert!(policy.check(AgentRole::Executor, "system.run").is_ok());
//! assert!(policy.check(AgentRole::Scout, "system.run").is_err());
//!
//! // Generate some traffic and drain it into the metrics.
//! bridge.create_task("seed", None).await?;
//! recorder.drain_once().await;
//!
//! // Export as Prometheus text (the gateway can serve this).
//! let text = metrics.prometheus_export();
//! assert!(text.contains("tasks_created 1"));
//! # Ok(())
//! # }
//! ```

pub mod event_recorder;
pub mod metrics;
pub mod operator_acl;
#[cfg(feature = "otlp")]
pub mod otlp;
pub mod rbac;
pub mod trace;

pub use event_recorder::EventRecorder;
pub use metrics::{
    BucketBound, Counter, Gauge, Histogram, MetricsRegistry, COUNTER_AGENT_TURNS,
    COUNTER_CONTRACT_BREACHED, COUNTER_CONTRACT_FULFILLED, COUNTER_CONTRACT_PROPOSED,
    COUNTER_DURABLE_REPLAYS, COUNTER_LLM_CALLS, COUNTER_LLM_FALLBACKS, COUNTER_TASKS_COMPLETED,
    COUNTER_TASKS_CREATED, COUNTER_TASK_HANDOFFS, COUNTER_TOOL_CALLS,
    COUNTER_WORKFLOW_STEPS_COMPLETED, GAUGE_AGENTS_ONLINE,
};
pub use operator_acl::{OperatorAcl, OperatorRole};
#[cfg(feature = "otlp")]
pub use otlp::{otlp_endpoint_from_env, otlp_traces_url, OtlpSpanEnvelope, OTLP_ENDPOINT_ENV};
pub use rbac::{RbacError, RbacPolicy};
pub use trace::TraceContext;

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_bridge::{AgentRole, FamilyBridge};

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[tokio::test]
    async fn public_api_is_reexported() {
        // If any re-export is removed, this test will fail to compile.
        let metrics: MetricsRegistry = MetricsRegistry::with_fleet_defaults();
        let _counter: Counter = metrics.counter(COUNTER_TASKS_CREATED);
        let _gauge: Gauge = metrics.gauge(GAUGE_AGENTS_ONLINE);
        let _hist: Histogram = metrics.histogram("latency_seconds");
        let bound: BucketBound = BucketBound::PosInf;
        assert_eq!(bound.label(), "+Inf");

        let bridge: FamilyBridge = FamilyBridge::new();
        let _recorder: EventRecorder = EventRecorder::new(&bridge, metrics.clone());

        let policy: RbacPolicy = RbacPolicy::new().allow(AgentRole::Executor, "system.run");
        let ok: Result<(), RbacError> = policy.check(AgentRole::Executor, "system.run");
        assert!(ok.is_ok());
    }
}
