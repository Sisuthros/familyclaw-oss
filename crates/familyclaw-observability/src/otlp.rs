//! Optional OTLP scaffolding (feature `otlp`).
//!
//! `FamilyClaw` stays dependency-light by default: without this feature there is
//! **no** `OpenTelemetry` SDK. With `otlp`, operators get a thin export hook that
//! turns [`crate::TraceContext`] into a JSON span envelope suitable for an
//! OTLP/HTTP collector adapter (or a local file sink during development).
//!
//! This is intentionally **not** a full `OTel` SDK — it is the appliance-shaped
//! bridge so enterprise RFP checklists can point at a real export path.

use crate::trace::TraceContext;

/// Environment variable for the OTLP/HTTP traces endpoint.
pub const OTLP_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// One span ready for OTLP/HTTP `application/json` export (simplified shape).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OtlpSpanEnvelope {
    /// W3C trace id (32 hex).
    pub trace_id: String,
    /// W3C span id (16 hex).
    pub span_id: String,
    /// Logical operation name (e.g. `agent.turn`).
    pub name: String,
    /// Unix nanoseconds start (best-effort).
    pub start_time_unix_nano: u64,
    /// Unix nanoseconds end (best-effort).
    pub end_time_unix_nano: u64,
}

impl OtlpSpanEnvelope {
    /// Build an envelope from a [`TraceContext`] and operation name.
    #[must_use]
    pub fn from_trace(ctx: &TraceContext, name: impl Into<String>) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        Self {
            trace_id: ctx.trace_id.clone(),
            span_id: ctx.span_id.clone(),
            name: name.into(),
            start_time_unix_nano: now.saturating_sub(1_000_000),
            end_time_unix_nano: now,
        }
    }

    /// Serialize as compact JSON for a collector adapter.
    ///
    /// # Errors
    /// Propagates [`serde_json`] serialization failures.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Reads `OTEL_EXPORTER_OTLP_ENDPOINT` when set.
#[must_use]
pub fn otlp_endpoint_from_env() -> Option<String> {
    std::env::var(OTLP_ENDPOINT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Builds the traces URL (`{endpoint}/v1/traces`) when an endpoint is configured.
#[must_use]
pub fn otlp_traces_url() -> Option<String> {
    otlp_endpoint_from_env().map(|base| format!("{}/v1/traces", base.trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_json_contains_ids() {
        let ctx = TraceContext {
            trace_id: "0".repeat(32),
            span_id: "a".repeat(16),
        };
        let env = OtlpSpanEnvelope::from_trace(&ctx, "agent.turn");
        let json = env.to_json().expect("json");
        assert!(json.contains(&ctx.trace_id));
        assert!(json.contains("agent.turn"));
    }

    #[test]
    fn traces_url_none_when_unset() {
        // Do not mutate process env in parallel-safe unit tests; only assert
        // the helper shape when the var happens to be unset.
        if std::env::var(OTLP_ENDPOINT_ENV).is_err() {
            assert!(otlp_traces_url().is_none());
        }
    }
}
