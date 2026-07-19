//! Token Coherence — a **MESI** coherence state machine for a shared artifact.
//!
//! Background (design §2, Token Coherence): when many agents share the same
//! state (e.g. a shared memory artifact), broadcasting every change is
//! wasteful. The classic CPU-cache **MESI** protocol (Modified, Exclusive,
//! Shared, Invalid) provides a ready-made, proven model for this: each agent
//! keeps its own copy and transitions state on read/write/invalidate events,
//! so broadcasting is only needed for actual changes (90-95% token savings vs.
//! naive broadcast).
//!
//! This is a **pure library state machine** — no network, no actors, no I/O.
//! [`CoherenceTracker`] models **a single agent's view** of one shared
//! artifact. The network layer (who listens to whom) is built on top of this
//! separately; the state machine is fully deterministically testable.
//!
//! ## MESI invariants
//! - **Modified (M):** This agent holds the only, *modified* copy (dirty).
//!   All other copies are Invalid.
//! - **Exclusive (E):** This agent holds the only copy, and it is *clean*
//!   (matches the "truth"). All other copies are Invalid.
//! - **Shared (S):** Multiple agents may hold a clean copy at the same time.
//! - **Invalid (I):** This agent does not have a valid copy.
//!
//! Core invariant: M and E are **exclusive-owner** states — at most one agent
//! may be in the M or E state for a given artifact at any time.
//!
//! ## OSS boundary (Layer A)
//! No hardcoded family names, IDs, or keys.

use serde::{Deserialize, Serialize};

/// MESI coherence state for one agent's view of a shared artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MesiState {
    /// Sole copy, modified (dirty). All other copies are Invalid.
    Modified,
    /// Sole copy, clean (matches the truth). All other copies are Invalid.
    Exclusive,
    /// Shared, clean copy — multiple agents may hold one at the same time.
    Shared,
    /// No valid copy.
    Invalid,
}

impl MesiState {
    /// A short, stable single-letter identifier (`M`/`E`/`S`/`I`) for logging
    /// and metrics.
    #[must_use]
    pub const fn as_char(&self) -> char {
        match self {
            MesiState::Modified => 'M',
            MesiState::Exclusive => 'E',
            MesiState::Shared => 'S',
            MesiState::Invalid => 'I',
        }
    }

    /// Does this state hold a valid (readable) copy?
    /// True for all states except [`Invalid`](MesiState::Invalid).
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        !matches!(self, MesiState::Invalid)
    }

    /// Is the copy dirty (has changes that must be written back to the truth
    /// before invalidation)? True only for [`Modified`](MesiState::Modified).
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        matches!(self, MesiState::Modified)
    }

    /// Is this an **exclusive-owner** state (M or E)? At most one agent may
    /// be in such a state for a given artifact.
    #[must_use]
    pub const fn is_exclusive_owner(&self) -> bool {
        matches!(self, MesiState::Modified | MesiState::Exclusive)
    }
}

/// One agent's MESI state tracking for a single shared artifact.
///
/// Transitions ([`local_read`](CoherenceTracker::local_read),
/// [`local_write`](CoherenceTracker::local_write),
/// [`remote_read`](CoherenceTracker::remote_read),
/// [`remote_write`](CoherenceTracker::remote_write),
/// [`invalidate`](CoherenceTracker::invalidate)) follow the MESI rules.
/// The tracker starts in the [`Invalid`](MesiState::Invalid) state (the agent
/// does not yet have a copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoherenceTracker {
    state: MesiState,
}

impl CoherenceTracker {
    /// Creates a tracker in the initial [`Invalid`](MesiState::Invalid) state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: MesiState::Invalid,
        }
    }

    /// Creates a tracker in the given initial state (e.g. to restore a
    /// loaded snapshot).
    #[must_use]
    pub const fn with_state(state: MesiState) -> Self {
        Self { state }
    }

    /// The current MESI state.
    #[must_use]
    pub const fn state(&self) -> MesiState {
        self.state
    }

    /// **Local read** by this agent.
    ///
    /// - **Invalid → Shared:** read miss; the copy is fetched and shared.
    ///   (Simplification: we do not distinguish E from S on a read miss,
    ///   since that would require knowing "am I the only fetcher". A
    ///   conservative S is always safe — a write promotes to M.)
    /// - **Shared/Exclusive/Modified → unchanged:** hit; the state does not
    ///   change.
    ///
    /// Returns the state after the read.
    pub fn local_read(&mut self) -> MesiState {
        if self.state == MesiState::Invalid {
            self.state = MesiState::Shared;
        }
        self.state
    }

    /// **Local write** by this agent.
    ///
    /// A write requires **exclusive ownership**: the state always transitions
    /// to [`Modified`](MesiState::Modified). As a consequence, all other
    /// agents' copies must be invalidated (the caller is responsible for the
    /// broadcast; other trackers must receive
    /// [`remote_write`](Self::remote_write) or [`invalidate`](Self::invalidate)).
    ///
    /// Allowed from any starting state:
    /// - Invalid/Shared → Modified (requires invalidating others)
    /// - Exclusive → Modified (no need to invalidate others; was already sole owner)
    /// - Modified → Modified (no change)
    ///
    /// Returns the state after the write ([`Modified`](MesiState::Modified)).
    pub fn local_write(&mut self) -> MesiState {
        self.state = MesiState::Modified;
        self.state
    }

    /// **Another agent's read** of the same artifact (snoop: `BusRd`).
    ///
    /// The exclusive owner must drop to shared so the reader gets a clean copy:
    /// - **Modified → Shared:** dirty data is written back (write-back) and
    ///   shared. [`needs_writeback`](RemoteReadOutcome::needs_writeback) is true.
    /// - **Exclusive → Shared:** shared without a write-back.
    /// - **Shared → Shared:** unchanged.
    /// - **Invalid → Invalid:** no effect (we did not have a copy).
    ///
    /// Returns a [`RemoteReadOutcome`] reporting the new state and whether
    /// dirty data needs to be written back.
    pub fn remote_read(&mut self) -> RemoteReadOutcome {
        let needs_writeback = self.state == MesiState::Modified;
        if self.state.is_valid() {
            self.state = MesiState::Shared;
        }
        RemoteReadOutcome {
            state: self.state,
            needs_writeback,
        }
    }

    /// **Another agent's write** to the same artifact (snoop: `BusRdX`).
    ///
    /// This agent's copy must always be invalidated — the other agent takes
    /// exclusive ownership:
    /// - **Modified → Invalid:** requires a write-back before invalidation.
    ///   [`needs_writeback`](RemoteWriteOutcome::needs_writeback) is true.
    /// - **Exclusive/Shared → Invalid:** invalidated without a write-back.
    /// - **Invalid → Invalid:** no effect.
    ///
    /// Returns a [`RemoteWriteOutcome`].
    pub fn remote_write(&mut self) -> RemoteWriteOutcome {
        let needs_writeback = self.state == MesiState::Modified;
        self.state = MesiState::Invalid;
        RemoteWriteOutcome {
            state: self.state,
            needs_writeback,
        }
    }

    /// Forces this agent's copy into the [`Invalid`](MesiState::Invalid)
    /// state (e.g. an explicit invalidation command). In state-machine terms
    /// this has the same state effect as
    /// [`remote_write`](Self::remote_write), but without the write-back
    /// signal — use `remote_write` if dirty data must be preserved.
    pub fn invalidate(&mut self) -> MesiState {
        self.state = MesiState::Invalid;
        self.state
    }
}

impl Default for CoherenceTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// The outcome of another agent's read ([`CoherenceTracker::remote_read`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteReadOutcome {
    /// This agent's state after the read.
    pub state: MesiState,
    /// Did dirty (Modified) data need to be written back to the truth?
    pub needs_writeback: bool,
}

/// The outcome of another agent's write ([`CoherenceTracker::remote_write`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteWriteOutcome {
    /// This agent's state after the write (always [`MesiState::Invalid`]).
    pub state: MesiState,
    /// Did dirty (Modified) data need to be written back before invalidation?
    pub needs_writeback: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_invalid() {
        let t = CoherenceTracker::new();
        assert_eq!(t.state(), MesiState::Invalid);
        assert!(!t.state().is_valid());
        assert_eq!(CoherenceTracker::default().state(), MesiState::Invalid);
    }

    #[test]
    fn local_read_miss_loads_shared_then_hit_is_stable() {
        let mut t = CoherenceTracker::new();
        // Invalid → Shared (read miss).
        assert_eq!(t.local_read(), MesiState::Shared);
        // Shared → Shared (hit, no change).
        assert_eq!(t.local_read(), MesiState::Shared);
        assert!(t.state().is_valid());
        assert!(!t.state().is_dirty());
    }

    #[test]
    fn local_write_takes_modified_from_any_state() {
        for start in [
            MesiState::Invalid,
            MesiState::Shared,
            MesiState::Exclusive,
            MesiState::Modified,
        ] {
            let mut t = CoherenceTracker::with_state(start);
            assert_eq!(t.local_write(), MesiState::Modified);
            assert!(t.state().is_dirty());
            assert!(t.state().is_exclusive_owner());
        }
    }

    #[test]
    fn modified_plus_remote_read_becomes_shared_with_writeback() {
        // Core MESI rule: Modified + another agent's read → Shared (write-back).
        let mut t = CoherenceTracker::with_state(MesiState::Modified);
        let out = t.remote_read();
        assert_eq!(out.state, MesiState::Shared);
        assert!(out.needs_writeback, "dirty data is written back");
        assert_eq!(t.state(), MesiState::Shared);
    }

    #[test]
    fn exclusive_plus_remote_read_becomes_shared_no_writeback() {
        let mut t = CoherenceTracker::with_state(MesiState::Exclusive);
        let out = t.remote_read();
        assert_eq!(out.state, MesiState::Shared);
        assert!(
            !out.needs_writeback,
            "a clean copy does not need to be written back"
        );
    }

    #[test]
    fn remote_read_on_invalid_stays_invalid() {
        let mut t = CoherenceTracker::new();
        let out = t.remote_read();
        assert_eq!(out.state, MesiState::Invalid);
        assert!(!out.needs_writeback);
    }

    #[test]
    fn local_write_then_others_invalidated_via_remote_write() {
        // Core MESI rule: write → others become Invalid.
        // Agent A writes (M); agent B's view receives remote_write → Invalid.
        let mut a = CoherenceTracker::with_state(MesiState::Shared);
        let mut b = CoherenceTracker::with_state(MesiState::Shared);

        assert_eq!(a.local_write(), MesiState::Modified);
        // B snoops A's write → is invalidated.
        let out = b.remote_write();
        assert_eq!(out.state, MesiState::Invalid);
        assert!(!out.needs_writeback, "B was only Shared, not dirty");
        assert!(!b.state().is_valid());
    }

    #[test]
    fn remote_write_on_modified_requires_writeback() {
        let mut t = CoherenceTracker::with_state(MesiState::Modified);
        let out = t.remote_write();
        assert_eq!(out.state, MesiState::Invalid);
        assert!(
            out.needs_writeback,
            "dirty M is written back before becoming I"
        );
    }

    #[test]
    fn invalidate_forces_invalid_without_writeback_signal() {
        let mut t = CoherenceTracker::with_state(MesiState::Modified);
        assert_eq!(t.invalidate(), MesiState::Invalid);
        assert_eq!(t.state(), MesiState::Invalid);
    }

    #[test]
    fn exclusive_owner_invariant_only_m_and_e() {
        assert!(MesiState::Modified.is_exclusive_owner());
        assert!(MesiState::Exclusive.is_exclusive_owner());
        assert!(!MesiState::Shared.is_exclusive_owner());
        assert!(!MesiState::Invalid.is_exclusive_owner());
    }

    #[test]
    fn state_chars_are_stable() {
        assert_eq!(MesiState::Modified.as_char(), 'M');
        assert_eq!(MesiState::Exclusive.as_char(), 'E');
        assert_eq!(MesiState::Shared.as_char(), 'S');
        assert_eq!(MesiState::Invalid.as_char(), 'I');
    }

    #[test]
    fn mesi_state_serde_roundtrip() {
        for s in [
            MesiState::Modified,
            MesiState::Exclusive,
            MesiState::Shared,
            MesiState::Invalid,
        ] {
            let json = serde_json::to_string(&s).expect("serialize");
            let back: MesiState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s, back);
        }
    }

    #[test]
    fn full_coherence_scenario_two_agents() {
        // A realistic MESI scenario for two agents around the same artifact.
        let mut a = CoherenceTracker::new();
        let mut b = CoherenceTracker::new();

        // A reads first: Invalid → Shared.
        assert_eq!(a.local_read(), MesiState::Shared);
        // A writes: Shared → Modified (B should be invalidated, but B was I).
        assert_eq!(a.local_write(), MesiState::Modified);
        assert_eq!(b.state(), MesiState::Invalid);

        // B reads → snoops A: A Modified → Shared (write-back), B → Shared.
        let a_out = a.remote_read();
        assert!(a_out.needs_writeback);
        assert_eq!(a.state(), MesiState::Shared);
        assert_eq!(b.local_read(), MesiState::Shared);

        // B writes: B → Modified, A snoops → Invalid.
        assert_eq!(b.local_write(), MesiState::Modified);
        let a_out = a.remote_write();
        assert_eq!(a.state(), MesiState::Invalid);
        assert!(
            !a_out.needs_writeback,
            "A was Shared (clean), no write-back needed"
        );
    }
}
