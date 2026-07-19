//! The dream cycle as a schedulable skill ([`DreamSkill`]) -- Phase 4, D5.
//!
//! This wraps [`familyclaw_dream::DreamCycle`] as a
//! [`familyclaw_actions`] skill so that [`familyclaw_scheduler`] can run it
//! as a scheduled task (through `submit_task_idempotent`) instead of the
//! runtime keeping a hand-coded `tokio::sleep` loop. Benefits:
//! - **observability:** the run goes through the action runtime -> turn-audit
//!   + metrics see it like any other tool call;
//! - **idempotency:** the scheduler's deterministic key prevents a double run
//!   across a crash replay;
//! - **consistency:** one scheduling mechanism for all proactive tasks.
//!
//! The skill lives **in the runtime** (not in the `familyclaw-actions` Layer A
//! core), because it needs the `familyclaw-dream` + `familyclaw-memory`
//! handles; `ActionRuntime::register_skill` is public, so an external skill
//! can be registered without bloating the core crate with these dependencies.

use std::sync::Arc;

use async_trait::async_trait;
use familyclaw_actions::manifest::SkillManifest;
use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
use familyclaw_actions::{ActionExecutor, ActionRequest, ActionResult, Result, Skill, SkillId};
use familyclaw_dream::DreamCycle;
use familyclaw_durable::Journal;
use familyclaw_memory::MemoryStore;
use serde_json::json;

/// The skill's fixed identifier. A constant UUID, so the scheduler's
/// idempotency key stays stable across processes (ASCII "dreamcyc" bytes as padding).
const DREAM_SKILL_UUID: uuid::Uuid = uuid::uuid!("d4ea3c1c-0000-4000-8000-647265616d63");

/// The dream cycle as a schedulable skill.
///
/// Carries the shared [`MemoryStore`] and [`Journal`] handles (the same ones
/// the runtime uses), so [`execute`](ActionExecutor::execute) can run a full
/// dream cycle without a payload dependency -- the input is an empty object `{}`.
pub struct DreamSkill {
    store: Arc<dyn MemoryStore + Send + Sync>,
    journal: Arc<dyn Journal + Send + Sync>,
}

impl DreamSkill {
    /// Builds the skill with shared memory and journal handles.
    #[must_use]
    pub fn new(
        store: Arc<dyn MemoryStore + Send + Sync>,
        journal: Arc<dyn Journal + Send + Sync>,
    ) -> Self {
        Self { store, journal }
    }

    /// The skill's fixed identifier (the scheduler's task refers to this).
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(DREAM_SKILL_UUID)
    }
}

#[async_trait]
impl ActionExecutor for DreamSkill {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        // Run one dream cycle with the injected clock (`request.now`) -- the
        // same timestamp the scheduler/runtime journals -> deterministic.
        let store: &(dyn MemoryStore + Send + Sync) = &*self.store;
        let journal: &(dyn Journal + Send + Sync) = &*self.journal;
        let cycle = DreamCycle::new(store);
        match cycle.run(journal, request.now).await {
            Ok(report) => {
                let output = json!({
                    "scanned": report.scanned,
                    "merged": report.merged,
                    "dropped": report.dropped,
                    "strengthened": report.strengthened,
                    "archived": report.archived,
                    "dates_absolutized": report.dates_absolutized,
                });
                Ok(ActionResult::success(
                    format!(
                        "dream cycle: scanned={} merged={} dropped={} strengthened={} archived={}",
                        report.scanned,
                        report.merged,
                        report.dropped,
                        report.strengthened,
                        report.archived
                    ),
                    output,
                    request.now,
                ))
            }
            Err(e) => Ok(ActionResult::failure(
                format!("dream cycle failed: {e}"),
                request.now,
            )),
        }
    }
}

impl Skill for DreamSkill {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "dream_cycle".to_string(),
            version: "1.0.0".to_string(),
            description: "Ajaa yhden muistin konsolidaatio-syklin (merge/drop/strengthen/archive) \
                          Eternal Thread -muistille. Sisäinen ylläpito; ei käyttäjäsyötettä."
                .to_string(),
            permissions: vec![SkillPermission::WriteLocalFiles],
            // Modifies local memory -> WriteLocal. `RequireApproval`
            // together with the WriteLocal risk RUNS AUTOMATICALLY (see
            // `policy.rs`: WriteLocal + RequireApproval -> AutoRun) -- an
            // internal maintenance task doesn't require human approval per
            // run, but it also isn't pure read (AutoIfReadOnly would be wrong).
            risk: ActionRisk::WriteLocal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("{} (ei syötettä)".to_string()),
            output_hint: Some("uni-raportin laskurit (scanned/merged/dropped/...)".to_string()),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            publisher: None,
            signature: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_durable::InMemoryJournal;
    use familyclaw_memory::{LocalJsonStore, Memory, MemoryStore};

    fn skill() -> DreamSkill {
        let store: Arc<dyn MemoryStore + Send + Sync> = Arc::new(LocalJsonStore::in_memory());
        let journal: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
        DreamSkill::new(store, journal)
    }

    #[test]
    fn manifest_is_write_local_autorun_and_generic() {
        let m = skill().manifest();
        assert_eq!(m.name, "dream_cycle");
        assert_eq!(m.risk, ActionRisk::WriteLocal);
        assert_eq!(m.approval_policy, ApprovalPolicy::RequireApproval);
        assert_eq!(m.permissions, vec![SkillPermission::WriteLocalFiles]);
        assert_eq!(m.id, DreamSkill::skill_id());
        // Layer B: name/description are generic, no family names. Forbidden
        // names are built from ROT13, so this TEST itself doesn't contain
        // Layer B names as literals (otherwise scripts/audit-layer-b.sh would
        // trip on this test file even though the production code is clean).
        let blob = format!("{} {}", m.name, m.description).to_lowercase();
        let forbidden_rot13 = ["yhzra", "yhzvan", "cubgba", "cevfzn", "nheben"];
        for enc in forbidden_rot13 {
            let frag: String = enc
                .chars()
                .map(|c| (((c as u8 - b'a' + 13) % 26) + b'a') as char)
                .collect();
            assert!(!blob.contains(&frag), "Layer B -vuoto manifestissa");
        }
    }

    #[tokio::test]
    async fn execute_runs_a_cycle_on_empty_store() {
        let store: Arc<dyn MemoryStore + Send + Sync> = Arc::new(LocalJsonStore::in_memory());
        let journal: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
        let s = DreamSkill::new(Arc::clone(&store), journal);
        let now = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("clock");
        let req = ActionRequest::new(
            familyclaw_actions::ActionId::new(),
            DreamSkill::skill_id(),
            familyclaw_actions::ActionTaskId::new(),
            json!({}),
            now,
        );
        let result = s.execute(req).await.expect("execute");
        // Empty memory -> succeeds, scanned 0.
        assert!(matches!(
            result.status,
            familyclaw_actions::ActionStatus::Succeeded
        ));
        assert!(result.output_summary.contains("scanned=0"));
    }

    #[tokio::test]
    async fn execute_consolidates_real_memories() {
        let store: Arc<dyn MemoryStore + Send + Sync> = Arc::new(LocalJsonStore::in_memory());
        let now = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("clock");
        store
            .add(Memory::builder("muisto yksi").created_at(now).build())
            .await
            .expect("add");
        let journal: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
        let s = DreamSkill::new(Arc::clone(&store), journal);
        let req = ActionRequest::new(
            familyclaw_actions::ActionId::new(),
            DreamSkill::skill_id(),
            familyclaw_actions::ActionTaskId::new(),
            json!({}),
            now,
        );
        let result = s.execute(req).await.expect("execute");
        assert!(matches!(
            result.status,
            familyclaw_actions::ActionStatus::Succeeded
        ));
        // One memory was scanned.
        assert!(result.output_summary.contains("scanned=1"));
    }
}
