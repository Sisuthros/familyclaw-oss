//! Deterministic replay (the LOOP idea).
//!
//! The **LOOP** paper (2605.14237) proposes: record an execution *once* as
//! an ordered event log, and afterward replay it deterministically from the
//! log alone — with no clock, network, or any external input. Because all
//! non-deterministic inputs (bytes read, fuel consumed, final result) are
//! *recorded* into the events, replay needs neither the original backend nor
//! the original environment. This is a prerequisite for reliable debugging
//! and auditing (the same execution can be inspected bit-exactly after the
//! fact), and it cuts the token/compute cost to a fraction.
//!
//! ## What gets recorded
//! [`ExecutionTrace`] is an ordered sequence of [`TraceEvent`]s. The events
//! describe the *observations* relevant to determinism:
//! - [`TraceEvent::Started`] — execution began (backend + code size).
//! - [`TraceEvent::Output`] — the code produced a byte chunk.
//! - [`TraceEvent::FuelConsumed`] — `amount` units of fuel were consumed.
//! - [`TraceEvent::Finished`] — execution ended (succeeded / failed).
//!
//! ## Replay
//! [`replay`] walks the events and assembles an [`Outcome`] fully
//! deterministically: it *does not* run WASM code, and *does not* read the
//! clock or the network. Replaying the same [`ExecutionTrace`] always
//! produces an identical [`Outcome`].
//!
//! ## Example
//! ```
//! use familyclaw_sandbox::{ExecutionTrace, TraceEvent, replay};
//!
//! let trace = ExecutionTrace::new(vec![
//!     TraceEvent::started("noop", 4),
//!     TraceEvent::output(b"hello".to_vec()),
//!     TraceEvent::fuel_consumed(7),
//!     TraceEvent::finished(true),
//! ]);
//!
//! let a = replay(&trace).expect("replay ok");
//! let b = replay(&trace).expect("replay ok again");
//! assert_eq!(a, b); // determinism
//! assert_eq!(a.output, b"hello");
//! assert_eq!(a.fuel_consumed, 7);
//! assert!(a.success);
//! ```

use serde::{Deserialize, Serialize};

use crate::error::{Result, SandboxError};
use crate::sandbox::SandboxOutput;

/// A single recorded event during execution.
///
/// Events are serializable and complete with respect to determinism: replay
/// needs nothing from outside the events.
///
/// `#[non_exhaustive]` so that new event types can be added without breaking
/// downstream code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
#[non_exhaustive]
pub enum TraceEvent {
    /// Execution started.
    Started {
        /// The identifier of the backend that produced this log.
        backend: String,
        /// The size of the executed code in bytes.
        code_len: usize,
    },

    /// The code produced output bytes. Multiple chunks may occur; they are
    /// concatenated in order of occurrence during replay.
    Output {
        /// The produced byte chunk.
        bytes: Vec<u8>,
    },

    /// Fuel was consumed. Multiple entries are summed.
    FuelConsumed {
        /// The amount consumed at this step.
        amount: u64,
    },

    /// Execution ended.
    Finished {
        /// Whether the execution completed successfully.
        success: bool,
    },
}

impl TraceEvent {
    /// A [`TraceEvent::Started`] event.
    pub fn started(backend: impl Into<String>, code_len: usize) -> Self {
        Self::Started {
            backend: backend.into(),
            code_len,
        }
    }

    /// A [`TraceEvent::Output`] event.
    pub fn output(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Output {
            bytes: bytes.into(),
        }
    }

    /// A [`TraceEvent::FuelConsumed`] event.
    #[must_use]
    pub const fn fuel_consumed(amount: u64) -> Self {
        Self::FuelConsumed { amount }
    }

    /// A [`TraceEvent::Finished`] event.
    #[must_use]
    pub const fn finished(success: bool) -> Self {
        Self::Finished { success }
    }
}

/// An ordered, serializable log of a single sandbox execution.
///
/// This is the record of the LOOP mechanism: once recorded, it can be
/// replayed deterministically via [`replay`] without the original backend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionTrace {
    events: Vec<TraceEvent>,
}

impl ExecutionTrace {
    /// Builds a log from a ready sequence of events.
    #[must_use]
    pub fn new(events: impl Into<Vec<TraceEvent>>) -> Self {
        Self {
            events: events.into(),
        }
    }

    /// An empty log, to which events are added via [`push`](ExecutionTrace::push).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Appends an event to the end of the log (append-only).
    pub fn push(&mut self, event: TraceEvent) {
        self.events.push(event);
    }

    /// The events in append order.
    #[must_use]
    pub fn events(&self) -> &[TraceEvent] {
        &self.events
    }

    /// The number of events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// The result of a replay.
///
/// A serializable summary of what the execution produced: output bytes,
/// fuel consumed, and whether it succeeded. [`From<Outcome>`] produces a
/// [`SandboxOutput`] so the replay can be compared to the original
/// execution result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The combined output bytes in order of occurrence.
    pub output: Vec<u8>,
    /// The total fuel consumed.
    pub fuel_consumed: u64,
    /// Whether the execution succeeded.
    pub success: bool,
}

impl Outcome {
    /// Builds a result from its parts.
    #[must_use]
    pub fn new(output: impl Into<Vec<u8>>, fuel_consumed: u64, success: bool) -> Self {
        Self {
            output: output.into(),
            fuel_consumed,
            success,
        }
    }
}

impl From<Outcome> for SandboxOutput {
    /// A successful/failed [`Outcome`] maps to the bytes and consumption
    /// that [`SandboxOutput`] carries (the success flag is not part of the
    /// output — failure is typically expressed via an error at a higher
    /// level).
    fn from(outcome: Outcome) -> Self {
        SandboxOutput::new(outcome.output, outcome.fuel_consumed)
    }
}

/// Deterministically replays a recorded [`ExecutionTrace`].
///
/// Walks through the events and assembles an [`Outcome`] **from the log
/// alone** — it does not run WASM code, and does not read the clock or the
/// network. Replaying the same log always produces an identical result (the
/// LOOP guarantee).
///
/// For determinism, the log is structurally validated: it must begin with
/// [`TraceEvent::Started`] and end with [`TraceEvent::Finished`], and
/// neither may occur twice. This way an incomplete or corrupted log is
/// detected instead of producing a misleading "successful" result.
///
/// # Errors
/// [`SandboxError::Execution`] if the log is structurally invalid (empty,
/// does not begin with `Started`, does not end with `Finished`, or contains
/// duplicate lifecycle events).
pub fn replay(trace: &ExecutionTrace) -> Result<Outcome> {
    let events = trace.events();
    if events.is_empty() {
        return Err(SandboxError::execution("cannot replay an empty trace"));
    }

    let mut started = false;
    let mut finished = false;
    let mut output: Vec<u8> = Vec::new();
    let mut fuel_consumed: u64 = 0;
    let mut success = false;

    for event in events {
        match event {
            TraceEvent::Started { .. } => {
                if started {
                    return Err(SandboxError::execution(
                        "trace contains more than one Started event",
                    ));
                }
                started = true;
            }
            TraceEvent::Output { bytes } => {
                if !started || finished {
                    return Err(SandboxError::execution(
                        "Output event outside the Started..Finished window",
                    ));
                }
                output.extend_from_slice(bytes);
            }
            TraceEvent::FuelConsumed { amount } => {
                if !started || finished {
                    return Err(SandboxError::execution(
                        "FuelConsumed event outside the Started..Finished window",
                    ));
                }
                // Saturating addition: a corrupted log does not panic on overflow.
                fuel_consumed = fuel_consumed.saturating_add(*amount);
            }
            TraceEvent::Finished { success: s } => {
                if !started {
                    return Err(SandboxError::execution(
                        "Finished event before any Started event",
                    ));
                }
                if finished {
                    return Err(SandboxError::execution(
                        "trace contains more than one Finished event",
                    ));
                }
                finished = true;
                success = *s;
            }
        }
    }

    if !finished {
        return Err(SandboxError::execution(
            "trace ended without a Finished event",
        ));
    }

    Ok(Outcome {
        output,
        fuel_consumed,
        success,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_trace() -> ExecutionTrace {
        ExecutionTrace::new(vec![
            TraceEvent::started("noop", 4),
            TraceEvent::output(b"hello ".to_vec()),
            TraceEvent::output(b"world".to_vec()),
            TraceEvent::fuel_consumed(10),
            TraceEvent::fuel_consumed(5),
            TraceEvent::finished(true),
        ])
    }

    #[test]
    fn replay_assembles_outcome_from_trace() {
        let outcome = replay(&happy_trace()).expect("replay ok");
        assert_eq!(outcome.output, b"hello world");
        assert_eq!(outcome.fuel_consumed, 15);
        assert!(outcome.success);
    }

    #[test]
    fn replay_is_deterministic() {
        let trace = happy_trace();
        let first = replay(&trace).expect("first ok");
        let second = replay(&trace).expect("second ok");
        let third = replay(&trace).expect("third ok");
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn replay_preserves_failure_flag() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::fuel_consumed(3),
            TraceEvent::finished(false),
        ]);
        let outcome = replay(&trace).expect("replay ok");
        assert!(!outcome.success);
        assert_eq!(outcome.fuel_consumed, 3);
        assert!(outcome.output.is_empty());
    }

    #[test]
    fn replay_rejects_empty_trace() {
        let err = replay(&ExecutionTrace::empty()).expect_err("empty must fail");
        assert!(err.to_string().contains("empty trace"));
    }

    #[test]
    fn replay_rejects_missing_finished() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::output(b"x".to_vec()),
        ]);
        let err = replay(&trace).expect_err("missing finished must fail");
        assert!(err.to_string().contains("without a Finished"));
    }

    #[test]
    fn replay_rejects_output_before_started() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::output(b"x".to_vec()),
            TraceEvent::started("noop", 1),
            TraceEvent::finished(true),
        ]);
        let err = replay(&trace).expect_err("output before start must fail");
        assert!(err.to_string().contains("outside the Started"));
    }

    #[test]
    fn replay_rejects_double_finished() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::finished(true),
            TraceEvent::finished(true),
        ]);
        let err = replay(&trace).expect_err("double finished must fail");
        assert!(err.to_string().contains("more than one Finished"));
    }

    #[test]
    fn replay_rejects_double_started() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::started("noop", 1),
            TraceEvent::finished(true),
        ]);
        let err = replay(&trace).expect_err("double started must fail");
        assert!(err.to_string().contains("more than one Started"));
    }

    #[test]
    fn replay_fuel_saturates_without_overflow() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::fuel_consumed(u64::MAX),
            TraceEvent::fuel_consumed(10),
            TraceEvent::finished(true),
        ]);
        let outcome = replay(&trace).expect("replay ok");
        assert_eq!(outcome.fuel_consumed, u64::MAX);
    }

    #[test]
    fn trace_push_appends_in_order() {
        let mut trace = ExecutionTrace::empty();
        trace.push(TraceEvent::started("noop", 2));
        trace.push(TraceEvent::finished(true));
        assert_eq!(trace.len(), 2);
        assert!(!trace.is_empty());
        assert!(matches!(trace.events()[0], TraceEvent::Started { .. }));
    }

    #[test]
    fn trace_serde_roundtrip() {
        let trace = happy_trace();
        let json = serde_json::to_string(&trace).expect("serialize");
        let back: ExecutionTrace = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(trace, back);
        // The replay result survives a serde round-trip.
        assert_eq!(replay(&trace).unwrap(), replay(&back).unwrap());
    }

    #[test]
    fn outcome_converts_to_sandbox_output() {
        let outcome = Outcome::new(b"data".to_vec(), 42, true);
        let out: SandboxOutput = outcome.into();
        assert_eq!(out.output, b"data");
        assert_eq!(out.fuel_consumed, 42);
    }
}
