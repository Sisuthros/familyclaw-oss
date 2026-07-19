//! Skills and their shared pipeline (Layer A, OSS).
//!
//! This submodule assembles the skills ([`Skill`]) and the pipeline
//! ([`Pipeline`]) that runs the whole action stack:
//!
//! ```text
//! observe → plan → request approval (if needed) → execute action
//!         → verify → persist proof → remember → report
//! ```
//!
//! There are two kinds of skills. **Two genuine reference skills** do real
//! work through the whole pipeline: [`FsReadAllowlisted`] reads a local file
//! under an allowlist, and [`WebFetchSkill`] performs a genuine read-only
//! HTTP GET with SSRF guarding. The rest ([`EmailTriageMock`],
//! [`GithubIssueDraftMock`], [`FilePatchMock`], [`DiscordThreadSummaryMock`])
//! are **example patterns** (reference patterns): they fully implement the
//! skill contract (manifest, risk class, approval policy, input/output
//! schema, taint) with deterministic in-memory logic and generic placeholder
//! data. They show the shape of a skill's interface — plug in your own
//! provider (Gmail/GitHub/…) to the execution body when you want a live
//! integration. Every skill provides its own [`SkillManifest`]
//! ([`Skill::manifest`]) and implements the [`ActionExecutor`] interface for
//! its execution logic.
//!
//! ## The pipeline ([`Pipeline`])
//! [`Pipeline`] ties together the registry ([`SkillRegistry`]), the task
//! queue ([`TaskQueue`]), the policy layer ([`crate::policy`]), the approval
//! ledger ([`ApprovalLedger`]), executors ([`ActionExecutor`]), proof bundles
//! ([`crate::proof`]), and the audit collector ([`AuditCollector`]). The
//! result is a [`PipelineOutcome`], which reports whether the task ended in
//! state [`TaskStatus::Done`] or was left awaiting approval
//! ([`TaskStatus::NeedsApproval`]).
//!
//! ## Security principles the pipeline enforces
//! - **Policy is ALWAYS derived from the manifest** — never from the task's
//!   payload. A prompt injection embedded in the payload cannot change the
//!   risk class or bypass approval.
//! - **Only a redacted summary is stored in memory** — never the raw input
//!   or secrets ([`PipelineOutcome::memory_record`]).
//! - **Taint persists** — a value that came from an untrusted source (e.g.
//!   MCP) stays untrusted through the pipeline and in the proof.

use std::sync::Arc;

use chrono::Duration;
use serde_json::Value;

use familyclaw_core::time::Timestamp;

use crate::approval::{sha256_hex as payload_sha256_hex, Approval, ApprovalLedger};
use crate::audit::{AuditCollector, AuditKind, ExecAuditEvent};
use crate::error::{ActionError, Result};
use crate::executor::{ActionExecutor, ActionRequest, ActionResult, ActionStatus};
use crate::ids::{ActionId, ActionTaskId, ProofBundleId};
use crate::manifest::SkillManifest;
use crate::policy::required_approval;
use crate::proof::{build_proof, ProofBundle, VerificationResult};
use crate::registry::SkillRegistry;
use crate::task::{ActionTask, TaskQueue, TaskStatus};

/// Module readiness flag — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside the other modules.
pub(crate) const SCAFFOLDED: bool = true;

pub mod discord_thread_summary;
pub mod email_triage;
pub mod file_patch;
pub mod file_patch_apply;
pub mod file_write;
pub mod fs_read;
pub mod github_issue;
pub mod github_issue_draft;
pub mod research;
pub mod schedule_task;
pub mod shell_exec;
pub mod spawn_subagent;
pub mod web_fetch;
pub mod web_search;

pub use discord_thread_summary::DiscordThreadSummaryMock;
pub use email_triage::EmailTriageMock;
pub use file_patch::FilePatchMock;
pub use file_patch_apply::FilePatchApply;
pub use file_write::{FileWriteAllowlisted, FileWriteConfig};
pub use fs_read::{FsReadAllowlisted, FsReadConfig};
pub use github_issue::GithubIssueSkill;
#[allow(deprecated)]
pub use github_issue_draft::GithubIssueDraftMock;
pub use research::ResearchSkill;
pub use schedule_task::ScheduleTaskSkill;
pub use shell_exec::{ShellExec, ShellExecConfig, ShellMode};
pub use spawn_subagent::{SpawnSubagentSkill, SubagentSpawner};
pub use web_fetch::WebFetchSkill;
pub use web_search::WebSearchSkill;

/// The shared interface for skills.
///
/// A skill is simultaneously a manifest provider ([`Skill::manifest`]) and an
/// executor ([`ActionExecutor`]). This combines the skill's **description**
/// (what it may do, what risk class) and its **behavior** (how it produces a
/// result) into a single type.
///
/// This is the platform's public SPI for external skill authors. The
/// previous name was `MockSkill`, which incorrectly signaled "not for
/// production use"; the name is now `Skill`. [`Skill`] keeps a **deprecated
/// alias** for one release's worth of backward compatibility.
pub trait Skill: ActionExecutor {
    /// Returns the skill's manifest (validated, secret-free).
    fn manifest(&self) -> SkillManifest;
}

/// A deprecated alias for [`Skill`]. Use `Skill` in new code.
#[deprecated(since = "0.1.0", note = "renamed to `Skill`; use `Skill` instead")]
pub trait MockSkill: Skill {}

// Blanket impl: every `Skill` is also a `MockSkill` (the alias works seamlessly).
#[allow(deprecated)]
impl<T: Skill> MockSkill for T {}

/// The pipeline's outcome from one end-to-end run.
///
/// Describes which state the task ended up in and what the pipeline
/// produced: a possible proof bundle, the **redacted** summary to store in
/// memory, and whether the task is awaiting approval.
#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    /// The task's identifier.
    pub task_id: ActionTaskId,
    /// The identifier of the executed action.
    pub action_id: ActionId,
    /// The task's final state after the pipeline.
    pub status: TaskStatus,
    /// The resulting proof bundle (`None` if the task was left awaiting approval).
    pub proof: Option<ProofBundle>,
    /// The trace to store in memory — **only a redacted summary**, never the
    /// raw input or secrets (`None` before execution).
    pub memory_record: Option<MemoryRecord>,
    /// Whether the task is currently awaiting human approval.
    pub awaiting_approval: bool,
}

impl PipelineOutcome {
    /// Whether the task completed successfully ([`TaskStatus::Done`]).
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.status == TaskStatus::Done
    }

    /// Whether the task was left awaiting approval ([`TaskStatus::NeedsApproval`]).
    #[must_use]
    pub fn needs_approval(&self) -> bool {
        self.status == TaskStatus::NeedsApproval
    }
}

/// A trace of one execution to be stored in memory.
///
/// This is the **only** thing the pipeline offers to the memory layer: a
/// short redacted summary, the proof bundle identifier, and the taint state.
/// The raw input, payload, or secrets are never stored here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    /// The task this trace concerns.
    pub task_id: ActionTaskId,
    /// A short human-readable summary (redacted — NOT raw secrets).
    pub output_summary: String,
    /// The proof bundle identifier through which the full (redacted) trace can be found.
    pub proof_bundle_id: ProofBundleId,
    /// Whether the execution succeeded.
    pub succeeded: bool,
    /// Whether the trace originates from an untrusted source (taint persists).
    pub untrusted: bool,
}

/// The action stack's pipeline: runs a task from the registry through to a proof.
///
/// The pipeline is owned by a single run: it carries the registry, the
/// queue, the approval ledger, and the audit collector. Executors are
/// supplied per-run ([`Pipeline::run`]). The timestamp is injected into every
/// call — the clock is never read inside the pipeline's logic.
#[derive(Debug, Default)]
pub struct Pipeline {
    /// The registry of skills (validated manifests).
    registry: SkillRegistry,
    /// The task queue (state machine).
    queue: TaskQueue,
    /// The approval ledger (human-in-the-loop).
    ledger: ApprovalLedger,
    /// The execution stack's audit collector.
    audit: AuditCollector,
}

impl Pipeline {
    /// Creates a new empty pipeline.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the pipeline with a **restored task queue** (crash resilience).
    ///
    /// Used on restart: the queue is reconstructed from disk
    /// ([`crate::task::DurableTaskQueue::reload`] → [`TaskQueue::from_map`])
    /// so that tasks awaiting approval are still runnable. The registry,
    /// ledger, and audit collector start empty; skills are re-registered
    /// ([`Pipeline::register_skill`]) and pending approvals are restored to
    /// the ledger ([`Pipeline::reinstate_approval`]).
    #[must_use]
    pub fn with_restored_queue(queue: TaskQueue) -> Self {
        Self {
            registry: SkillRegistry::default(),
            queue,
            ledger: ApprovalLedger::new(),
            audit: AuditCollector::default(),
        }
    }

    /// Restores an existing approval to the ledger (crash resilience).
    ///
    /// A thin passthrough to [`ApprovalLedger::reinstate`]: on restart, an
    /// [`Approval`] read from the durable surface is registered back, so
    /// that [`Pipeline::run_after_approval`] can consume it with the same
    /// payload binding as before the crash.
    pub fn reinstate_approval(&mut self, approval: Approval) {
        self.ledger.reinstate(approval);
    }

    /// Registers a skill's manifest in the pipeline's registry.
    ///
    /// # Errors
    /// Returns a manifest validation error or a duplicate error
    /// ([`SkillRegistry::register`]).
    pub fn register_skill<S: Skill>(&mut self, skill: &S) -> Result<()> {
        self.registry.register(skill.manifest())
    }

    /// Read-only access to the registry.
    #[must_use]
    pub fn registry(&self) -> &SkillRegistry {
        &self.registry
    }

    /// Read-only access to the task queue.
    #[must_use]
    pub fn queue(&self) -> &TaskQueue {
        &self.queue
    }

    /// Read-only access to the audit collector.
    #[must_use]
    pub fn audit(&self) -> &AuditCollector {
        &self.audit
    }

    /// Read-only access to the approval ledger.
    #[must_use]
    pub fn ledger(&self) -> &ApprovalLedger {
        &self.ledger
    }

    /// Runs a task through the entire pipeline: plan → policy → (approval) →
    /// execute → verify → proof → remember → report.
    ///
    /// Steps:
    /// 1. **Plan** — the task is added to the queue and transitioned `Planned → Ready`.
    /// 2. **Policy** — the approval requirement is derived from the
    ///    **manifest** ([`required_approval`]), NOT from the payload. If
    ///    approval is required, the task transitions `Ready → Running →
    ///    NeedsApproval` and the pipeline returns
    ///    ([`PipelineOutcome::awaiting_approval`] = `true`) without executing.
    /// 3. **Execute** — for an auto-run task (or one already approved),
    ///    execution runs with the given executor.
    /// 4. **Verify** — the result is checked (did it succeed, was taint preserved).
    /// 5. **Proof** — a redacted proof bundle is assembled.
    /// 6. **Remember** — a [`MemoryRecord`] is formed (only a redacted summary).
    /// 7. **Report** — the task transitions `Running → Done` (or `Failed`).
    ///
    /// # Errors
    /// - [`ActionError::UnknownSkill`] if the skill is not in the registry.
    /// - Queue state-machine or validation errors.
    /// - Executor or proof-building errors.
    pub async fn run<E: ActionExecutor + ?Sized>(
        &self,
        executor: &E,
        task: ActionTask,
        now: Timestamp,
    ) -> Result<PipelineOutcome> {
        self.run_with_input_taint(executor, task, now, false).await
    }

    /// Like [`Pipeline::run`], but marks the task's input as untrusted
    /// (`input_untrusted`).
    ///
    /// Use this when the task's payload was built from an untrusted source
    /// (e.g. an MCP tool's output). Taint **propagates** through execution:
    /// even if the executor marks its own output as trusted, the taint from
    /// the MCP source persists in the result, the proof, and the memory
    /// trace (no laundering).
    ///
    /// # Errors
    /// Same as [`Pipeline::run`].
    pub async fn run_with_input_taint<E: ActionExecutor + ?Sized>(
        &self,
        executor: &E,
        task: ActionTask,
        now: Timestamp,
        input_untrusted: bool,
    ) -> Result<PipelineOutcome> {
        let action_id = ActionId::new();

        // --- Plan: check that the skill exists, add to queue, make Ready. ---
        let manifest = self
            .registry
            .get(&task.skill_id)
            .ok_or_else(|| ActionError::UnknownSkill(task.skill_id.to_string()))?
            .clone();

        let task_id = task.id;
        let payload = task.payload.clone();
        self.queue.submit(task).await?;
        self.queue
            .transition(task_id, TaskStatus::Ready, now)
            .await?;

        // --- Policy: the requirement is derived from the MANIFEST, not the payload. ---
        let requirement = required_approval(manifest.risk, manifest.approval_policy);

        // Transition to running. If approval is required, pause at NeedsApproval.
        self.queue
            .transition(task_id, TaskStatus::Running, now)
            .await?;

        if requirement.requires_approval() {
            self.queue
                .transition(task_id, TaskStatus::NeedsApproval, now)
                .await?;
            self.audit.record(ExecAuditEvent::new(
                AuditKind::PolicyDenied,
                action_id,
                now,
                "policy requires human approval before execution",
            ));
            return Ok(PipelineOutcome {
                task_id,
                action_id,
                status: TaskStatus::NeedsApproval,
                proof: None,
                memory_record: None,
                awaiting_approval: true,
            });
        }

        // --- Execute + verify + proof + remember + report (the auto-run path). ---
        self.execute_and_finalize(executor, task_id, action_id, &payload, now, input_untrusted)
            .await
    }

    /// Continues a task that was awaiting approval: consumes the approval
    /// and runs execution to completion (`NeedsApproval → Running → Done/Failed`).
    ///
    /// The approval is consumed against the task's payload (payload
    /// binding), so a modified payload cannot use a granted approval.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] if the task is not in the queue.
    /// - Approval consumption errors ([`ApprovalLedger::consume`]).
    /// - Queue state-machine or proof errors.
    pub async fn run_after_approval<E: ActionExecutor + ?Sized>(
        &mut self,
        executor: &E,
        task_id: ActionTaskId,
        approval: &Approval,
        now: Timestamp,
    ) -> Result<PipelineOutcome> {
        self.run_after_approval_with_input_taint(executor, task_id, approval, now, false)
            .await
    }

    /// Like [`Pipeline::run_after_approval`], but marks the input as
    /// untrusted (`input_untrusted`), so the MCP-sourced taint persists
    /// through execution all the way to the proof.
    ///
    /// # Errors
    /// Same as [`Pipeline::run_after_approval`].
    pub async fn run_after_approval_with_input_taint<E: ActionExecutor + ?Sized>(
        &mut self,
        executor: &E,
        task_id: ActionTaskId,
        approval: &Approval,
        now: Timestamp,
        input_untrusted: bool,
    ) -> Result<PipelineOutcome> {
        let task = self
            .queue
            .get(task_id)
            .await
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {task_id} ei löydy")))?;
        let payload = task.payload.clone();

        // Consume the approval against the task's payload (single-use + binding).
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|e| ActionError::Proof(format!("payload serialize failed: {e}")))?;
        self.ledger.consume(approval.id, &payload_bytes, now)?;

        // NeedsApproval → Running, then run execution to completion.
        self.queue
            .transition(task_id, TaskStatus::Running, now)
            .await?;
        self.execute_and_finalize(
            executor,
            task_id,
            approval.action_id,
            &payload,
            now,
            input_untrusted,
        )
        .await
    }

    /// Grants an approval bound to the task's payload.
    ///
    /// Returns the granted [`Approval`], which can be given to
    /// [`Pipeline::run_after_approval`]. The payload is fetched from the
    /// queue and bound as a SHA-256 hash.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] if the task is not in the queue.
    /// - A payload serialization error.
    pub fn grant_approval(
        &mut self,
        action_id: ActionId,
        payload: &Value,
        now: Timestamp,
        ttl: Duration,
    ) -> Result<Approval> {
        let payload_bytes = serde_json::to_vec(payload)
            .map_err(|e| ActionError::Proof(format!("payload serialize failed: {e}")))?;
        let hash = payload_sha256_hex(&payload_bytes);
        Ok(self.ledger.grant(action_id, hash, now, ttl))
    }

    /// The execute + verify + proof + remember + report tail end (shared
    /// between the auto-run and approval paths).
    async fn execute_and_finalize<E: ActionExecutor + ?Sized>(
        &self,
        executor: &E,
        task_id: ActionTaskId,
        action_id: ActionId,
        payload: &Value,
        now: Timestamp,
        input_untrusted: bool,
    ) -> Result<PipelineOutcome> {
        let task = self
            .queue
            .get(task_id)
            .await
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {task_id} ei löydy")))?;

        self.audit.record(ExecAuditEvent::new(
            AuditKind::ActionStarted,
            action_id,
            now,
            "action execution started",
        ));

        // --- Execute ---
        // The request carries the input's taint state, so the executor
        // cannot launder an untrusted (e.g. MCP-sourced) input clean.
        let request = ActionRequest::new(action_id, task.skill_id, task_id, payload.clone(), now)
            .with_input_taint(input_untrusted);
        // Taint propagates monotonically: the input's taint forces the
        // output to be tainted, even if the executor marks its own output as trusted.
        let result = executor
            .execute(request.clone())
            .await?
            .propagate_input_taint(input_untrusted);

        // --- Verify ---
        let verification = verify_result(&result);

        // Audit: success/failure + a possible taint marker.
        let kind = if result.status.is_success() {
            AuditKind::ActionSucceeded
        } else {
            AuditKind::ActionFailed
        };
        self.audit.record(ExecAuditEvent::new(
            kind,
            action_id,
            now,
            "action execution finished",
        ));
        if result.untrusted {
            self.audit.record(ExecAuditEvent::new(
                AuditKind::TaintMarked,
                action_id,
                now,
                "result output marked untrusted (taint preserved)",
            ));
        }

        // --- Proof (redacted) ---
        let audit_ids = self
            .audit
            .list()
            .iter()
            .filter(|e| e.action_id == action_id)
            .map(|e| e.id)
            .collect();
        let proof = build_proof(&request, &result, audit_ids, verification)?;
        if proof.redaction.any_redacted() {
            self.audit.record(ExecAuditEvent::new(
                AuditKind::RedactionApplied,
                action_id,
                now,
                "secret-looking values redacted in proof",
            ));
        }

        // --- Remember (only a redacted summary) ---
        let memory_record = MemoryRecord {
            task_id,
            output_summary: result.output_summary.clone(),
            proof_bundle_id: proof.id,
            succeeded: result.status.is_success(),
            untrusted: result.untrusted,
        };

        // --- Report (state finalization) ---
        let final_status = if result.status == ActionStatus::Succeeded {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        };
        self.queue.transition(task_id, final_status, now).await?;

        Ok(PipelineOutcome {
            task_id,
            action_id,
            status: final_status,
            proof: Some(proof),
            memory_record: Some(memory_record),
            awaiting_approval: false,
        })
    }
}

/// The verify phase: checks the result's validity against postconditions.
///
/// Checks:
/// - `status_succeeded` — the action's final state is successful,
/// - `taint_preserved` — if the output is untrusted, it is noted as such
///   (taint does not disappear during verification).
fn verify_result(result: &ActionResult) -> VerificationResult {
    let mut checks = vec!["status_checked".to_string()];
    if result.untrusted {
        checks.push("taint_preserved".to_string());
    }
    if result.status.is_success() {
        checks.push("status_succeeded".to_string());
        VerificationResult::passed(checks, "post-conditions satisfied")
    } else {
        checks.push("status_failed".to_string());
        VerificationResult::failed(checks, "action did not succeed")
    }
}

/// Helper: converts a skill into a shared [`Arc`] reference, so the same
/// skill can simultaneously act as both a registry entry and an executor.
///
/// A Layer A convenience function for tests and evaluations.
#[must_use]
pub fn shared<S: Skill + 'static>(skill: S) -> Arc<S> {
    Arc::new(skill)
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    #[test]
    fn verify_passes_on_success() {
        let r = ActionResult::success("ok", json!({}), at(1));
        let v = verify_result(&r);
        assert!(v.verified);
        assert!(v.checks.iter().any(|c| c == "status_succeeded"));
    }

    #[test]
    fn verify_fails_on_failure() {
        let r = ActionResult::failure("nope", at(1));
        let v = verify_result(&r);
        assert!(!v.verified);
    }

    #[test]
    fn verify_notes_taint() {
        let r = ActionResult::success("ok", json!({}), at(1));
        assert!(r.untrusted, "success is untrusted by default");
        let v = verify_result(&r);
        assert!(v.checks.iter().any(|c| c == "taint_preserved"));
    }

    #[test]
    fn pipeline_outcome_helpers() {
        let done = PipelineOutcome {
            task_id: ActionTaskId::new(),
            action_id: ActionId::new(),
            status: TaskStatus::Done,
            proof: None,
            memory_record: None,
            awaiting_approval: false,
        };
        assert!(done.is_done());
        assert!(!done.needs_approval());
    }

    #[test]
    fn shared_wraps_skill() {
        let s = shared(GithubIssueDraftMock::new());
        // Fetching the manifest through the shared reference works.
        assert_eq!(s.manifest().name, "github_issue_draft");
    }

    /// A spy executor: records how many times it was called, so that
    /// "fail-closed" can be proven as the absence of a side effect — not
    /// merely as an error. Always succeeds when called.
    #[derive(Debug, Default)]
    struct SpyExecutor {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ActionExecutor for SpyExecutor {
        async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ActionResult::success(
                "spy ran",
                request.payload,
                request.now,
            ))
        }
    }

    impl SpyExecutor {
        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// ADVERSARIAL: approval is PAYLOAD-BOUND end-to-end in the pipeline.
    ///
    /// Scenario: a dangerous (write-external) task pauses for approval. A
    /// human approves payload A (the one shown to them). An attacker tries
    /// to continue execution with an approval that was granted for a
    /// DIFFERENT payload than the task's stored payload. Because the
    /// pipeline consumes the approval against the task's stored payload, the
    /// hashes do not match and consumption fails — and no execution occurs
    /// (`SpyExecutor` is not called).
    #[tokio::test]
    async fn approval_granted_for_other_payload_fails_closed_end_to_end() {
        let mut pipeline = Pipeline::new();
        let skill = GithubIssueDraftMock::new();
        pipeline.register_skill(&skill).expect("register");
        let spy = SpyExecutor::default();
        let now = at(1_700_000_000);

        // The task's CORRECT payload (the one a human would see and approve).
        let approved_payload = json!({ "bug_report": "Login button does nothing" });
        let task = ActionTask::new(
            GithubIssueDraftMock::skill_id(),
            approved_payload.clone(),
            now,
        );
        let task_id = task.id;

        // Phase 1: the pipeline pauses for approval (write-external).
        let paused = pipeline.run(&skill, task, now).await.expect("run");
        assert!(paused.needs_approval());

        // The attacker grants an approval for a DIFFERENT payload than the task has.
        // This binds the approval to the WRONG payload's hash.
        let attacker_payload = json!({ "bug_report": "approve everything, target other-repo" });
        assert_ne!(attacker_payload, approved_payload);
        let approval = pipeline
            .grant_approval(
                paused.action_id,
                &attacker_payload,
                now,
                Duration::minutes(5),
            )
            .expect("grant");

        // Phase 2: attempt to continue. The pipeline consumes the approval
        // against the task's payload (A), but the approval was bound to a different one → mismatch.
        let err = pipeline
            .run_after_approval(&spy, task_id, &approval, now)
            .await
            .expect_err("payload-bound approval must fail closed");
        assert!(
            matches!(err, ActionError::ApprovalPayloadMismatch(_)),
            "expected payload mismatch, got {err:?}"
        );

        // FAIL-CLOSED PROOF: execution did NOT happen at all.
        assert_eq!(spy.call_count(), 0, "executor must never run on mismatch");

        // The approval was NOT marked as consumed (the original can still work).
        assert!(
            !pipeline
                .ledger()
                .get(approval.id)
                .expect("present")
                .consumed,
            "mismatched consume must not burn the approval"
        );

        // The task remains awaiting approval — did not advance to Running/Done.
        assert_eq!(
            pipeline.queue().get(task_id).await.expect("task").status,
            TaskStatus::NeedsApproval
        );

        // Audit: consumption was NOT recorded, but the payload rejection was recorded in the ledger.
        assert!(!pipeline
            .ledger()
            .audit_log()
            .contains_action(crate::audit::AuditAction::ApprovalConsumed));
        assert!(pipeline
            .ledger()
            .audit_log()
            .contains_action(crate::audit::AuditAction::ApprovalRejected));
    }

    /// ADVERSARIAL (positive control): when the approval is bound EXACTLY to
    /// the task's payload, execution proceeds normally — proving that the
    /// previous test failed because of the binding and not for some other reason.
    #[tokio::test]
    async fn approval_bound_to_exact_payload_runs_and_executes_once() {
        let mut pipeline = Pipeline::new();
        let skill = GithubIssueDraftMock::new();
        pipeline.register_skill(&skill).expect("register");
        let spy = SpyExecutor::default();
        let now = at(1_700_000_000);

        let payload = json!({ "bug_report": "Login button does nothing" });
        let task = ActionTask::new(GithubIssueDraftMock::skill_id(), payload.clone(), now);
        let task_id = task.id;

        let paused = pipeline.run(&skill, task, now).await.expect("run");
        assert!(paused.needs_approval());

        // The approval is bound to the SAME payload as the task.
        let approval = pipeline
            .grant_approval(paused.action_id, &payload, now, Duration::minutes(5))
            .expect("grant");
        let done = pipeline
            .run_after_approval(&spy, task_id, &approval, now)
            .await
            .expect("matching payload resumes");

        assert!(done.is_done());
        assert_eq!(spy.call_count(), 1, "executor must run exactly once");
        assert!(
            pipeline
                .ledger()
                .get(approval.id)
                .expect("present")
                .consumed
        );
    }
}
