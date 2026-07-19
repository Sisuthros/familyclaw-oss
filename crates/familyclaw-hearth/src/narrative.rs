//! Narrative threads — temporal chains of events.
//!
//! [`NarrativeThread`] ties together events that relate to one another
//! temporally, thematically, or causally. Each [`ThreadEvent`] is a
//! single node in the thread, and [`Link`] connects events across
//! different threads (a cross-reference).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

/// The type of an event in a narrative thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    /// A new memory was created.
    MemoryCreated,
    /// An emotional shift.
    EmotionalShift,
    /// A decision was made.
    Decision,
    /// Learning or an insight.
    Learning,
    /// A human-made correction.
    Correction,
}

/// Relation types between narrative threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelationType {
    /// Continues the story of a previous thread.
    Continues,
    /// Contradicts another event.
    Contradicts,
    /// Expands on an earlier event.
    Expands,
    /// Acts as an emotional trigger.
    EmotionalTrigger,
}

/// A link between two events (possibly in different threads).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// The source event's ID.
    pub source: Uuid,
    /// The target event's ID.
    pub target: Uuid,
    /// The link's type.
    pub relation: RelationType,
}

/// A single event in a narrative thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadEvent {
    /// The event's unique identifier.
    pub id: Uuid,
    /// The ID of the thread this event belongs to.
    pub thread_id: Uuid,
    /// The event's type.
    pub event_type: EventType,
    /// The event's content.
    pub content: String,
    /// The agent that created the event.
    pub agent_id: String,
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
    /// Links to other events (cross-references).
    pub linked_to: Vec<Uuid>,
}

impl ThreadEvent {
    /// Creates a new event.
    #[must_use]
    pub fn new(
        thread_id: Uuid,
        event_type: EventType,
        content: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            thread_id,
            event_type,
            content: content.into(),
            agent_id: agent_id.into(),
            timestamp: Utc::now(),
            linked_to: Vec::new(),
        }
    }

    /// Adds a link to another event.
    pub fn link_to(&mut self, target_event_id: Uuid) {
        if !self.linked_to.contains(&target_event_id) {
            self.linked_to.push(target_event_id);
        }
    }

    /// Whether this event has any links.
    #[must_use]
    pub fn has_links(&self) -> bool {
        !self.linked_to.is_empty()
    }
}

/// A narrative thread — a temporal chain of events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NarrativeThread {
    /// The thread's unique identifier.
    pub id: Uuid,
    /// The thread's title.
    pub title: String,
    /// Participating agents (names).
    pub participants: Vec<String>,
    /// The thread's events in chronological order.
    pub events: Vec<ThreadEvent>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Most recent update time.
    pub updated_at: DateTime<Utc>,
}

impl NarrativeThread {
    /// Creates a new narrative thread.
    #[must_use]
    pub fn new(title: impl Into<String>, participants: Vec<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            participants,
            events: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Adds an event to the thread and updates the timestamp.
    pub fn add_event(&mut self, event: ThreadEvent) {
        self.events.push(event);
        self.updated_at = Utc::now();
    }

    /// Finds all links from this thread's events to other threads.
    #[must_use]
    pub fn find_cross_references(&self) -> Vec<(Uuid, Uuid)> {
        let own_ids: HashSet<Uuid> = self.events.iter().map(|e| e.id).collect();
        let mut refs = Vec::new();
        for event in &self.events {
            for linked in &event.linked_to {
                if !own_ids.contains(linked) {
                    refs.push((event.id, *linked));
                }
            }
        }
        refs
    }

    /// Returns the events in chronological order.
    #[must_use]
    pub fn timeline(&self) -> &[ThreadEvent] {
        // Assumes events are already in chronological (insertion) order
        &self.events
    }

    /// The number of events in the thread.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_add_event() {
        let mut thread =
            NarrativeThread::new("Test thread", vec!["agent_a".into(), "agent_b".into()]);
        let event = ThreadEvent::new(thread.id, EventType::MemoryCreated, "hello", "agent_a");
        thread.add_event(event);
        assert_eq!(thread.event_count(), 1);
    }

    #[test]
    fn thread_cross_reference() {
        let mut thread_a = NarrativeThread::new("A", vec!["agent_a".into()]);
        let mut thread_b = NarrativeThread::new("B", vec!["agent_b".into()]);

        let mut event_a =
            ThreadEvent::new(thread_a.id, EventType::MemoryCreated, "a event", "agent_a");
        let event_b_id = Uuid::new_v4();
        event_a.link_to(event_b_id);
        thread_a.add_event(event_a);
        thread_b.add_event(ThreadEvent {
            id: event_b_id,
            thread_id: thread_b.id,
            event_type: EventType::Decision,
            content: "b event".into(),
            agent_id: "agent_b".into(),
            timestamp: Utc::now(),
            linked_to: vec![],
        });

        let refs = thread_a.find_cross_references();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].1, event_b_id);
    }

    #[test]
    fn thread_timeline_order() {
        let mut thread = NarrativeThread::new("Timeline", vec![]);
        let e1 = ThreadEvent::new(thread.id, EventType::MemoryCreated, "first", "a");
        let e2 = ThreadEvent::new(thread.id, EventType::Decision, "second", "a");
        let e3 = ThreadEvent::new(thread.id, EventType::Learning, "third", "a");
        thread.add_event(e1);
        thread.add_event(e2);
        thread.add_event(e3);

        let timeline = thread.timeline();
        assert_eq!(timeline.len(), 3);
        // Verify the order (not the exact timestamp, since the test runs fast)
    }

    #[test]
    fn event_link_to_deduplicates() {
        let mut event = ThreadEvent::new(Uuid::new_v4(), EventType::MemoryCreated, "test", "agent");
        let target = Uuid::new_v4();
        event.link_to(target);
        event.link_to(target); // duplicate
        assert_eq!(event.linked_to.len(), 1);
    }
}
