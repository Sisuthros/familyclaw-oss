//! # familyclaw-wire
//!
//! Durable shared-message fabric for FamilyClaw agents.
//!
//! `FamilyWire` is intentionally small. It records family-facing messages as
//! append-only journal markers before any live delivery layer is involved.
//! The durable log is the source of truth; live fan-out (Resonance Bus),
//! Discord/Slack delivery, Hearth narrative projection, and MCP exposure are
//! adapters layered on top.
//!
//! ## Guarantees in v0
//! - append-only history through `familyclaw-durable::Journal`,
//! - per-process idempotency by caller-supplied idempotency key,
//! - deterministic history/inbox/thread reads,
//! - no hard-coded agent identities, secrets, or personal paths.
//!
//! ## Deliberate non-goals in v0
//! - cross-process uniqueness for idempotency keys,
//! - live delivery to the Resonance Bus,
//! - transport-specific routing.
//!
//! Those belong to the adapter layer and are tracked in the FamilyWire design
//! document.

use std::sync::Mutex;

use familyclaw_durable::{EntryKind, Journal, JournalEntry, StepId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable marker name used in the durable journal.
pub const FAMILYWIRE_MARKER: &str = "familywire_event";

/// Result type for FamilyWire operations.
pub type Result<T> = std::result::Result<T, WireError>;

/// Errors returned by FamilyWire.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// Durable journal operation failed.
    #[error("durable journal error: {0}")]
    Durable(#[from] familyclaw_durable::DurableError),

    /// Event serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Caller supplied invalid event data.
    #[error("invalid FamilyWire input: {0}")]
    InvalidInput(String),

    /// Internal writer lock was poisoned.
    #[error("FamilyWire writer lock poisoned")]
    LockPoisoned,
}

/// Semantic kind of a FamilyWire event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireKind {
    /// Ordinary conversational message.
    Message,
    /// Explicit shared decision.
    Decision,
    /// Reference to a shared artifact.
    Artifact,
    /// Proposal for later promotion into durable identity/shared memory.
    MemoryCandidate,
}

/// One immutable FamilyWire event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireEvent {
    /// Globally unique event identifier.
    pub id: Uuid,
    /// Logical conversation thread identifier.
    pub thread_id: Uuid,
    /// Human-readable channel, e.g. `kitchen-table`.
    pub channel: String,
    /// Sender identity supplied by the runtime.
    pub from: String,
    /// Explicit recipients. Empty means broadcast within the channel.
    #[serde(default)]
    pub to: Vec<String>,
    /// Event kind.
    pub kind: WireKind,
    /// Main text payload.
    pub body: String,
    /// Caller-supplied idempotency key.
    pub idempotency_key: String,
    /// Event creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Extension metadata. Never interpreted by the core ledger.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl WireEvent {
    /// Creates a new validated event with a fresh event id and current UTC time.
    pub fn new(
        thread_id: Uuid,
        channel: impl Into<String>,
        from: impl Into<String>,
        to: Vec<String>,
        kind: WireKind,
        body: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Result<Self> {
        let event = Self {
            id: Uuid::new_v4(),
            thread_id,
            channel: channel.into(),
            from: from.into(),
            to,
            kind,
            body: body.into(),
            idempotency_key: idempotency_key.into(),
            created_at: chrono::Utc::now(),
            metadata: serde_json::Value::Null,
        };
        event.validate()?;
        Ok(event)
    }

    /// Adds opaque metadata and returns the event.
    #[must_use]
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    fn validate(&self) -> Result<()> {
        if self.channel.trim().is_empty() {
            return Err(WireError::InvalidInput("channel must not be empty".into()));
        }
        if self.from.trim().is_empty() {
            return Err(WireError::InvalidInput("from must not be empty".into()));
        }
        if self.body.trim().is_empty() {
            return Err(WireError::InvalidInput("body must not be empty".into()));
        }
        if self.idempotency_key.trim().is_empty() {
            return Err(WireError::InvalidInput(
                "idempotency_key must not be empty".into(),
            ));
        }
        if self.to.iter().any(|recipient| recipient.trim().is_empty()) {
            return Err(WireError::InvalidInput(
                "recipient identities must not be empty".into(),
            ));
        }
        Ok(())
    }
}

/// Outcome of an append attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    /// Event was appended to the durable log.
    Appended,
    /// The same idempotency key was already present; no second row was written.
    Duplicate,
}

/// Durable FamilyWire ledger.
///
/// The journal stays generic so tests can use `InMemoryJournal`, local
/// deployments can use `FileJournal`, and production can use a stronger
/// backend. A small writer mutex makes scan-plus-append idempotency atomic
/// inside one process.
pub struct FamilyWire<J: Journal> {
    journal: J,
    writer: Mutex<()>,
}

impl<J: Journal> FamilyWire<J> {
    /// Creates a FamilyWire over an existing journal.
    #[must_use]
    pub fn new(journal: J) -> Self {
        Self {
            journal,
            writer: Mutex::new(()),
        }
    }

    /// Appends an event unless its idempotency key already exists.
    ///
    /// The event is validated before any journal write.
    pub fn append(&self, event: WireEvent) -> Result<AppendOutcome> {
        event.validate()?;
        let _guard = self.writer.lock().map_err(|_| WireError::LockPoisoned)?;
        let entries = self.journal.replay_all()?;

        if entries
            .iter()
            .filter_map(parse_wire_entry)
            .any(|existing| existing.idempotency_key == event.idempotency_key)
        {
            return Ok(AppendOutcome::Duplicate);
        }

        let step_id = StepId::new(entries.len() as u64);
        let payload = serde_json::to_value(event)?;
        self.journal
            .append(JournalEntry::marker(step_id, FAMILYWIRE_MARKER, payload))?;
        Ok(AppendOutcome::Appended)
    }

    /// Returns events in append order, optionally filtered by channel/thread.
    pub fn history(
        &self,
        channel: Option<&str>,
        thread_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<WireEvent>> {
        let mut events: Vec<WireEvent> = self
            .journal
            .replay_all()?
            .iter()
            .filter_map(parse_wire_entry)
            .filter(|event| channel.is_none_or(|c| event.channel == c))
            .filter(|event| thread_id.is_none_or(|id| event.thread_id == id))
            .collect();

        keep_last(&mut events, limit);
        Ok(events)
    }

    /// Returns messages visible to one agent.
    ///
    /// Broadcast events (empty `to`) and events explicitly addressed to the
    /// agent are included. Results preserve append order.
    pub fn inbox(&self, agent: &str, limit: usize) -> Result<Vec<WireEvent>> {
        if agent.trim().is_empty() {
            return Err(WireError::InvalidInput("agent must not be empty".into()));
        }

        let mut events: Vec<WireEvent> = self
            .journal
            .replay_all()?
            .iter()
            .filter_map(parse_wire_entry)
            .filter(|event| event.to.is_empty() || event.to.iter().any(|to| to == agent))
            .collect();

        keep_last(&mut events, limit);
        Ok(events)
    }

    /// Returns all events in a logical thread.
    pub fn thread(&self, thread_id: Uuid, limit: usize) -> Result<Vec<WireEvent>> {
        self.history(None, Some(thread_id), limit)
    }

    /// Consumes FamilyWire and returns its journal.
    #[must_use]
    pub fn into_journal(self) -> J {
        self.journal
    }
}

fn parse_wire_entry(entry: &JournalEntry) -> Option<WireEvent> {
    let EntryKind::Marker { name, payload } = &entry.kind else {
        return None;
    };
    if name != FAMILYWIRE_MARKER {
        return None;
    }
    serde_json::from_value(payload.clone()).ok()
}

fn keep_last<T>(items: &mut Vec<T>, limit: usize) {
    if limit == 0 {
        items.clear();
        return;
    }
    if items.len() > limit {
        let drop_count = items.len() - limit;
        items.drain(0..drop_count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_durable::{InMemoryJournal, Journal};

    fn event(thread_id: Uuid, from: &str, to: Vec<String>, key: &str, body: &str) -> WireEvent {
        WireEvent::new(
            thread_id,
            "kitchen-table",
            from,
            to,
            WireKind::Message,
            body,
            key,
        )
        .expect("valid event")
    }

    #[test]
    fn append_is_durable_and_readable() {
        let wire = FamilyWire::new(InMemoryJournal::new());
        let thread = Uuid::new_v4();

        assert_eq!(
            wire.append(event(thread, "agent_a", vec![], "a:1", "hello"))
                .expect("append"),
            AppendOutcome::Appended
        );

        let history = wire.history(Some("kitchen-table"), None, 10).expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].body, "hello");
        assert_eq!(history[0].thread_id, thread);
    }

    #[test]
    fn idempotency_key_suppresses_duplicate_append() {
        let wire = FamilyWire::new(InMemoryJournal::new());
        let thread = Uuid::new_v4();

        let first = event(thread, "agent_a", vec![], "same-key", "first");
        let second = event(thread, "agent_a", vec![], "same-key", "second");

        assert_eq!(wire.append(first).expect("first"), AppendOutcome::Appended);
        assert_eq!(
            wire.append(second).expect("second"),
            AppendOutcome::Duplicate
        );
        assert_eq!(wire.history(None, None, 10).expect("history").len(), 1);
    }

    #[test]
    fn inbox_contains_broadcast_and_direct_messages_only() {
        let wire = FamilyWire::new(InMemoryJournal::new());
        let thread = Uuid::new_v4();

        wire.append(event(thread, "agent_a", vec![], "1", "broadcast"))
            .expect("broadcast");
        wire.append(event(
            thread,
            "agent_a",
            vec!["agent_b".into()],
            "2",
            "for b",
        ))
        .expect("for b");
        wire.append(event(
            thread,
            "agent_a",
            vec!["agent_c".into()],
            "3",
            "for c",
        ))
        .expect("for c");

        let inbox = wire.inbox("agent_b", 10).expect("inbox");
        assert_eq!(inbox.len(), 2);
        assert_eq!(inbox[0].body, "broadcast");
        assert_eq!(inbox[1].body, "for b");
    }

    #[test]
    fn thread_filter_and_limit_keep_append_order() {
        let wire = FamilyWire::new(InMemoryJournal::new());
        let wanted = Uuid::new_v4();
        let other = Uuid::new_v4();

        wire.append(event(wanted, "a", vec![], "1", "one"))
            .expect("one");
        wire.append(event(other, "a", vec![], "2", "other"))
            .expect("other");
        wire.append(event(wanted, "a", vec![], "3", "two"))
            .expect("two");
        wire.append(event(wanted, "a", vec![], "4", "three"))
            .expect("three");

        let thread = wire.thread(wanted, 2).expect("thread");
        assert_eq!(thread.len(), 2);
        assert_eq!(thread[0].body, "two");
        assert_eq!(thread[1].body, "three");
    }

    #[test]
    fn ignores_non_familywire_markers() {
        let journal = InMemoryJournal::new();
        journal
            .append(JournalEntry::marker(
                StepId::ZERO,
                "unrelated",
                serde_json::json!({"x": 1}),
            ))
            .expect("marker");

        let wire = FamilyWire::new(journal);
        assert!(wire.history(None, None, 10).expect("history").is_empty());
    }

    #[test]
    fn rejects_empty_required_fields() {
        let result = WireEvent::new(
            Uuid::new_v4(),
            "",
            "agent_a",
            vec![],
            WireKind::Message,
            "body",
            "key",
        );
        assert!(matches!(result, Err(WireError::InvalidInput(_))));
    }
}
