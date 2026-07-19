//! Weekly review ([`weekly_review`] + [`WeeklyReport`]).
//!
//! Where [`crate::DreamReport`] reports *one night's* events, the weekly
//! review is a **rolled-up snapshot** of the memory store at the end of the
//! week: how many memories are active/archived/tombstoned, which are the
//! most important surviving memories, and which conflicts are still
//! awaiting resolution ([`crate::conflict`]). This mirrors the Amplifier
//! prosthesis's weekly "scorecard" summary as a native feature (design
//! §2.3): it is an auditable, serializable report — it mutates nothing.
//!
//! The review is **deterministic**: it takes the `now` instant as a
//! parameter (not from the system clock) and sorts its output stably, so
//! the same store always produces the same report.

use familyclaw_core::{MessageId, Result, Timestamp};
use familyclaw_memory::{Memory, MemoryStatus, MemoryStore};
use serde::{Deserialize, Serialize};

use crate::conflict::is_conflicted;

/// How many of the most important memories the weekly review lists by default.
pub const DEFAULT_TOP_N: usize = 5;

/// A compact reference to one memory within a weekly review.
///
/// Does not carry the full [`Memory`] struct — only the id, importance, and
/// a short excerpt of content — so the report stays a lightweight, loggable,
/// serializable summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryDigest {
    /// The memory's identifier.
    pub id: MessageId,
    /// Precomputed combined importance (`0.0..=1.0`).
    pub importance: f32,
    /// The memory's content (truncated if long).
    pub content: String,
}

impl MemoryDigest {
    /// Content truncation limit in characters before the `…` ellipsis is appended.
    const CONTENT_CLAMP: usize = 120;

    /// Builds a digest from a memory, truncating long content.
    #[must_use]
    fn from_memory(memory: &Memory) -> Self {
        Self {
            id: memory.id,
            importance: memory.importance,
            content: clamp_content(&memory.content, Self::CONTENT_CLAMP),
        }
    }
}

/// Truncates content to at most `max` characters, appending `…` if truncated.
/// Operates on Unicode scalar values (not byte boundaries), so it never
/// breaks UTF-8.
fn clamp_content(content: &str, max: usize) -> String {
    if content.chars().count() <= max {
        return content.to_string();
    }
    let mut out: String = content.chars().take(max).collect();
    out.push('…');
    out
}

/// A rolled-up snapshot of the memory store for a single week.
///
/// The counters describe the store's *current state* at the moment of
/// review, `generated_at` (not the transitions that happened during the
/// week — those are tracked by the nightly [`crate::DreamReport`]).
/// `top_memories` is sorted descending by importance, and `conflicts` lists
/// the conflict-tagged memories still awaiting resolution.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WeeklyReport {
    /// The instant the review was generated (UTC). `None` until set.
    #[serde(default)]
    pub generated_at: Option<Timestamp>,
    /// Total number of memories in the store.
    pub total: usize,
    /// Number of active (full-weight, retrievable) memories.
    pub active: usize,
    /// Number of archived (decayed, still retrievable) memories.
    pub archived: usize,
    /// Number of tombstoned (removed from active retrieval) memories.
    pub tombstoned: usize,
    /// Number of consolidated (retrievable: active + archived) memories.
    /// This is a rough measure of "information still retained at week's end."
    pub consolidated: usize,
    /// Number of conflict-tagged ([`crate::conflict::CONFLICT_TAG`]) memories.
    pub conflicted: usize,
    /// The most important retrievable memories, sorted descending by importance.
    #[serde(default)]
    pub top_memories: Vec<MemoryDigest>,
    /// Conflict-tagged memories (id + importance + content), in id order.
    #[serde(default)]
    pub conflicts: Vec<MemoryDigest>,
}

impl WeeklyReport {
    /// Did the week produce anything worth keeping (is there retrievable content).
    #[must_use]
    pub fn has_content(&self) -> bool {
        self.total > 0
    }
}

/// Assembles a weekly review from the store's current state at instant `now`.
///
/// Lists at most [`DEFAULT_TOP_N`] of the most important retrievable
/// memories. Use [`weekly_review_top_n`] if you want a different limit.
///
/// # Errors
/// [`familyclaw_core::FamilyClawError`] if reading the memory store fails.
pub async fn weekly_review<S>(store: &S, now: Timestamp) -> Result<WeeklyReport>
where
    S: MemoryStore + ?Sized,
{
    weekly_review_top_n(store, now, DEFAULT_TOP_N).await
}

/// Like [`weekly_review`], but lists the top `top_n` most important memories.
///
/// # Errors
/// [`familyclaw_core::FamilyClawError`] if reading the memory store fails.
pub async fn weekly_review_top_n<S>(store: &S, now: Timestamp, top_n: usize) -> Result<WeeklyReport>
where
    S: MemoryStore + ?Sized,
{
    let memories = store.all().await?;

    let mut report = WeeklyReport {
        generated_at: Some(now),
        total: memories.len(),
        ..WeeklyReport::default()
    };

    // Collect conflicted memories deterministically (in id order).
    let mut conflicts: Vec<&Memory> = Vec::new();

    for memory in &memories {
        match memory.status {
            MemoryStatus::Active => report.active += 1,
            MemoryStatus::Archived => report.archived += 1,
            MemoryStatus::Tombstoned => report.tombstoned += 1,
        }
        if memory.status.is_retrievable() {
            report.consolidated += 1;
        }
        if is_conflicted(memory) {
            report.conflicted += 1;
            conflicts.push(memory);
        }
    }

    // Top-importance: only retrievable memories (tombstoned ones are never surfaced).
    let mut retrievable: Vec<&Memory> = memories.iter().filter(|m| m.is_retrievable()).collect();
    // Descending importance; ties broken by the smaller id (deterministic).
    retrievable.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    report.top_memories = retrievable
        .into_iter()
        .take(top_n)
        .map(MemoryDigest::from_memory)
        .collect();

    // Conflicts in id order (stable presentation).
    conflicts.sort_by_key(|a| a.id);
    report.conflicts = conflicts
        .into_iter()
        .map(MemoryDigest::from_memory)
        .collect();

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use familyclaw_memory::{ImportanceFactors, LocalJsonStore};

    use crate::conflict::tag_conflict;

    /// Fixed reference instant: 2026-06-04 12:00 UTC (deterministic).
    fn at() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
            .single()
            .expect("valid instant")
    }

    fn mem(content: &str, importance: f32) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(importance, 0.0, 0.0, 0.0))
            .build()
    }

    #[tokio::test]
    async fn empty_store_yields_empty_report() {
        let store = LocalJsonStore::in_memory();
        let report = weekly_review(&store, at()).await.expect("review");
        assert_eq!(report.total, 0);
        assert_eq!(report.consolidated, 0);
        assert_eq!(report.conflicted, 0);
        assert!(report.top_memories.is_empty());
        assert!(report.conflicts.is_empty());
        assert_eq!(report.generated_at, Some(at()));
        assert!(!report.has_content());
    }

    #[tokio::test]
    async fn counts_statuses_correctly() {
        let store = LocalJsonStore::in_memory();
        let active = store.add(mem("active one", 0.5)).await.expect("a");
        let archived = store.add(mem("archived one", 0.4)).await.expect("b");
        let tombstoned = store.add(mem("buried one", 0.3)).await.expect("c");
        store
            .set_status(archived, MemoryStatus::Archived)
            .await
            .expect("arch");
        store
            .set_status(tombstoned, MemoryStatus::Tombstoned)
            .await
            .expect("tomb");

        let report = weekly_review(&store, at()).await.expect("review");
        assert_eq!(report.total, 3);
        assert_eq!(report.active, 1);
        assert_eq!(report.archived, 1);
        assert_eq!(report.tombstoned, 1);
        // Consolidated = retrievable = active + archived.
        assert_eq!(report.consolidated, 2);
        assert!(report.has_content());
        // Tombstoned memories are never surfaced in the top list.
        assert!(report.top_memories.iter().all(|d| d.id != tombstoned));
        assert!(report.top_memories.iter().any(|d| d.id == active));
    }

    #[tokio::test]
    async fn top_memories_sorted_by_importance_descending() {
        let store = LocalJsonStore::in_memory();
        store.add(mem("low", 0.1)).await.expect("a");
        let high = store.add(mem("high", 0.9)).await.expect("b");
        let mid = store.add(mem("mid", 0.5)).await.expect("c");

        let report = weekly_review(&store, at()).await.expect("review");
        assert_eq!(report.top_memories.len(), 3);
        // Most important first.
        assert_eq!(report.top_memories[0].id, high);
        assert_eq!(report.top_memories[1].id, mid);
        // Descending order.
        assert!(report.top_memories[0].importance >= report.top_memories[1].importance);
        assert!(report.top_memories[1].importance >= report.top_memories[2].importance);
    }

    #[tokio::test]
    async fn top_n_limits_list() {
        let store = LocalJsonStore::in_memory();
        for i in 0u8..10 {
            store
                .add(mem(&format!("memory {i}"), 0.1 * f32::from(i)))
                .await
                .expect("add");
        }
        let report = weekly_review_top_n(&store, at(), 3).await.expect("review");
        assert_eq!(report.total, 10);
        assert_eq!(report.top_memories.len(), 3, "top_n limits the list");
    }

    #[tokio::test]
    async fn detected_conflicts_are_summarized() {
        let store = LocalJsonStore::in_memory();
        let a = store
            .add(mem("agent_a is in city a", 0.5))
            .await
            .expect("a");
        let b = store
            .add(mem("agent_a is in city b", 0.5))
            .await
            .expect("b");
        store.add(mem("unrelated fact", 0.5)).await.expect("c");

        tag_conflict(&store, a, b, at()).await.expect("tag");

        let report = weekly_review(&store, at()).await.expect("review");
        assert_eq!(report.conflicted, 2, "both parties are counted");
        assert_eq!(report.conflicts.len(), 2);
        // Conflict list ids = both parties.
        let ids: Vec<MessageId> = report.conflicts.iter().map(|d| d.id).collect();
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
        // Conflict tagging did not remove any memories → they're still in the total.
        assert_eq!(report.total, 3);
    }

    #[tokio::test]
    async fn long_content_is_clamped_in_digest() {
        let store = LocalJsonStore::in_memory();
        let long = "x".repeat(500);
        store.add(mem(&long, 0.9)).await.expect("add");
        let report = weekly_review(&store, at()).await.expect("review");
        let digest = &report.top_memories[0];
        // 120 characters + '…'.
        assert_eq!(
            digest.content.chars().count(),
            MemoryDigest::CONTENT_CLAMP + 1
        );
        assert!(digest.content.ends_with('…'));
    }

    #[tokio::test]
    async fn report_serde_roundtrip() {
        let store = LocalJsonStore::in_memory();
        let a = store.add(mem("claim x", 0.7)).await.expect("a");
        let b = store.add(mem("claim not-x", 0.6)).await.expect("b");
        tag_conflict(&store, a, b, at()).await.expect("tag");

        let report = weekly_review(&store, at()).await.expect("review");
        let json = serde_json::to_string(&report).expect("serialize");
        let back: WeeklyReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(report, back);
    }
}
