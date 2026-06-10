//! # familyclaw-observability
//!
//! Havainnoitavuus- ja pääsynvalvontakerros monen agentin laivueelle:
//! **mittarit**, **tapahtumatallennus** ja **roolipohjainen pääsynvalvonta**
//! (RBAC) — kaikki riippuvuuskevyenä (vain `familyclaw-core`,
//! `familyclaw-bridge`, `serde`, `serde_json`). Ei raskaita
//! `metrics`-/`opentelemetry`-pinoja: Prometheus-tekstivienti on käsin
//! kirjoitettu, jotta binääri pysyy pienenä (`FamilyClaw`n 2–8 MB tavoite).
//!
//! ## Osat
//! - [`metrics`] — [`MetricsRegistry`] ja sen tyypit ([`Counter`], [`Gauge`],
//!   [`Histogram`]) sekä deterministinen [`MetricsRegistry::prometheus_export`].
//! - [`event_recorder`] — [`EventRecorder`] tilaa siltakerroksen
//!   tapahtumaväylän ([`FamilyBridge`]) ja muuntaa tapahtumat
//!   mittaripäivityksiksi (vain lukeva, eteenpäin-yhteensopiva).
//! - [`rbac`] — [`RbacPolicy`] per-rooli-kyvykkyysluvat (syvyyspuolustus
//!   sandboxin päällä).
//!
//! ## Suunnitteluperiaatteet
//! - **Riippuvuuskevyt.** Ei verkko- eikä HTTP-kerrosta tässä cratessa:
//!   [`MetricsRegistry::prometheus_export`] palauttaa pelkän `String`:n, jonka
//!   gateway voi tarjoilla (esim. `GET /metrics`).
//! - **Deterministinen vienti.** Mittarit järjestetään nimen mukaan; tuloste
//!   on vakaa (golden-string-testattava).
//! - **Eteenpäin-yhteensopiva.** [`EventRecorder`] ohittaa tuntemattomat
//!   ([`EventKind`]) lajit `_ => {}`-haarassa, joten uudet tapahtumatyypit
//!   eivät riko sitä.
//! - **Kuljetuksesta riippumaton.** Kuten [`familyclaw_bridge`], tämä crate
//!   kuluttaa vain sillan julkista rajapintaa — ei sidontaa Resonance Busiin
//!   tai muihin liikkuviin osiin.
//!
//! [`EventKind`]: familyclaw_bridge::EventKind
//! [`FamilyBridge`]: familyclaw_bridge::FamilyBridge
//!
//! ## Esimerkki
//! ```
//! use familyclaw_bridge::{AgentRole, FamilyBridge};
//! use familyclaw_observability::{EventRecorder, MetricsRegistry, RbacPolicy};
//!
//! # async fn run() -> familyclaw_core::Result<()> {
//! let bridge = FamilyBridge::new();
//! let metrics = MetricsRegistry::with_fleet_defaults();
//! let mut recorder = EventRecorder::new(&bridge, metrics.clone());
//!
//! // Pääsynvalvonta: Executor saa ajaa komentoja, Scout ei.
//! let policy = RbacPolicy::new().allow(AgentRole::Executor, "system.run");
//! assert!(policy.check(AgentRole::Executor, "system.run").is_ok());
//! assert!(policy.check(AgentRole::Scout, "system.run").is_err());
//!
//! // Tuota liikennettä ja valuta se mittareihin.
//! bridge.create_task("seed", None).await?;
//! recorder.drain_once().await;
//!
//! // Vie Prometheus-tekstinä (gateway voi tarjoilla tämän).
//! let text = metrics.prometheus_export();
//! assert!(text.contains("tasks_created 1"));
//! # Ok(())
//! # }
//! ```

pub mod event_recorder;
pub mod metrics;
pub mod rbac;

pub use event_recorder::EventRecorder;
pub use metrics::{
    BucketBound, Counter, Gauge, Histogram, MetricsRegistry, COUNTER_AGENT_TURNS,
    COUNTER_CONTRACT_BREACHED, COUNTER_CONTRACT_FULFILLED, COUNTER_CONTRACT_PROPOSED,
    COUNTER_DURABLE_REPLAYS, COUNTER_LLM_CALLS, COUNTER_LLM_FALLBACKS, COUNTER_TASKS_COMPLETED,
    COUNTER_TASKS_CREATED, COUNTER_TASK_HANDOFFS, COUNTER_WORKFLOW_STEPS_COMPLETED,
    GAUGE_AGENTS_ONLINE,
};
pub use rbac::{RbacError, RbacPolicy};

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
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
        // Jos jokin re-export poistetaan, tämä testi ei käänny.
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
