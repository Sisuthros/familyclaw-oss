//! Lightweight W3C-compatible trace correlation (no OpenTelemetry SDK).

use std::fmt;

/// 128-bit trace id + 64-bit span id as lowercase hex (W3C `traceparent` shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    /// 32 hex chars.
    pub trace_id: String,
    /// 16 hex chars.
    pub span_id: String,
}

impl TraceContext {
    /// Fresh random-ish ids from a non-crypto counter + time mix.
    ///
    /// Good enough for log correlation; not a security boundary.
    #[must_use]
    pub fn new() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let trace_id = format!("{nanos:032x}");
        #[allow(clippy::cast_possible_truncation)]
        let span_mix = (nanos as u64).rotate_left(17) ^ 0x9e37_79b9;
        let span_id = format!("{span_mix:016x}");
        Self { trace_id, span_id }
    }

    /// Parse `traceparent: 00-<trace>-<span>-<flags>` (version 00). Soft-fail → `None`.
    #[must_use]
    pub fn from_traceparent(header: &str) -> Option<Self> {
        let parts: Vec<&str> = header.trim().split('-').collect();
        if parts.len() < 4 || parts[0] != "00" {
            return None;
        }
        let trace_id = parts[1];
        let span_id = parts[2];
        if trace_id.len() != 32 || span_id.len() != 16 {
            return None;
        }
        if !trace_id.chars().all(|c| c.is_ascii_hexdigit())
            || !span_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return None;
        }
        Some(Self {
            trace_id: trace_id.to_ascii_lowercase(),
            span_id: span_id.to_ascii_lowercase(),
        })
    }

    /// Emit a W3C `traceparent` value (sampled flag `01`).
    #[must_use]
    pub fn to_traceparent(&self) -> String {
        format!("00-{}-{}-01", self.trace_id, self.span_id)
    }
}

impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trace_id={} span_id={}", self.trace_id, self.span_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_traceparent() {
        let ctx = TraceContext {
            trace_id: "0".repeat(32),
            span_id: "a".repeat(16),
        };
        let header = ctx.to_traceparent();
        let parsed = TraceContext::from_traceparent(&header).expect("parse");
        assert_eq!(parsed.trace_id, ctx.trace_id);
        assert_eq!(parsed.span_id, ctx.span_id);
    }

    #[test]
    fn new_has_expected_lengths() {
        let ctx = TraceContext::new();
        assert_eq!(ctx.trace_id.len(), 32);
        assert_eq!(ctx.span_id.len(), 16);
    }
}
