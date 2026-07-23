//! Operator surface for the action runtime (Layer A).
//!
//! [`ActionRuntime`] is a thin facade that ties together the whole action
//! stack — the registry, queue, approval ledger, executors, proofs, and audit
//! collector — behind a single type, so that operator tools (e.g. the
//! `familyclaw-actions-cli` command-line binary) can be plain shells. The
//! facade provides exactly the operations the operator needs:
//!
//! ```text
//! list-skills   → registered skills + risk class (no secrets)
//! submit-task   → submit a task, run the pipeline, return the task id
//! approve       → consume/mark an approval → continue execution to completion
//! status        → task status
//! proof         → redacted proof bundle (retrievable by id)
//! ```
//!
//! ## Security principles (same as the pipeline)
//! - **Policy is ALWAYS derived from the manifest**, never from the task payload.
//! - **Only redacted proofs** ([`crate::proof`]) are stored and
//!   returned — the raw payload or secrets are never exposed.
//! - **Approval is payload-bound and single-use**; a changed payload cannot
//!   consume a granted approval.
//! - **Determinism:** a timestamp is injected into every call — the clock is
//!   never read from within the logic.
//!
//! ## OSS boundary (Layer A)
//! The facade registers only generic **mock skills** ([`crate::skills`]) —
//! no real providers, real identities, keys, or personal paths.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use familyclaw_core::time::Timestamp;

use crate::dispatch_outbox::{
    DispatchLookup, DispatchOutboxStore, DispatchedOutcome, InMemoryDispatchOutbox,
    JournalDispatchOutbox,
};
use crate::error::{ActionError, Result};
use crate::executor::ActionExecutor;
use crate::ids::{ActionTaskId, ApprovalId, SkillId};
use crate::mcp::McpToolDescriptor;
use crate::pending_store::{
    DangerousToolRateLimiter, InMemoryPendingStore, PendingApprovalStore, PendingRecord,
};
use crate::policy::{ActionRisk, SkillPermission};
use crate::proof::ProofBundle;
use crate::skills::{
    DiscordThreadSummaryMock, EmailTriageLive, EmailTriageMock, FilePatchApply,
    FileWriteAllowlisted, FileWriteConfig, FsReadAllowlisted, FsReadConfig, GithubIssueDraftMock,
    Pipeline, ResearchSkill, ScheduleTaskSkill, ShellExec, ShellExecConfig, Skill, WebFetchSkill,
    WebSearchSkill,
};
use crate::task::{ActionTask, DurableTaskQueue, TaskQueue, TaskStatus};

/// Module readiness flag — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside the other modules.
pub(crate) const SCAFFOLDED: bool = true;

/// Default TTL for an approval request when the operator grants approval
/// (`submit-task` leaves the task waiting; the approval is valid for this long).
const DEFAULT_APPROVAL_TTL_MINUTES: i64 = 1440;

fn approval_ttl_minutes() -> i64 {
    std::env::var("FAMILYCLAW_APPROVAL_TTL_MINUTES")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_APPROVAL_TTL_MINUTES)
}

/// Default length in seconds (1 hour) of the **sliding window** for the
/// per-being rate limiter on dangerous (approval-requiring) tool calls.
///
/// Together with [`DEFAULT_DANGEROUS_TOOL_LIMIT`] this deliberately forms a
/// **permissive default**: in a human-in-the-loop setting a single being does
/// not, in practice, submit hundreds of approval-requiring actions per hour,
/// so the default doesn't interfere with normal use but does cut off a clear
/// flood.
const DEFAULT_DANGEROUS_TOOL_WINDOW_SECS: i64 = 3_600;

/// **Default cap** for the per-being rate limiter on dangerous tool calls,
/// within one window ([`DEFAULT_DANGEROUS_TOOL_WINDOW_SECS`]).
///
/// A permissive default (256 approval-requiring actions per being per hour):
/// comfortably above a normal human-in-the-loop pace, but still bounds a
/// single being's ability to flood the approval queue. The operator can
/// tighten this ([`ActionRuntime::with_rate_limiter`]).
const DEFAULT_DANGEROUS_TOOL_LIMIT: usize = 256;

/// Generic default being id used for rate-limit accounting when the caller
/// does not provide an explicit being ([`ActionRuntime::submit_task`]).
///
/// Deliberately neutral (**not** a family member's name): all dangerous
/// actions submitted anonymously through the same runtime share this quota.
/// Provide the real being via [`ActionRuntime::submit_task_as`] when multiple
/// beings share the same runtime and each should get its own quota.
const DEFAULT_BEING_ID: &str = "operator";

/// Condensed description of a single skill, for operator listing.
///
/// Contains only public, secret-free fields — id, name, version, and risk
/// class — so the output can be shown directly to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    /// The skill's id in the registry.
    pub id: SkillId,
    /// Human-readable name.
    pub name: String,
    /// Version string.
    pub version: String,
    /// The action's risk class (drives the approval requirement).
    pub risk: ActionRisk,
    /// Whether this skill requires human approval before execution.
    pub requires_approval: bool,
}

/// Outcome of a `submit-task` operation, for the operator.
///
/// Reports the id of the submitted task, the task's status after the
/// pipeline ran, and — if the task stopped to wait for human approval — the
/// id of the granted approval that can be used to resume execution
/// (`approve`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitOutcome {
    /// Id of the submitted task.
    pub task_id: ActionTaskId,
    /// The task's status after the first pipeline run.
    pub status: TaskStatus,
    /// Id of the approval that can be used to resume execution, if the task
    /// was left waiting for approval (`None` if the task already ran to
    /// completion).
    pub pending_approval: Option<ApprovalId>,
}

impl SubmitOutcome {
    /// Whether the task was left waiting for human approval.
    #[must_use]
    pub const fn awaiting_approval(&self) -> bool {
        self.pending_approval.is_some()
    }
}

/// Summary of a single pending approval, for display to the operator.
///
/// Secret-free: refers only by id to what the approval concerns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// The approval's id (`approve <id>` resumes execution).
    pub approval_id: ApprovalId,
    /// The task the approval concerns.
    pub task_id: ActionTaskId,
}

/// Facade for the action runtime: a thin operator surface over the whole
/// pipeline.
///
/// Owns the pipeline ([`Pipeline`]), the skill executors, the proofs
/// produced, and the pending approvals. The operator tool only calls this
/// type's public methods and never touches the pipeline's internals.
///
/// A timestamp is injected into every call, so behavior is deterministic and
/// testable.
///
/// [`Debug`] is implemented by hand: the executors ([`ActionExecutor`] trait
/// objects) don't implement [`Debug`], so only their count is printed.
///
/// ## Storage of pending approvals
/// Pending approvals no longer live in a plain `HashMap` but behind the
/// [`PendingApprovalStore`] trait (the internal `pending` field). The
/// default is [`InMemoryPendingStore`] (same behavior as before), but the
/// operator can swap in the crash-resistant
/// [`crate::pending_store::JournalPendingStore`]
/// ([`ActionRuntime::with_pending_store`]), so that a crash between
/// `submit-task` and `approve` **no longer** loses the pending approval.
pub struct ActionRuntime {
    /// The whole action stack's pipeline (registry + queue + ledger + audit).
    pipeline: Pipeline,
    /// Skill id → executor, for execution.
    executors: HashMap<SkillId, Arc<dyn ActionExecutor>>,
    /// Task id → the redacted proof bundle produced.
    proofs: HashMap<ActionTaskId, ProofBundle>,
    /// Storage surface for pending approvals (in-memory by default,
    /// swappable for a crash-resistant one).
    pending: Box<dyn PendingApprovalStore>,
    /// **Crash-resistant task queue** (optional). When set
    /// ([`ActionRuntime::with_durable_stores`]), every task snapshot produced
    /// by `submit-task` and `approve` is mirrored into this JSONL log, and on
    /// restart the pipeline's queue is reconstructed from it — so that a task
    /// awaiting approval is still `approve`-eligible even if the process
    /// crashed between `submit-task` and `approve`.
    /// `None` → an in-memory queue (does not survive a crash), the default.
    durable_queue: Option<DurableTaskQueue>,
    /// **Per-being rate limit for dangerous (approval-requiring) tool
    /// calls.** Checked in `submit-task` **before** granting approval: if the
    /// being has already used up its quota within the sliding window,
    /// `submit-task` rejects fail-closed ([`ActionError::PolicyDenied`])
    /// without granting approval and without leaving the task pending.
    ///
    /// The capacity cap ([`crate::pending_store::PendingCapacity`]) is
    /// **global** (the whole queue); this limiter adds a **per-being** cap on
    /// top of that, so that a single being can't fill up the queue alone.
    /// Auto-run tasks (read / local write) are not rate-limited — only ones
    /// that would be left waiting for human approval.
    ///
    /// The default is **permissive**
    /// ([`DEFAULT_DANGEROUS_TOOL_WINDOW_SECS`] /
    /// [`DEFAULT_DANGEROUS_TOOL_LIMIT`]); the operator can tighten it via
    /// [`ActionRuntime::with_rate_limiter`].
    rate_limiter: DangerousToolRateLimiter,
    /// **Default being id** for rate-limit accounting when
    /// [`ActionRuntime::submit_task`] is called without an explicit being.
    ///
    /// The default is the generic [`DEFAULT_BEING_ID`] (not a family
    /// member's name). Use [`ActionRuntime::submit_task_as`] to provide a
    /// being per call, or [`ActionRuntime::with_being_id`] to set this
    /// runtime's default being.
    being_id: String,
    /// **Dispatch idempotency outbox** (the cornerstone of the at-most-once
    /// boundary: prevents double-dispatch across a crash, NOT universal
    /// exactly-once completion).
    ///
    /// [`ActionRuntime::submit_task_idempotent`] attaches a caller-derived
    /// stable key to every dispatch and records the dispatch in this outbox
    /// in two phases (intent before the side effect, committed after it).
    /// When the same key is seen again (replay/restart), the already
    /// committed dispatch is returned **value-identical without re-running
    /// the side effect** — regardless of where the agent layer's own
    /// journal-append window happened to land during the crash.
    ///
    /// The default is [`InMemoryDispatchOutbox`] (does not survive a crash,
    /// same behavior as before the outbox existed); for crash resistance,
    /// provide [`crate::dispatch_outbox::JournalDispatchOutbox`]
    /// ([`ActionRuntime::with_dispatch_outbox`]).
    dispatch_outbox: Box<dyn DispatchOutboxStore>,
}

impl Default for ActionRuntime {
    /// Default: an empty runtime whose pending approvals live in the
    /// in-memory surface ([`InMemoryPendingStore`]).
    fn default() -> Self {
        Self {
            pipeline: Pipeline::default(),
            executors: HashMap::new(),
            proofs: HashMap::new(),
            pending: Box::new(InMemoryPendingStore::new()),
            durable_queue: None,
            rate_limiter: DangerousToolRateLimiter::new(
                DEFAULT_DANGEROUS_TOOL_WINDOW_SECS,
                DEFAULT_DANGEROUS_TOOL_LIMIT,
            ),
            being_id: DEFAULT_BEING_ID.to_string(),
            dispatch_outbox: Box::new(InMemoryDispatchOutbox::new()),
        }
    }
}

impl std::fmt::Debug for ActionRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionRuntime")
            .field("pipeline", &self.pipeline)
            .field("executor_count", &self.executors.len())
            .field("proofs", &self.proofs.len())
            .field("pending_count", &self.pending.len().unwrap_or(0))
            .field("durable_queue", &self.durable_queue)
            .field("rate_limiter", &self.rate_limiter)
            .field("being_id", &self.being_id)
            .field("dispatch_outbox", &self.dispatch_outbox)
            .finish()
    }
}

impl ActionRuntime {
    /// Creates a new empty runtime with no registered skills.
    ///
    /// Pending approvals live by default in the in-memory surface
    /// ([`InMemoryPendingStore`]) — use [`ActionRuntime::with_pending_store`]
    /// for crash-resistant storage.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty runtime with the given **pending approval storage
    /// surface**.
    ///
    /// This is the hook for crash resistance: provide
    /// [`crate::pending_store::JournalPendingStore`], and an action granted
    /// but not yet approved by `submit-task` **survives a process crash**
    /// and is still [`ActionRuntime::approve`]-eligible after a restart. The
    /// default storage ([`ActionRuntime::new`]) is in-memory and does not
    /// survive a crash.
    #[must_use]
    pub fn with_pending_store(pending: Box<dyn PendingApprovalStore>) -> Self {
        Self {
            pending,
            ..Self::default()
        }
    }

    /// Swaps the runtime's **rate limiter for dangerous tool calls**
    /// (per-being, sliding window) for the given limiter and returns itself
    /// (builder style).
    ///
    /// This is the operator's hook to tighten (or loosen) the permissive
    /// default (`DEFAULT_DANGEROUS_TOOL_WINDOW_SECS` /
    /// `DEFAULT_DANGEROUS_TOOL_LIMIT`). The limiter is checked in
    /// `submit-task` **before** granting approval, only for tasks that would
    /// be left waiting for human approval — auto-run tasks (read / local
    /// write) are not rate-limited.
    ///
    /// ```
    /// # use familyclaw_actions::ActionRuntime;
    /// # use familyclaw_actions::pending_store::DangerousToolRateLimiter;
    /// // At most 3 approval-requiring actions per being per 60 s.
    /// let runtime = ActionRuntime::new()
    ///     .with_rate_limiter(DangerousToolRateLimiter::new(60, 3));
    /// let _ = runtime;
    /// ```
    #[must_use]
    pub fn with_rate_limiter(mut self, rate_limiter: DangerousToolRateLimiter) -> Self {
        self.rate_limiter = rate_limiter;
        self
    }

    /// Sets the runtime's **default being id** for rate-limit accounting and
    /// returns itself (builder style).
    ///
    /// This being is used when [`ActionRuntime::submit_task`] is called
    /// without an explicit being. Use a generic, **non-personal** id (e.g.
    /// `"agent-a"` / `"operator"`). The per-call being can be given directly
    /// via [`ActionRuntime::submit_task_as`] without this setting.
    #[must_use]
    pub fn with_being_id(mut self, being_id: impl Into<String>) -> Self {
        self.being_id = being_id.into();
        self
    }

    /// Creates a runtime with **fully crash-resistant** suspend/resume state:
    /// a crash-resistant pending-approvals surface, a crash-resistant task
    /// queue, **and** a crash-resistant dispatch outbox — all three
    /// reconstructed from the given durable files.
    ///
    /// This is the actions-side crash resistance for the suspend/resume
    /// bridge (roadmap §6): [`ActionRuntime::with_pending_store`] alone
    /// preserves the pending **approval**, but `approve` also needs the task
    /// (payload + state) in the pipeline's queue and the approval itself in
    /// the ledger. In addition, `submit_task`'s / `approve`'s at-most-once
    /// guarantee (prevents double-dispatch across a crash) requires a
    /// crash-resistant **dispatch outbox**. All of these are lost if the
    /// process crashes, unless they are persisted. This constructor wires up
    /// **all three crash-resistant surfaces at once**:
    ///
    /// 1. builds a crash-resistant **pending surface** from the given path
    ///    ([`crate::pending_store::JournalPendingStore`]),
    /// 2. reconstructs the **task queue** from the durable queue
    ///    ([`DurableTaskQueue::reload`] → [`TaskQueue::from_map`]),
    /// 3. opens a crash-resistant **dispatch outbox** from the given path
    ///    ([`JournalDispatchOutbox::open`]) — already committed dispatches
    ///    are reconstructed immediately, so at-most-once holds across a
    ///    restart,
    /// 4. **restores to the ledger** every pending approval from the durable
    ///    surface ([`crate::pending_store::PendingRecord::approval`]), so
    ///    that `approve` can consume it with the same payload binding,
    /// 5. mirrors every task snapshot going forward into the durable queue,
    ///    so a restart finds it.
    ///
    /// Skills are registered normally after this
    /// ([`ActionRuntime::register_skill`] /
    /// [`ActionRuntime::register_default_skills`]); they are pure code and
    /// need no persistence.
    ///
    /// # The dispatch outbox is now crash-resistant BY DEFAULT (no longer a trap)
    /// Previously this constructor left the dispatch outbox at its in-memory
    /// default ([`InMemoryDispatchOutbox`]), so a caller who did NOT
    /// separately chain [`ActionRuntime::with_dispatch_outbox`] silently got
    /// crash resistance turned OFF for dispatch — exactly the at-most-once
    /// property the durable state is meant to provide. Now the constructor
    /// opens a [`JournalDispatchOutbox`] directly from the
    /// `dispatch_outbox_path` path, so **all three surfaces are
    /// crash-resistant without a separate chain call**. The caller should
    /// provide separate paths (e.g.
    /// `<data_dir>/{pending_approvals,action_tasks,dispatch_outbox}.jsonl`),
    /// so the logs don't mix.
    ///
    /// [`ActionRuntime::with_dispatch_outbox`] remains available for special
    /// cases (e.g. an outbox wrapped with a crash hook in red-team tests): it
    /// can still **replace** the default journal outbox opened here. If it
    /// is not chained, the default is already crash-resistant.
    ///
    /// ```no_run
    /// # use familyclaw_actions::ActionRuntime;
    /// # async fn wire(dir: &std::path::Path) -> familyclaw_actions::error::Result<()> {
    /// let mut rt = ActionRuntime::with_durable_stores(
    ///     dir.join("pending_approvals.jsonl"),
    ///     dir.join("action_tasks.jsonl"),
    ///     dir.join("dispatch_outbox.jsonl"),
    /// )
    /// .await?; // the dispatch outbox is already journal-resistant — no need to chain with_dispatch_outbox
    /// rt.register_default_skills()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// - [`ActionError::Proof`] if opening/reading the pending, task, or dispatch-outbox journal
    ///   fails.
    pub async fn with_durable_stores(
        pending_path: impl AsRef<std::path::Path>,
        task_queue_path: impl Into<std::path::PathBuf>,
        dispatch_outbox_path: impl AsRef<std::path::Path>,
    ) -> Result<Self> {
        let pending: Box<dyn PendingApprovalStore> = Box::new(
            crate::pending_store::JournalPendingStore::open(pending_path)?,
        );
        let durable_queue = DurableTaskQueue::new(task_queue_path);

        // The crash-resistant dispatch outbox is opened DIRECTLY here, so durable
        // state is durable for ALL THREE surfaces (pending + task + dispatch) and
        // the caller doesn't have to remember to chain with_dispatch_outbox (formerly a trap).
        let dispatch_outbox: Box<dyn DispatchOutboxStore> =
            Box::new(JournalDispatchOutbox::open(dispatch_outbox_path)?);

        // Reconstruct the task queue from disk → pipeline with the restored queue.
        let task_map = durable_queue.reload().await?;
        let queue = TaskQueue::from_map(task_map);
        let mut pipeline = Pipeline::with_restored_queue(queue);

        // Restore pending approvals to the ledger, so `approve` can find them.
        for record in pending.list()? {
            pipeline.reinstate_approval(record.approval);
        }

        Ok(Self {
            pipeline,
            executors: HashMap::new(),
            proofs: HashMap::new(),
            pending,
            durable_queue: Some(durable_queue),
            rate_limiter: DangerousToolRateLimiter::new(
                DEFAULT_DANGEROUS_TOOL_WINDOW_SECS,
                DEFAULT_DANGEROUS_TOOL_LIMIT,
            ),
            being_id: DEFAULT_BEING_ID.to_string(),
            dispatch_outbox,
        })
    }

    /// Swaps the runtime's **dispatch idempotency outbox** for the given
    /// implementation and returns itself (builder style).
    ///
    /// This is the wiring point for the at-most-once guarantee (prevents
    /// double-dispatch, NOT universal exactly-once completion). The default
    /// ([`ActionRuntime::new`]) is in-memory ([`InMemoryDispatchOutbox`]) and
    /// does not survive a crash; provide
    /// [`crate::dispatch_outbox::JournalDispatchOutbox`] to get the
    /// guarantee: `submit_task`'s side effect runs **at most once** across a
    /// SIGKILL crash (never twice), and an already committed dispatch is
    /// returned value-identical.
    ///
    /// Note: [`ActionRuntime::with_durable_stores`] already opens a
    /// crash-resistant journal outbox by default, so on top of that this is
    /// only needed when you want to **replace** the default in a special
    /// case (e.g. an outbox wrapped with a crash hook in red-team tests). In
    /// in-memory mode ([`ActionRuntime::new`] /
    /// [`ActionRuntime::with_default_skills`]) this is the only way to wire
    /// up a crash-resistant outbox.
    ///
    /// ```
    /// # use familyclaw_actions::ActionRuntime;
    /// # use familyclaw_actions::dispatch_outbox::InMemoryDispatchOutbox;
    /// let runtime = ActionRuntime::new()
    ///     .with_dispatch_outbox(Box::new(InMemoryDispatchOutbox::new()));
    /// let _ = runtime;
    /// ```
    #[must_use]
    pub fn with_dispatch_outbox(mut self, outbox: Box<dyn DispatchOutboxStore>) -> Self {
        self.dispatch_outbox = outbox;
        self
    }

    /// Returns the **kind tag** of the connected dispatch outbox
    /// (`"in-memory"` or `"journal"`).
    ///
    /// This is a secret-free check hook for the assembler and tests: it lets
    /// you confirm that a persistent configuration got the crash-resistant
    /// (`"journal"`) outbox instead of the default in-memory (`"in-memory"`)
    /// one, without exposing internal state or the file path. The value
    /// delegates directly to [`DispatchOutboxStore::kind`].
    #[must_use]
    pub fn dispatch_outbox_kind(&self) -> &'static str {
        self.dispatch_outbox.kind()
    }

    /// Returns the **kind tag** of the connected **pending-approvals
    /// surface** (`"in-memory"` or `"journal"`).
    ///
    /// This is a secret-free check hook for the assembler and tests — same
    /// purpose as [`ActionRuntime::dispatch_outbox_kind`]: it lets you
    /// confirm that a persistent configuration got the crash-resistant
    /// (`"journal"`) pending surface instead of the default in-memory
    /// (`"in-memory"`) one, without exposing internal state or the file
    /// path. The value delegates directly to
    /// [`PendingApprovalStore::kind`].
    #[must_use]
    pub fn pending_store_kind(&self) -> &'static str {
        self.pending.kind()
    }

    /// Snapshots the task's current state into the crash-resistant queue, if
    /// one is set ([`ActionRuntime::with_durable_stores`]). A no-op in
    /// in-memory mode.
    ///
    /// Best-effort: a snapshot failure **does not** fail the action itself
    /// (it already succeeded in the pipeline), but it does jeopardize crash
    /// resistance. Returns `Ok(())` in no-op mode too; the caller may ignore
    /// the error or propagate it. The actions crate does not depend on a
    /// logging library, so the error is returned rather than logged here.
    ///
    /// # Errors
    /// [`ActionError::Proof`] if writing to the durable queue fails.
    async fn snapshot_task_if_durable(&self, task_id: ActionTaskId) -> Result<()> {
        let Some(durable) = self.durable_queue.as_ref() else {
            return Ok(());
        };
        // Read the task's current state from the pipeline's queue and append it to the durable log.
        if let Some(task) = self.pipeline.queue().get(task_id).await {
            durable.append(&task).await?;
        }
        Ok(())
    }

    /// Creates a runtime with all five Layer A skills already registered.
    ///
    /// This is the operator's default configuration: [`EmailTriageMock`],
    /// [`GithubIssueDraftMock`], [`DiscordThreadSummaryMock`], [`FilePatchMock`]
    /// and the flagship [`FsReadAllowlisted`].
    ///
    /// [`FsReadAllowlisted`] is registered with an **empty allowlist**
    /// (fail-closed): it is listed and published as an MCP tool, but rejects
    /// all paths until the operator provides an allowlist
    /// ([`FsReadAllowlisted::with_config`]) and registers it via
    /// [`ActionRuntime::register_skill`]. This way the default configuration
    /// hard-codes no path at all and stays generic.
    ///
    /// # Errors
    /// Returns the manifest validation or duplicate-registration error, if
    /// one of the built-in skills is invalid (should not happen).
    pub fn with_default_skills() -> Result<Self> {
        let mut runtime = Self::new();
        runtime.register_default_skills()?;
        Ok(runtime)
    }

    /// Registers the five Layer A default skills into an **existing**
    /// runtime (`&mut self`).
    ///
    /// This is the shared core of [`ActionRuntime::with_default_skills`]: an
    /// assembler that builds a runtime with crash-resistant surfaces
    /// ([`ActionRuntime::with_durable_stores`]) can register the same
    /// default skills without duplicating the 5-skill list. The skills are
    /// pure code and need no persistence; [`FsReadAllowlisted`] is
    /// registered with an empty allowlist (fail-closed), as in
    /// [`ActionRuntime::with_default_skills`].
    ///
    /// ## Third-party skills and the wasmtime sandbox
    ///
    /// These default skills are built-in Layer A references (plain Rust, no
    /// signature). **Third-party skills** should be run in
    /// [`familyclaw-sandbox`]'s Wasmtime sandbox (fuel cap, host-import
    /// block, capability grants) — see
    /// [`docs/SECURITY_MODEL.md`](../../docs/SECURITY_MODEL.md) layer 6.
    ///
    /// The runtime assembler [`build_family`] (`familyclaw-runtime`) wires
    /// the sandbox into the agent when `FAMILYCLAW_SANDBOX_SKILLS=1` and
    /// `familyclaw-sandbox::default_sandbox()` is available. External
    /// manifests additionally require an Ed25519 signature
    /// (`FAMILYCLAW_SKILL_REGISTRY`).
    ///
    /// # Errors
    /// Returns the manifest validation or duplicate-registration error, if
    /// one of the built-in skills is invalid (should not happen).
    pub fn register_default_skills(&mut self) -> Result<()> {
        self.register_default_skills_with_fs_read(None)
    }

    /// Registers the default skills, but lets the caller **configure the
    /// allowlist of the flagship research skill** [`FsReadAllowlisted`] (a
    /// superset of [`ActionRuntime::register_default_skills`]).
    ///
    /// [`FsReadAllowlisted`] is shared across the whole platform with a
    /// **fixed id** ([`FsReadAllowlisted::skill_id`]), so it cannot be
    /// registered twice (duplicate rejection). That's why its allowlist must
    /// be provided AT registration time — not later via
    /// [`ActionRuntime::register_skill`] with a second copy.
    ///
    /// - `fs_read_config = None` → empty allowlist (fail-closed, default):
    ///   the skill is listed and published as a tool, but rejects all paths.
    /// - `fs_read_config = Some(cfg)` → the skill is registered with the
    ///   given allowlist, so a read falling under it runs
    ///   **automatically** (the skill is [`ActionRisk::ReadOnly`] +
    ///   `AutoIfReadOnly`) without approval.
    ///
    /// The allowlist (allowed/trusted roots) is **Layer B data**: this
    /// facade hard-codes no path — the caller (gateway/runtime) reads it
    /// from the environment and provides it as [`FsReadConfig`]. This way
    /// Layer A stays generic.
    ///
    /// # Errors
    /// Returns the manifest validation or duplicate-registration error, if
    /// one of the built-in skills is invalid (should not happen).
    pub fn register_default_skills_with_fs_read(
        &mut self,
        fs_read_config: Option<FsReadConfig>,
    ) -> Result<()> {
        self.register_default_skills_with_configs(fs_read_config, None, None)
    }

    /// Registers the default skills and lets the caller **configure the
    /// allowlists of both the file-read** ([`FsReadAllowlisted`]) **and
    /// file-write** ([`FileWriteAllowlisted`]) **skills**.
    ///
    /// Both skills have a fixed skill id, so their allowlist must be
    /// provided AT registration time (not later — duplicate rejection).
    /// Both follow the same fail-closed default:
    ///
    /// - `config = None` → empty allowlist (rejects all paths). The skill is
    ///   listed and published as a tool, but does nothing until the
    ///   allowlist is provided.
    /// - `config = Some(cfg)` → the skill is registered with the given
    ///   allowlist, so an operation falling under it works. `fs_read`
    ///   (`ReadOnly`) runs automatically; `file_write` (`WriteLocal`) still
    ///   always stops for approval
    ///   (`ApprovalPolicy::AlwaysRequireApproval`) — the allowlist only
    ///   allows the write to be possible at all after that.
    ///
    /// The allowlists are **Layer B data**: this facade hard-codes no path —
    /// the caller (gateway/runtime) reads them from the environment.
    ///
    /// # Errors
    /// Returns the manifest validation or duplicate-registration error, if
    /// one of the built-in skills is invalid (should not happen).
    pub fn register_default_skills_with_configs(
        &mut self,
        fs_read_config: Option<FsReadConfig>,
        file_write_config: Option<FileWriteConfig>,
        shell_exec_config: Option<ShellExecConfig>,
    ) -> Result<()> {
        self.register_skill(EmailTriageMock::new())?;
        // Optional live email triage: registered only when FAMILYCLAW_EMAIL_TRIAGE_URL
        // is set to a public HTTPS endpoint (SSRF-guarded). Unset → mock only.
        // Misconfigured/empty URL fails closed (registration error).
        if let Some(live) = EmailTriageLive::try_from_env()? {
            self.register_skill(live)?;
        }
        // github_issue_draft is a real, credential-free skill: it produces a draft
        // and can save it to an allowlisted artifact (no network call). Same
        // Layer B allowlist as file_write; the write stays behind approval
        // (WriteExternal + RequireApproval).
        let issue_draft = match file_write_config.clone() {
            Some(config) => GithubIssueDraftMock::with_config(config),
            None => GithubIssueDraftMock::new(),
        };
        self.register_skill(issue_draft)?;
        self.register_skill(DiscordThreadSummaryMock::new())?;
        let file_patch = match file_write_config.clone() {
            Some(config) => FilePatchApply::with_config(config),
            None => FilePatchApply::new(),
        };
        self.register_skill(file_patch)?;
        // Flagship research skill: empty allowlist (fail-closed) by default, or
        // a caller-provided Layer B allowlist, in which case reads actually research.
        let fs_read = match fs_read_config {
            Some(config) => FsReadAllowlisted::with_config(config),
            None => FsReadAllowlisted::new(),
        };
        self.register_skill(fs_read)?;
        // 2026-06-25: a real research skill (read-only web-fetch, SSRF-guarded).
        self.register_skill(WebFetchSkill::new())?;
        // 2026-07-03: functionality-parity executors (closes a real agent
        // capability gap: web search, disk write, research).
        // web_search + research are read-only (AutoIfReadOnly), so they run
        // without approval.
        self.register_skill(WebSearchSkill::new())?;
        self.register_skill(ResearchSkill::new())?;
        // file_write is WriteLocal + AlwaysRequireApproval. Empty allowlist
        // (fail-closed) by default, or a caller-provided Layer B allowlist,
        // in which case a write falling under it is possible (but still behind approval).
        let file_write = match file_write_config {
            Some(config) => FileWriteAllowlisted::with_config(config),
            None => FileWriteAllowlisted::new(),
        };
        self.register_skill(file_write)?;
        self.register_skill(ScheduleTaskSkill::new())?;
        // shell_exec: Hermes-style hard block + manual/smart/off modes.
        let shell_exec = match shell_exec_config {
            Some(config) => ShellExec::with_config(config),
            None => ShellExec::new(),
        };
        self.register_skill(shell_exec)?;
        Ok(())
    }

    /// Registers the skill into both the pipeline's registry (manifest) and
    /// the facade's executor map (execution).
    ///
    /// # Errors
    /// Returns the manifest validation or duplicate-registration error
    /// ([`Pipeline::register_skill`]).
    pub fn register_skill<S>(&mut self, skill: S) -> Result<()>
    where
        S: Skill + 'static,
    {
        self.pipeline.register_skill(&skill)?;
        let id = skill.manifest().id;
        self.executors.insert(id, Arc::new(skill));
        Ok(())
    }

    /// Lists the registered skills in condensed form (id, name, version,
    /// risk class, approval requirement). Order is stabilized by name.
    ///
    /// The output never contains secrets — the manifest was already
    /// validated to be secret-free at registration time.
    #[must_use]
    pub fn list_skills(&self) -> Vec<SkillSummary> {
        let mut out: Vec<SkillSummary> = self
            .pipeline
            .registry()
            .list()
            .into_iter()
            .map(|m| SkillSummary {
                id: m.id,
                name: m.name.clone(),
                version: m.version.clone(),
                risk: m.risk,
                requires_approval: crate::policy::required_approval(m.risk, m.approval_policy)
                    .requires_approval(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Returns the **raw** MCP tool descriptors for each registered
    /// skill — exactly the data the agent needs to build the tool
    /// definitions offered to the LLM.
    ///
    /// ## Layer responsibility (intentional)
    /// This facade **does not** know about the `familyclaw-agent` layer and
    /// does not build the final LLM `ToolDefinition` value. It only exposes
    /// [`McpToolDescriptor`] descriptors (name, description, input schema,
    /// required permission, trust level); the agent assembles its own shape
    /// from those and routes the tool call back to the skill via
    /// [`ActionRuntime::map_name_to_skill`]. This way the dependency runs
    /// only in the direction agent → actions, never back.
    ///
    /// ## Derivation from the manifest
    /// Each descriptor is derived from the skill's validated
    /// [`crate::manifest::SkillManifest`] manifest:
    /// - `name` ← the manifest's name (the same one
    ///   [`ActionRuntime::map_name_to_skill`] uses to route the call back),
    /// - `description` ← the manifest's description,
    /// - `input_schema` ← the manifest's machine-readable input schema
    ///   ([`crate::manifest::SkillManifest::input_schema`]); the root is
    ///   always a JSON object (validation guarantees this), so it fits the
    ///   LLM tool's `parameters` field as-is,
    /// - `required_permission` ← the **strictest** single permission among
    ///   the manifest's permissions (the most severe in terms of side
    ///   effects); if the skill requires no permissions, the default is
    ///   [`SkillPermission::ReadFiles`] (the least permissive),
    /// - `trusted` ← always `false`: output from a tool derived from a skill
    ///   is by default treated as untrusted, as elsewhere in the crate.
    ///
    /// Order is stabilized by name (same as
    /// [`ActionRuntime::list_skills`]), ties broken by id, so the output is
    /// reproducible.
    ///
    /// The output never contains secrets — the manifest was already
    /// validated to be secret-free at registration time.
    #[must_use]
    pub fn tool_definitions(&self) -> Vec<McpToolDescriptor> {
        let mut out: Vec<(SkillId, McpToolDescriptor)> = self
            .pipeline
            .registry()
            .list()
            .into_iter()
            .map(|m| {
                let descriptor = McpToolDescriptor::new(
                    m.name.clone(),
                    m.description.clone(),
                    m.input_schema.clone(),
                    strictest_permission(&m.permissions),
                );
                (m.id, descriptor)
            })
            .collect();
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name).then_with(|| a.0.cmp(&b.0)));
        out.into_iter().map(|(_, d)| d).collect()
    }

    /// Routes a tool name back to the corresponding skill id.
    ///
    /// The agent calls this when the LLM picks a tool by name (the same
    /// name [`ActionRuntime::tool_definitions`] published): the name yields
    /// the [`SkillId`], which lets the task be forwarded to
    /// [`ActionRuntime::submit_task`]. Returns `None` if no registered skill
    /// has this name.
    ///
    /// The lookup is an exact string comparison against the manifest name.
    /// If two skills shared the same name, the smallest id is returned in a
    /// stabilized way, so routing is deterministic (in practice names are
    /// unique).
    #[must_use]
    pub fn map_name_to_skill(&self, name: &str) -> Option<SkillId> {
        self.pipeline
            .registry()
            .list()
            .into_iter()
            .filter(|m| m.name == name)
            .map(|m| m.id)
            .min()
    }

    /// Submits a task to the given skill and runs the pipeline under
    /// **this runtime's default being** ([`ActionRuntime::with_being_id`],
    /// default `DEFAULT_BEING_ID`) for rate-limit accounting.
    ///
    /// If the skill's risk class permits auto-run, the pipeline runs the
    /// action to completion and the proof is stored. If policy requires
    /// human approval, the task is left in [`TaskStatus::NeedsApproval`]
    /// state and the facade **grants** a payload-bound approval whose id is
    /// returned ([`SubmitOutcome::pending_approval`]); execution can be
    /// resumed with an [`ActionRuntime::approve`] call.
    ///
    /// When multiple beings share the same runtime and each should get its
    /// **own** rate-limit quota, use [`ActionRuntime::submit_task_as`] and
    /// provide the being explicitly.
    ///
    /// # Errors
    /// - [`ActionError::UnknownSkill`] if the skill is not registered.
    /// - [`ActionError::PolicyDenied`] if the task would require approval
    ///   but the being has already used its dangerous-tool rate-limit
    ///   quota.
    /// - Pipeline queue, execution, or proof errors.
    pub async fn submit_task(
        &mut self,
        skill_id: SkillId,
        payload: Value,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        let being = self.being_id.clone();
        self.submit_task_as(&being, skill_id, payload, now).await
    }

    /// Like [`ActionRuntime::submit_task`], but submits the task under an
    /// **explicit being** (`being`) for rate-limit accounting.
    ///
    /// This is the point where the **per-being rate limit** for dangerous
    /// (approval-requiring) tool calls hooks into the approval path: if the
    /// pipeline decides the task should be left waiting for human approval,
    /// the facade first asks the limiter
    /// ([`DangerousToolRateLimiter::check_and_record`]) whether `being`
    /// still has room in the sliding window. If the quota is exhausted,
    /// approval is **not** granted and the task is not left pending — the
    /// call is rejected fail-closed ([`ActionError::PolicyDenied`]). This
    /// way a single being cannot flood the approval queue, even if the
    /// global capacity cap has not yet been reached.
    ///
    /// **Auto-run tasks** (read / local write, which don't require approval)
    /// are **not** rate-limited: they run to completion normally, since they
    /// don't grow the approval queue. The rate limit applies precisely and
    /// only to approval-requiring actions.
    ///
    /// # Errors
    /// - [`ActionError::UnknownSkill`] if the skill is not registered.
    /// - [`ActionError::PolicyDenied`] if the task would require approval
    ///   but `being` has already used its dangerous-tool rate-limit quota.
    /// - Pipeline queue, execution, or proof errors.
    pub async fn submit_task_as(
        &mut self,
        being: &str,
        skill_id: SkillId,
        payload: Value,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        let executor = self
            .executors
            .get(&skill_id)
            .ok_or_else(|| ActionError::UnknownSkill(skill_id.to_string()))?
            .clone();

        let task = ActionTask::new(skill_id, payload.clone(), now);
        let task_id = task.id;

        let outcome = self.pipeline.run(executor.as_ref(), task, now).await?;

        if let Some(proof) = outcome.proof {
            self.proofs.insert(task_id, proof);
        }

        let pending_approval = if outcome.awaiting_approval {
            // Per-being rate limit: checked BEFORE granting approval.
            // If the being has already exhausted its quota within the sliding
            // window, reject fail-closed — approval is NOT granted and the task
            // is not left pending. Auto-run tasks never reach this branch.
            self.rate_limiter.check_and_record(being, now)?;
            let approval = self.pipeline.grant_approval(
                outcome.action_id,
                &payload,
                now,
                Duration::minutes(approval_ttl_minutes()),
            )?;
            let approval_id = approval.id;
            // Redacted summary: only the skill name and ids — NO raw
            // payload. Stored to the crash-resistant surface if one is set.
            let summary = self.pending_summary(skill_id);
            let record = PendingRecord::new(task_id, approval, summary, now);
            self.pending.insert(record)?;
            // Crash resistance: persist the task's NeedsApproval snapshot,
            // so `approve` finds the task (payload + state) even across a restart.
            self.snapshot_task_if_durable(task_id).await?;
            Some(approval_id)
        } else {
            // The auto-run task ran to completion — snapshot to the durable queue if
            // set (Done state), so it doesn't linger as a NeedsApproval row on restart.
            self.snapshot_task_if_durable(task_id).await?;
            None
        };

        Ok(SubmitOutcome {
            task_id,
            status: outcome.status,
            pending_approval,
        })
    }

    /// Submits a task **idempotently**, guarded by a caller-derived stable
    /// key (`key`) — the cornerstone of the at-most-once guarantee (the side
    /// effect is dispatched **at most once**, never twice; this prevents
    /// double-dispatch across a crash, and is NOT a promise of universal
    /// exactly-once *completion*).
    ///
    /// This is the crash-resistant wrapper around
    /// [`ActionRuntime::submit_task_as`]. It closes the window between
    /// executing the side effect and journaling it: when the same key is
    /// seen again (agent-layer replay or process restart), the dispatch
    /// **does not re-run the side effect** but returns the prior outcome
    /// value-identical (same `task_id` / `ApprovalId`).
    ///
    /// ## Two-phase commit to the outbox
    /// 1. **lookup(key)** — if the key already exists:
    ///    - **committed** → return the stored outcome immediately, do NOT run
    ///      the side effect.
    ///    - **in-progress** (intent recorded, not committed) → the process
    ///      crashed mid-side-effect. The recovery policy is **explicit and
    ///      fail-closed** ([`ActionError::PolicyDenied`]): the call is NOT
    ///      re-run, because the side effect may have happened partially.
    ///    - **not-started** → proceed.
    /// 2. **`record_intent`** — record the intent to the outbox (fsync)
    ///    BEFORE the side effect.
    /// 3. run the side effect ([`ActionRuntime::submit_task_as`]).
    /// 4. **`record_committed`** — record the outcome to the outbox
    ///    afterward (fsync). Only this makes the dispatch replay-recoverable.
    ///
    /// A `submit_task` error is stored as a committed error row, so it too
    /// is returned identically without re-running the side effect.
    ///
    /// ## Guarantee boundary (honestly)
    /// Guaranteed across a process crash / SIGKILL when the outbox is
    /// crash-resistant ([`crate::dispatch_outbox::JournalDispatchOutbox`]).
    /// With the in-memory default outbox, the guarantee only covers replay
    /// within the same process (not a restart). The power-loss /
    /// directory-metadata-fsync guarantee is only as strong as the
    /// underlying filesystem — that is not over-promised here.
    ///
    /// # Errors
    /// - [`ActionError::PolicyDenied`] if the key was left in-progress from a
    ///   prior crash.
    /// - [`ActionError::ExecutionFailed`] if the stored (committed) dispatch
    ///   was an error (replay recovery).
    /// - [`ActionError::Proof`] if reading/writing the outbox fails.
    /// - [`ActionRuntime::submit_task_as`]'s errors on a fresh run.
    pub async fn submit_task_idempotent(
        &mut self,
        key: &str,
        being: &str,
        skill_id: SkillId,
        payload: Value,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        // 1) Idempotency check: has the key already been started/committed?
        match self.dispatch_outbox.lookup(key)? {
            DispatchLookup::Committed(outcome) => {
                // Already committed → return the value-identical outcome without
                // re-running the side effect. THIS is the branch that closes double-firing.
                return outcome.into_result();
            }
            DispatchLookup::InProgress => {
                // Intent recorded but not committed → crashed mid-side-effect.
                // Fail-closed: don't re-run (the side effect may be partial).
                return Err(ActionError::PolicyDenied(format!(
                    "lähetys '{key}' jäi kesken aiemmassa kaatumisessa (intent ilman \
                     committed) — ei ajeta uudelleen kaksoislaukaisun estämiseksi"
                )));
            }
            DispatchLookup::NotStarted => {}
        }

        // 2) Record the INTENT BEFORE the side effect (fsync with a crash-resistant outbox).
        self.dispatch_outbox.record_intent(key)?;

        // 3) Execute the side effect exactly once.
        let result = self.submit_task_as(being, skill_id, payload, now).await;

        // 4) Record the COMMIT after the side effect — success or error.
        //    The error case is stored as a committed error, so replay returns
        //    the same error without re-running the side effect (no double-dispatch
        //    from a partially completed submission).
        match &result {
            Ok(outcome) => {
                self.dispatch_outbox
                    .record_committed(key, &DispatchedOutcome::from_submit(outcome))?;
            }
            Err(e) => {
                self.dispatch_outbox
                    .record_committed(key, &DispatchedOutcome::from_error(e.to_string()))?;
            }
        }

        result
    }

    /// Derives an approval's **stable idempotency key** for the dispatch
    /// outbox.
    ///
    /// The key is deterministic and **persistent across a restart**:
    /// `ApprovalId` is in the crash-resistant storage surface, so the same
    /// approval always produces the same key. This is the mechanism by
    /// which [`ActionRuntime::approve`]'s side effect is dispatched **at
    /// most once** across a process crash.
    #[must_use]
    fn approval_dispatch_key(approval_id: ApprovalId) -> String {
        format!("approval-{approval_id}")
    }

    /// Consumes (marks used) a pending approval and runs the stalled task's
    /// execution to completion — **idempotently**, guarded by the dispatch
    /// outbox (the cornerstone of the at-most-once guarantee on the approval
    /// path).
    ///
    /// The approval is consumed against the task's stored payload
    /// (payload binding + single-use), so a changed payload cannot consume
    /// the approval. On success the resulting proof is stored for retrieval.
    ///
    /// ## Why the outbox is needed on this path too (prevents double-dispatch)
    /// A window remains **between** executing the side effect
    /// ([`Pipeline::run_after_approval`]) and the record that consumes it
    /// ([`PendingApprovalStore::remove`]): if the process is killed
    /// (SIGKILL) right there, the side effect has already happened but the
    /// approval is still `pending` on the crash-resistant surface → after a
    /// restart, the operator could **re-approve the same approval** and the
    /// side effect **would fire twice**. The outbox closes this: the side
    /// effect is wrapped in the idempotency of a stable key
    /// (`approval-{id}`) exactly as in
    /// [`ActionRuntime::submit_task_idempotent`], so a re-approval hits the
    /// outbox and does not re-run the side effect.
    ///
    /// ## Two-phase commit to the outbox
    /// 1. **lookup(key)** — if the key already exists:
    ///    - **committed** → return the stored outcome immediately, do NOT
    ///      re-run the side effect.
    ///    - **in-progress** (intent recorded, not committed) → the process
    ///      crashed mid-side-effect → **fail-closed**
    ///      ([`ActionError::PolicyDenied`]), do NOT re-run.
    ///    - **not-started** → proceed.
    /// 2. **`record_intent`** (fsync) BEFORE the side effect.
    /// 3. run the side effect ([`Pipeline::run_after_approval`]).
    /// 4. **`record_committed`** (fsync) after the side effect — only this
    ///    makes the dispatch replay-recoverable.
    ///
    /// `pending.remove` + the state snapshot follow after the commit, but
    /// are now protected by idempotency: a re-approval does not re-run the
    /// side effect.
    ///
    /// ## Guarantee boundary (honestly)
    /// This prevents double-dispatch / is an **at-most-once dispatch**
    /// across a crash (fail-closed in the intent-only window) — **NOT** a
    /// promise of universal exactly-once *completion*. The guarantee covers
    /// SIGKILL only with a crash-resistant outbox
    /// ([`crate::dispatch_outbox::JournalDispatchOutbox`]); with the
    /// in-memory default outbox, behavior is unchanged (only replay within
    /// the same process, not across a restart).
    ///
    /// # Errors
    /// - [`ActionError::ApprovalMissing`] if no approval is pending.
    /// - [`ActionError::UnknownSkill`] if the task's skill can no longer be
    ///   found.
    /// - [`ActionError::PolicyDenied`] if the approval was left in-progress
    ///   (intent-only) from a prior crash.
    /// - [`ActionError::ExecutionFailed`] if the stored (committed) dispatch
    ///   was an error (replay recovery).
    /// - [`ActionError::Proof`] if reading/writing the outbox fails.
    /// - Errors from consuming the approval or from the pipeline
    ///   ([`Pipeline::run_after_approval`]).
    pub async fn approve(
        &mut self,
        approval_id: ApprovalId,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        let entry = self
            .pending
            .get(approval_id)?
            .ok_or_else(|| ActionError::ApprovalMissing(approval_id.to_string()))?;

        // Stable idempotency key: stays the same across a restart (ApprovalId is
        // on the crash-resistant surface). Same outbox protocol as in
        // `submit_task_idempotent`.
        let key = Self::approval_dispatch_key(approval_id);

        // 1) Idempotency check BEFORE the side effect.
        match self.dispatch_outbox.lookup(&key)? {
            DispatchLookup::Committed(outcome) => {
                // Already committed → return the value-identical outcome without
                // re-running the side effect. THIS is the branch that closes double-firing
                // across a re-approval.
                return outcome.into_result();
            }
            DispatchLookup::InProgress => {
                // Intent recorded but not committed → crashed mid-side-effect.
                // Fail-closed: don't re-run (the side effect may be partial).
                return Err(ActionError::PolicyDenied(format!(
                    "hyväksynnän '{approval_id}' lähetys jäi kesken aiemmassa \
                     kaatumisessa (intent ilman committed) — ei ajeta uudelleen \
                     kaksoislaukaisun estämiseksi"
                )));
            }
            DispatchLookup::NotStarted => {}
        }

        let task = self
            .pipeline
            .queue()
            .get(entry.task_id)
            .await
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {} ei löydy", entry.task_id)))?;
        let executor = self
            .executors
            .get(&task.skill_id)
            .ok_or_else(|| ActionError::UnknownSkill(task.skill_id.to_string()))?
            .clone();

        // 2) Record the INTENT BEFORE the side effect (fsync with a crash-resistant outbox).
        self.dispatch_outbox.record_intent(&key)?;

        // 3) Execute the side effect (consuming the approval + running the pipeline) exactly once.
        let run_result = self
            .pipeline
            .run_after_approval(executor.as_ref(), entry.task_id, &entry.approval, now)
            .await;

        // 4) Record the COMMIT after the side effect — success or error.
        //    The error case is stored as a committed error, so a re-approval
        //    returns the same error without re-running the side effect.
        let outcome = match run_result {
            Ok(outcome) => {
                let submit = SubmitOutcome {
                    task_id: entry.task_id,
                    status: outcome.status,
                    pending_approval: None,
                };
                self.dispatch_outbox
                    .record_committed(&key, &DispatchedOutcome::from_submit(&submit))?;
                outcome
            }
            Err(e) => {
                self.dispatch_outbox
                    .record_committed(&key, &DispatchedOutcome::from_error(e.to_string()))?;
                return Err(e);
            }
        };

        // The approval is now consumed — remove it from pending (permanently, including on
        // the crash-resistant surface). Protected by idempotency: a re-approval
        // hits the committed branch above and never reaches here again.
        self.pending.remove(approval_id)?;
        // Crash resistance: persist the task's final (Done/Failed) state
        // to the durable queue, so a restart no longer sees it as a NeedsApproval row.
        self.snapshot_task_if_durable(entry.task_id).await?;

        if let Some(proof) = outcome.proof {
            self.proofs.insert(entry.task_id, proof);
        }

        Ok(SubmitOutcome {
            task_id: entry.task_id,
            status: outcome.status,
            pending_approval: None,
        })
    }

    /// Denies a pending approval — removes the pending record and cancels the task.
    ///
    /// # Errors
    /// [`ActionError::ApprovalMissing`] if no approval is pending.
    pub async fn deny_pending(&mut self, approval_id: ApprovalId, now: Timestamp) -> Result<()> {
        let entry = self
            .pending
            .get(approval_id)?
            .ok_or_else(|| ActionError::ApprovalMissing(approval_id.to_string()))?;
        self.pending.remove(approval_id)?;
        self.pipeline
            .queue()
            .transition(entry.task_id, TaskStatus::Cancelled, now)
            .await?;
        self.snapshot_task_if_durable(entry.task_id).await?;
        Ok(())
    }

    /// Returns the task's status by id; `None` if the task is not in the queue.
    pub async fn status(&self, task_id: ActionTaskId) -> Option<TaskStatus> {
        self.pipeline.queue().get(task_id).await.map(|t| t.status)
    }

    /// Returns the **redacted** proof bundle produced for the task; `None`
    /// if there is no proof (yet) (e.g. the task is still awaiting approval).
    ///
    /// The proof was already redacted in the pipeline — it never contains
    /// the raw payload or secrets.
    #[must_use]
    pub fn proof(&self, task_id: ActionTaskId) -> Option<&ProofBundle> {
        self.proofs.get(&task_id)
    }

    /// Lists pending approvals (secret-free summaries).
    ///
    /// Order is stabilized by approval id for reproducibility. If reading
    /// the storage surface fails (e.g. a disk error on a crash-resistant
    /// surface), an **empty list** is returned — the operator's listing
    /// never panics. Use [`ActionRuntime::try_pending_approvals`] if you
    /// want the error to propagate.
    #[must_use]
    pub fn pending_approvals(&self) -> Vec<PendingApproval> {
        self.try_pending_approvals().unwrap_or_default()
    }

    /// Like [`ActionRuntime::pending_approvals`], but propagates a storage
    /// surface read error instead of returning an empty list.
    ///
    /// # Errors
    /// A read error from the storage surface ([`PendingApprovalStore::list`])
    /// — in practice only on a crash-resistant surface, if the journal
    /// cannot be read.
    pub fn try_pending_approvals(&self) -> Result<Vec<PendingApproval>> {
        let mut out: Vec<PendingApproval> = self
            .pending
            .list()?
            .into_iter()
            .map(|record| PendingApproval {
                approval_id: record.approval_id(),
                task_id: record.task_id,
            })
            .collect();
        out.sort_by_key(|a| a.approval_id);
        Ok(out)
    }

    /// Evicts from the storage surface all pending approvals that are
    /// expired as of the given moment `now`, and returns the number evicted.
    ///
    /// Uses the same fail-closed expiry boundary as [`crate::approval`]
    /// (`now > expires_at`). The operator can call this periodically to
    /// keep the pending queue tidy; an expired approval can no longer be
    /// consumed.
    ///
    /// # Errors
    /// A storage surface error ([`PendingApprovalStore::evict_expired`]).
    pub fn evict_expired_approvals(&self, now: Timestamp) -> Result<usize> {
        self.pending.evict_expired(now)
    }

    /// Returns the pending approval's **redacted, operator-safe summary**
    /// by id; `None` if the approval is no longer pending or reading the
    /// storage surface fails.
    ///
    /// This is the same string that `submit-task` stored in the pending
    /// record ([`crate::pending_store::PendingRecord::redacted_summary`]) —
    /// derived only from the skill's name and ids, **never the raw payload
    /// or secrets**. It can be shown to the operator or kept for resume
    /// as-is.
    ///
    /// Used, among other things, in the agent layer's
    /// `ThinkOutcome::Suspended` path: when a tool stops to wait for
    /// approval, the agent stores this safe summary (+ the `approval_id`)
    /// into the turn's durable state for resume — instead of leaking raw
    /// approval data into the reply pipeline.
    #[must_use]
    pub fn pending_summary_for(&self, approval_id: ApprovalId) -> Option<String> {
        self.pending
            .get(approval_id)
            .ok()
            .flatten()
            .map(|record| record.redacted_summary)
    }

    /// Returns the pending approval's **expiry moment**
    /// ([`crate::approval::Approval::expires_at`]) by id; `None` if the
    /// approval is no longer pending or reading the storage surface fails.
    ///
    /// This is a secret-free timestamp (no payload, no summary), which the
    /// agent layer needs to bind the **resumable turn's** (the resume state
    /// built on top of [`crate::pending_store::PendingRecord`]) TTL to
    /// exactly the same expiry as the granted approval. This way the
    /// resumable turn expires at the same moment as the permission that
    /// could consume it — neither earlier nor later.
    #[must_use]
    pub fn pending_expiry_for(&self, approval_id: ApprovalId) -> Option<Timestamp> {
        self.pending
            .get(approval_id)
            .ok()
            .flatten()
            .map(|record| record.expires_at())
    }

    /// Returns the pending approval's **creation moment**
    /// ([`crate::pending_store::PendingRecord::created_at`]) by id; `None`
    /// if the approval is no longer pending or reading the storage surface
    /// fails.
    ///
    /// This is a secret-free audit timestamp (no payload, no summary, no
    /// secrets), which the operator surface (e.g. the gateway's `GET
    /// /approvals/pending`) displays to report **when** the approval has
    /// been pending. It corresponds exactly to the metadata shown alongside
    /// [`PendingApproval`] and reveals nothing about **what** the approval
    /// concerns beyond what [`ActionRuntime::pending_summary_for`] already
    /// discloses in redacted form.
    #[must_use]
    pub fn pending_created_at_for(&self, approval_id: ApprovalId) -> Option<Timestamp> {
        self.pending
            .get(approval_id)
            .ok()
            .flatten()
            .map(|record| record.created_at)
    }

    /// Builds a **redacted** summary of a pending approval for storage: only
    /// the skill's name (or id) — never the raw payload or secrets.
    fn pending_summary(&self, skill_id: SkillId) -> String {
        let name = self
            .pipeline
            .registry()
            .get(&skill_id)
            .map_or_else(|| skill_id.to_string(), |m| m.name.clone());
        format!("taito '{name}' odottaa ihmisen hyväksyntää")
    }
}

/// Picks the **strictest** single permission from a set, used to gate the
/// MCP tool derived from a skill
/// ([`McpToolDescriptor::required_permission`] takes a single value).
///
/// A skill's manifest can declare multiple permissions, but the tool
/// descriptor only gets one. The most permissive one (most severe in terms
/// of side effects) is chosen, so the agent requires the strongest
/// necessary capability from the caller — fail-safe: the required
/// permission is never underestimated. Severity order, increasing:
///
/// ```text
/// ReadFiles < NetworkRead < WriteLocalFiles < SendMessage
///           < ExecuteCode < WriteExternal < SpendMoney
/// ```
///
/// If the list is empty (the skill requires no permissions), the least
/// permissive [`SkillPermission::ReadFiles`] is returned.
fn strictest_permission(permissions: &[SkillPermission]) -> SkillPermission {
    permissions
        .iter()
        .copied()
        .max_by_key(|p| permission_severity(*p))
        .unwrap_or(SkillPermission::ReadFiles)
}

/// Severity level of a single permission (higher = more permissive / more
/// severe in terms of side effects). Used in [`strictest_permission`] to
/// pick the strictest permission deterministically.
const fn permission_severity(permission: SkillPermission) -> u8 {
    match permission {
        SkillPermission::ReadFiles => 0,
        SkillPermission::NetworkRead => 1,
        SkillPermission::WriteLocalFiles => 2,
        SkillPermission::SendMessage => 3,
        SkillPermission::ExecuteCode => 4,
        SkillPermission::WriteExternal => 5,
        SkillPermission::SpendMoney => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{EmailTriageMock, FilePatchMock, GithubIssueDraftMock};
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    #[test]
    fn default_skills_are_listed_without_secrets() {
        let runtime = ActionRuntime::with_default_skills().expect("default skills");
        let skills = runtime.list_skills();
        assert_eq!(skills.len(), 11, "all eleven default skills registered");

        // Names alphabetized → deterministic order.
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);

        // The output contains no secrets (only public fields).
        let rendered = serde_json::to_string(&skills).expect("serialize summaries");
        assert!(!rendered.contains("sk-"));
        assert!(!rendered.contains("Bearer "));
    }

    /// RESEARCH SKILL ON: when [`ActionRuntime::register_default_skills_with_fs_read`]
    /// gets an allowlist, the flagship skill [`FsReadAllowlisted`] (a) remains
    /// in the listing and is not duplicated (same fixed skill id), and (b) really
    /// reads the allowlisted file on the **auto-run path** (no approval) — i.e. the
    /// agent can research files. Without an allowlist the same skill would reject everything.
    #[tokio::test]
    async fn fs_read_config_makes_research_skill_functional() {
        // Isolated allowlisted directory + file.
        let dir = std::env::temp_dir().join(format!(
            "familyclaw-facade-fsread-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let canonical = std::fs::canonicalize(&dir).expect("canonicalize");
        let file = canonical.join("note.txt");
        std::fs::write(&file, "first line of research\nsecond line\n").expect("write file");

        let config = FsReadConfig::new().allow_root(&canonical);
        let mut runtime = ActionRuntime::new();
        runtime
            .register_default_skills_with_fs_read(Some(config))
            .expect("register with fs_read allowlist");

        // (a) All nine skills still in the listing (fs_read wasn't duplicated).
        //     Six original + three parity executors (2026-07-03).
        let names: Vec<String> = runtime.list_skills().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names.len(),
            11,
            "all eleven default skills registered exactly once"
        );
        assert!(names.iter().any(|n| n == "fs_read_allowlisted"));
        assert!(names.iter().any(|n| n == "web_fetch"));
        // Parity executors wired by default (agent capability gap):
        assert!(names.iter().any(|n| n == "web_search"));
        assert!(names.iter().any(|n| n == "research"));
        assert!(names.iter().any(|n| n == "file_write_allowlisted"));

        // (b) Reading the allowlisted file runs on the auto-run path (ReadOnly +
        //     AutoIfReadOnly) → the task completes, does not wait for approval.
        let skill_id = runtime
            .map_name_to_skill("fs_read_allowlisted")
            .expect("fs_read registered");
        let payload = json!({ "path": file.to_string_lossy() });
        let outcome = runtime
            .submit_task(skill_id, payload, at(1_700_000_000))
            .await
            .expect("submit fs_read");
        assert!(
            outcome.pending_approval.is_none(),
            "read-only fs_read must auto-run, not wait for approval"
        );
        assert_eq!(
            outcome.status,
            TaskStatus::Done,
            "allowlisted file read must complete"
        );

        let _ = std::fs::remove_dir_all(&canonical);
    }

    /// WRITE SKILL ON: when [`ActionRuntime::register_default_skills_with_configs`]
    /// gets a `file_write` allowlist, `file_write` (a) remains in the listing, and (b)
    /// after approval REALLY writes the allowlisted file to disk.
    /// Without an allowlist the same skill would reject everything (fail-closed). This closes
    /// the gap found by the gap-recheck: in the default run `file_write` was fail-closed.
    #[tokio::test]
    async fn file_write_config_makes_write_skill_functional() {
        let dir = std::env::temp_dir().join(format!(
            "familyclaw-facade-filewrite-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let canonical = std::fs::canonicalize(&dir).expect("canonicalize");

        let fw_config = FileWriteConfig::new().allow_root(&canonical);
        let mut runtime = ActionRuntime::new();
        runtime
            .register_default_skills_with_configs(None, Some(fw_config), None)
            .expect("register with file_write allowlist");

        // file_write is WriteLocal + RequireApproval → the allowlisted write runs immediately.
        let skill_id = runtime
            .map_name_to_skill("file_write_allowlisted")
            .expect("file_write registered");
        let target = canonical.join("report.md");
        let payload = json!({
            "path": target.to_string_lossy(),
            "content": "# Report\nwritten via the default skill registry\n",
        });
        let submitted = runtime
            .submit_task(skill_id, payload, at(1_700_000_000))
            .await
            .expect("submit file_write");
        assert!(
            submitted.pending_approval.is_none(),
            "allowlisted write must auto-run"
        );
        assert_eq!(
            submitted.status,
            TaskStatus::Done,
            "allowlisted write must complete without manual approval"
        );

        let written = std::fs::read_to_string(&target).expect("file written to disk");
        assert!(written.contains("written via the default skill registry"));

        let _ = std::fs::remove_dir_all(&canonical);
    }

    /// The default configuration ([`ActionRuntime::with_default_skills`]) gets
    /// an in-memory outbox, and [`ActionRuntime::with_dispatch_outbox`]
    /// wires in the crash-resistant journal variant instead.
    ///
    /// This locks in a wiring check: the assembler (`familyclaw-runtime`) relies
    /// on `dispatch_outbox_kind()` to confirm that the persistent path got the
    /// `"journal"` outbox instead of the default `"in-memory"` one.
    #[test]
    fn dispatch_outbox_kind_reflects_wired_variant() {
        // Default: in-memory.
        let in_memory = ActionRuntime::with_default_skills().expect("default skills");
        assert_eq!(in_memory.dispatch_outbox_kind(), "in-memory");

        // Wired journal outbox → "journal".
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "familyclaw-facade-outbox-{}-{nanos}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let journal = crate::dispatch_outbox::JournalDispatchOutbox::open(&path)
            .expect("open journal outbox");
        let durable = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_dispatch_outbox(Box::new(journal));
        assert_eq!(durable.dispatch_outbox_kind(), "journal");
        let _ = std::fs::remove_file(&path);
    }

    /// Regression guard for a former trap: [`ActionRuntime::with_durable_stores`]
    /// wires up ALL THREE crash-resistant surfaces (pending + task + dispatch
    /// outbox) **without** a separate [`ActionRuntime::with_dispatch_outbox`]
    /// chain call.
    ///
    /// Previously the durable constructor hard-coded the in-memory
    /// [`InMemoryDispatchOutbox`], so a caller who forgot to chain silently
    /// had the at-most-once protection turned OFF for dispatch. This test
    /// proves that plain `with_durable_stores` is now sufficient: both
    /// `dispatch_outbox_kind()` and `pending_store_kind()` are `"journal"`.
    #[tokio::test]
    async fn durable_stores_yield_journal_dispatch_without_chaining() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "familyclaw-facade-durable-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        // NO with_dispatch_outbox chaining — just the constructor.
        let runtime = ActionRuntime::with_durable_stores(
            dir.join("pending_approvals.jsonl"),
            dir.join("action_tasks.jsonl"),
            dir.join("dispatch_outbox.jsonl"),
        )
        .await
        .expect("durable stores open");

        assert_eq!(
            runtime.dispatch_outbox_kind(),
            "journal",
            "durable-konstruktori kytkee journal-outboxin ilman ketjutusta (ent. ansa)"
        );
        assert_eq!(
            runtime.pending_store_kind(),
            "journal",
            "durable-konstruktori kytkee journal-pending-pinnan"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_definitions_mirror_skills_sorted_without_secrets() {
        let runtime = ActionRuntime::with_default_skills().expect("default skills");
        let tools = runtime.tool_definitions();
        assert_eq!(tools.len(), 11, "one descriptor per registered skill");

        // Same stabilized name order as list_skills.
        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        let skill_names: Vec<String> = runtime.list_skills().into_iter().map(|s| s.name).collect();
        assert_eq!(tool_names, skill_names);

        // Each descriptor's input schema is the manifest's schema (object root) and
        // the source defaults to untrusted.
        for tool in &tools {
            let id = runtime
                .map_name_to_skill(&tool.name)
                .expect("tool name maps to a skill");
            let manifest = runtime
                .pipeline
                .registry()
                .get(&id)
                .expect("mapped skill in registry");
            assert_eq!(tool.input_schema, manifest.input_schema);
            assert!(
                tool.input_schema.is_object(),
                "schema root must be a JSON object for LLM parameters"
            );
            assert!(!tool.trusted, "skill-derived tools default to untrusted");
            assert!(!tool.description.is_empty());
        }

        // No secrets in the output.
        let rendered = serde_json::to_string(&tools).expect("serialize descriptors");
        assert!(!rendered.contains("sk-"));
        assert!(!rendered.contains("Bearer "));
    }

    #[test]
    fn map_name_to_skill_roundtrips_with_tool_definitions() {
        let runtime = ActionRuntime::with_default_skills().expect("default skills");

        // Every published tool name routes back to the skill's id.
        for tool in runtime.tool_definitions() {
            let id = runtime
                .map_name_to_skill(&tool.name)
                .expect("known tool name maps to a skill");
            // The id matches the manifest's id in the registry.
            let manifest = runtime
                .pipeline
                .registry()
                .get(&id)
                .expect("mapped id exists in registry");
            assert_eq!(manifest.name, tool.name);
        }
    }

    #[test]
    fn map_name_to_skill_unknown_is_none() {
        let runtime = ActionRuntime::with_default_skills().expect("default skills");
        assert!(runtime.map_name_to_skill("does_not_exist").is_none());
    }

    #[test]
    fn tool_definition_required_permission_is_strictest() {
        // The GitHub issue draft skill writes to an external system →
        // the strictest permission must be write_external (not e.g. network_read).
        let runtime = ActionRuntime::with_default_skills().expect("default skills");
        let id = GithubIssueDraftMock::skill_id();
        let manifest = runtime
            .pipeline
            .registry()
            .get(&id)
            .expect("github skill registered");
        let expected = super::strictest_permission(&manifest.permissions);

        let tool = runtime
            .tool_definitions()
            .into_iter()
            .find(|t| t.name == manifest.name)
            .expect("github tool published");
        assert_eq!(tool.required_permission, expected);
    }

    #[test]
    fn strictest_permission_picks_most_privileged() {
        // Empty list → least-permissive default.
        assert_eq!(super::strictest_permission(&[]), SkillPermission::ReadFiles);
        // Mixed set → strictest (spend_money).
        assert_eq!(
            super::strictest_permission(&[
                SkillPermission::ReadFiles,
                SkillPermission::SpendMoney,
                SkillPermission::NetworkRead,
            ]),
            SkillPermission::SpendMoney
        );
        // write_external beats send_message.
        assert_eq!(
            super::strictest_permission(&[
                SkillPermission::SendMessage,
                SkillPermission::WriteExternal,
            ]),
            SkillPermission::WriteExternal
        );
    }

    #[tokio::test]
    async fn read_only_task_auto_runs_and_produces_proof() {
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let now = at(1_700_000_000);

        // Email triage is read-only → auto-run, no approval.
        let payload = json!({
            "emails": [
                { "from": "user@example.com", "subject": "Invoice question", "body": "When is it due?" }
            ]
        });
        let outcome = runtime
            .submit_task(EmailTriageMock::skill_id(), payload, now)
            .await
            .expect("submit");

        assert_eq!(outcome.status, TaskStatus::Done);
        assert!(!outcome.awaiting_approval());
        assert!(outcome.pending_approval.is_none());

        // Status is Done, the proof is retrievable.
        assert_eq!(
            runtime.status(outcome.task_id).await,
            Some(TaskStatus::Done)
        );
        let proof = runtime.proof(outcome.task_id).expect("proof present");
        assert_eq!(proof.task_id, outcome.task_id);
        assert!(proof.verification.verified);
    }

    #[tokio::test]
    async fn write_external_task_waits_for_approval_then_completes() {
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let now = at(1_700_000_000);

        // GitHub issue draft is write-external → requires approval.
        let payload = json!({ "bug_report": "Login button does nothing" });
        let submitted = runtime
            .submit_task(GithubIssueDraftMock::skill_id(), payload, now)
            .await
            .expect("submit");

        assert_eq!(submitted.status, TaskStatus::NeedsApproval);
        assert!(submitted.awaiting_approval());
        let approval_id = submitted.pending_approval.expect("approval granted");

        // The pending approval appears in the listing.
        let pending = runtime.pending_approvals();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].approval_id, approval_id);
        assert_eq!(pending[0].task_id, submitted.task_id);

        // Before approval there is no proof.
        assert!(runtime.proof(submitted.task_id).is_none());

        // Approve → run to completion, proof is produced.
        let approved = runtime.approve(approval_id, now).await.expect("approve");
        assert_eq!(approved.task_id, submitted.task_id);
        assert_eq!(approved.status, TaskStatus::Done);

        // Approval consumed → no longer pending.
        assert!(runtime.pending_approvals().is_empty());
        // The proof is now retrievable.
        assert!(runtime.proof(submitted.task_id).is_some());
        assert_eq!(
            runtime.status(submitted.task_id).await,
            Some(TaskStatus::Done)
        );
    }

    #[tokio::test]
    async fn per_being_rate_limit_denies_next_approval_required_submit() {
        // Strict limiter: at most 2 approval-requiring actions per being
        // in a 60 s window. A third approval-requiring dispatch from the same
        // being is rejected fail-closed.
        let mut runtime = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_rate_limiter(DangerousToolRateLimiter::new(60, 2));
        let now = at(1_700_000_000);
        let payload = json!({ "bug_report": "Button does nothing" });

        // The first two approval-requiring dispatches fit within the quota.
        let first = runtime
            .submit_task_as(
                "being-a",
                GithubIssueDraftMock::skill_id(),
                payload.clone(),
                now,
            )
            .await
            .expect("first approval-required submit fits quota");
        assert!(first.awaiting_approval(), "first must await approval");
        let second = runtime
            .submit_task_as(
                "being-a",
                GithubIssueDraftMock::skill_id(),
                payload.clone(),
                now,
            )
            .await
            .expect("second approval-required submit fits quota");
        assert!(second.awaiting_approval(), "second must await approval");

        // The third exceeds the per-being quota → PolicyDenied (approval is not granted).
        let err = runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload, now)
            .await
            .expect_err("third approval-required submit exceeds per-being quota");
        assert!(matches!(err, ActionError::PolicyDenied(_)));

        // Approval was not granted for the third → still only two are pending.
        assert_eq!(
            runtime.pending_approvals().len(),
            2,
            "denied submit must not enqueue a pending approval"
        );
    }

    #[tokio::test]
    async fn rate_limit_is_per_being_separate_quota() {
        // The limiter allows only one approval-requiring action per being per
        // window. being-a uses up its quota; being-b is unaffected (its own quota).
        let mut runtime = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_rate_limiter(DangerousToolRateLimiter::new(60, 1));
        let now = at(1_700_000_000);
        let payload = json!({ "bug_report": "Button does nothing" });

        runtime
            .submit_task_as(
                "being-a",
                GithubIssueDraftMock::skill_id(),
                payload.clone(),
                now,
            )
            .await
            .expect("being-a first fits its quota");
        // being-a's quota is now exhausted.
        let denied = runtime
            .submit_task_as(
                "being-a",
                GithubIssueDraftMock::skill_id(),
                payload.clone(),
                now,
            )
            .await
            .expect_err("being-a second exceeds quota");
        assert!(matches!(denied, ActionError::PolicyDenied(_)));

        // A DIFFERENT being → its own quota, unaffected by being-a's exhaustion.
        let other = runtime
            .submit_task_as("being-b", GithubIssueDraftMock::skill_id(), payload, now)
            .await
            .expect("being-b unaffected by being-a quota");
        assert!(other.awaiting_approval(), "being-b must still get approval");
    }

    #[tokio::test]
    async fn rate_limit_window_slides_capacity_returns() {
        // One approval-requiring action per 60 s window. After the window the
        // quota returns and the same being may submit again.
        let mut runtime = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_rate_limiter(DangerousToolRateLimiter::new(60, 1));
        let now = at(1_700_000_000);
        let payload = json!({ "bug_report": "Button does nothing" });

        runtime
            .submit_task_as(
                "being-a",
                GithubIssueDraftMock::skill_id(),
                payload.clone(),
                now,
            )
            .await
            .expect("first fits quota");
        // Immediately after, same window → blocked.
        let denied = runtime
            .submit_task_as(
                "being-a",
                GithubIssueDraftMock::skill_id(),
                payload.clone(),
                now,
            )
            .await
            .expect_err("second in same window is denied");
        assert!(matches!(denied, ActionError::PolicyDenied(_)));

        // After the window slides (now + 61 s) the old record is evicted → room again.
        let later = at(1_700_000_061);
        let after = runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload, later)
            .await
            .expect("capacity returns after window slides");
        assert!(
            after.awaiting_approval(),
            "submit must succeed after window"
        );
    }

    #[tokio::test]
    async fn auto_run_tasks_are_not_rate_limited() {
        // A limiter that would block all dangerous calls (quota 0). Read-only
        // (auto-run) tasks do NOT go through the rate limiter → they always run.
        let mut runtime = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_rate_limiter(DangerousToolRateLimiter::new(60, 0));
        let now = at(1_700_000_000);
        let payload = json!({
            "emails": [
                { "from": "user@example.com", "subject": "Invoice question", "body": "When is it due?" }
            ]
        });

        // Multiple consecutive read-only dispatches — a quota of 0 blocks none of
        // them, because they don't require approval and never touch the rate limiter.
        for _ in 0..3 {
            let outcome = runtime
                .submit_task_as("being-a", EmailTriageMock::skill_id(), payload.clone(), now)
                .await
                .expect("read-only auto-run is never rate-limited");
            assert_eq!(outcome.status, TaskStatus::Done);
            assert!(!outcome.awaiting_approval());
        }
        // None was left waiting for approval.
        assert!(runtime.pending_approvals().is_empty());
    }

    #[tokio::test]
    async fn pending_created_at_for_returns_record_creation_time() {
        // A pending approval → the creation moment is retrievable by id and
        // matches the `now` timestamp given to `submit_task` (deterministic).
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let now = at(1_700_000_000);
        let submitted = runtime
            .submit_task(
                GithubIssueDraftMock::skill_id(),
                json!({ "bug_report": "Button does nothing" }),
                now,
            )
            .await
            .expect("submit");
        let approval_id = submitted.pending_approval.expect("approval granted");

        assert_eq!(runtime.pending_created_at_for(approval_id), Some(now));
        // Unknown id → None (fail-closed, no panic).
        assert!(runtime.pending_created_at_for(ApprovalId::new()).is_none());
    }

    #[tokio::test]
    async fn submit_unknown_skill_fails() {
        let mut runtime = ActionRuntime::new();
        let err = runtime
            .submit_task(SkillId::new(), json!({}), at(1))
            .await
            .expect_err("unknown skill must fail");
        assert!(matches!(err, ActionError::UnknownSkill(_)));
    }

    #[tokio::test]
    async fn approve_unknown_approval_fails_closed() {
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let err = runtime
            .approve(ApprovalId::new(), at(1))
            .await
            .expect_err("unknown approval must fail closed");
        assert!(matches!(err, ActionError::ApprovalMissing(_)));
    }

    #[tokio::test]
    async fn approval_cannot_be_reused() {
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let now = at(1_700_000_000);

        let submitted = runtime
            .submit_task(
                FilePatchMock::skill_id(),
                json!({ "file_content": "line one\n", "requested_edit": "add a line" }),
                now,
            )
            .await
            .expect("submit");
        let approval_id = submitted
            .pending_approval
            .expect("file patch requires approval");

        runtime
            .approve(approval_id, now)
            .await
            .expect("first approve");

        // Second consumption fails: the approval was removed from pending.
        let err = runtime
            .approve(approval_id, now)
            .await
            .expect_err("second approve must fail closed");
        assert!(matches!(err, ActionError::ApprovalMissing(_)));
    }

    #[tokio::test]
    async fn status_and_proof_for_missing_task_are_none() {
        let runtime = ActionRuntime::new();
        let missing = ActionTaskId::new();
        assert!(runtime.status(missing).await.is_none());
        assert!(runtime.proof(missing).is_none());
    }

    /// Test skill that echoes the payload's `secret` field value directly
    /// into the output as a standalone value. Used to prove that the proof
    /// bundle produced through the facade gets redacted (Layer A — test use only).
    #[derive(Debug, Clone, Default)]
    struct EchoSecretSkill;

    /// The test skill's fixed id.
    const ECHO_SKILL_UUID: uuid::Uuid = uuid::uuid!("99999999-9999-4999-8999-999999999999");

    #[async_trait::async_trait]
    impl ActionExecutor for EchoSecretSkill {
        async fn execute(
            &self,
            request: crate::executor::ActionRequest,
        ) -> Result<crate::executor::ActionResult> {
            // Echo the payload's "secret" field into the output as a standalone value.
            let echoed = request
                .payload
                .get("secret")
                .cloned()
                .unwrap_or(Value::Null);
            Ok(crate::executor::ActionResult::success(
                "echoed input value",
                json!({ "echoed": echoed }),
                request.now,
            ))
        }
    }

    impl Skill for EchoSecretSkill {
        fn manifest(&self) -> crate::manifest::SkillManifest {
            crate::manifest::SkillManifest {
                id: SkillId::from_uuid(ECHO_SKILL_UUID),
                name: "echo_secret_test".to_string(),
                version: "1.0.0".to_string(),
                description: "Kaiuttaa syötteen tulosteeseen (vain luku, testikäyttö).".to_string(),
                permissions: vec![crate::policy::SkillPermission::NetworkRead],
                risk: ActionRisk::ReadOnly,
                approval_policy: crate::policy::ApprovalPolicy::AutoIfReadOnly,
                input_hint: None,
                output_hint: None,
                input_schema: crate::manifest::default_input_schema(),
                publisher: None,
                signature: None,
            }
        }
    }

    #[tokio::test]
    async fn proof_is_redacted_for_secret_looking_payload() {
        let mut runtime = ActionRuntime::new();
        runtime
            .register_skill(EchoSecretSkill)
            .expect("register echo skill");
        let now = at(1_700_000_000);

        // The secret is built at runtime (not a literal in source, Layer B).
        let fake = format!("sk-{}", "live".repeat(4));
        // The skill echoes the secret as a standalone value → without redaction
        // it would flow into the proof's redacted_output field.
        let payload = json!({ "secret": fake.clone() });
        let outcome = runtime
            .submit_task(SkillId::from_uuid(ECHO_SKILL_UUID), payload, now)
            .await
            .expect("submit");
        assert_eq!(outcome.status, TaskStatus::Done);

        let proof = runtime.proof(outcome.task_id).expect("proof present");
        // The output was redacted: the raw secret is nowhere in the proof.
        assert!(
            proof.redaction.any_redacted(),
            "secret-looking output value must be redacted"
        );
        let whole = serde_json::to_string(proof).expect("serialize proof");
        assert!(
            !whole.contains(&fake),
            "proof must never contain raw secret"
        );
    }

    // --- approve() idempotency (prevents double-dispatch on the approval path) ---

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// Test skill that **requires approval** (write-external) and counts
    /// side-effect runs. Used to prove that [`ActionRuntime::approve`] runs
    /// the side effect **at most once** under the outbox's protection.
    #[derive(Debug, Clone)]
    struct CountingApprovalSkill {
        /// Number of side-effect runs (shared with the test).
        runs: Arc<AtomicU64>,
    }

    /// The counting test skill's fixed id.
    const COUNTING_SKILL_UUID: uuid::Uuid = uuid::uuid!("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");

    #[async_trait::async_trait]
    impl ActionExecutor for CountingApprovalSkill {
        async fn execute(
            &self,
            request: crate::executor::ActionRequest,
        ) -> Result<crate::executor::ActionResult> {
            // SIDE EFFECT: increment the counter. This must happen exactly once.
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(crate::executor::ActionResult::success(
                "side effect fired",
                json!({ "ok": true }),
                request.now,
            ))
        }
    }

    impl Skill for CountingApprovalSkill {
        fn manifest(&self) -> crate::manifest::SkillManifest {
            crate::manifest::SkillManifest {
                id: SkillId::from_uuid(COUNTING_SKILL_UUID),
                name: "counting_approval_test".to_string(),
                version: "1.0.0".to_string(),
                description: "Laskee sivuvaikutuksen ajot (vaatii hyväksynnän, testikäyttö)."
                    .to_string(),
                // Write-external → requires approval → goes through the run_after_approval path.
                permissions: vec![crate::policy::SkillPermission::WriteExternal],
                risk: ActionRisk::WriteExternal,
                approval_policy: crate::policy::ApprovalPolicy::RequireApproval,
                input_hint: None,
                output_hint: None,
                input_schema: crate::manifest::default_input_schema(),
                publisher: None,
                signature: None,
            }
        }
    }

    /// A shared (Arc-backed) in-memory outbox for pre-seeding the test.
    ///
    /// [`ActionRuntime::with_dispatch_outbox`] consumes a `Box<dyn ...>`, so
    /// this wrapper gives the test a parallel handle to the same outbox
    /// state: the test can record a committed/intent row BEFORE the
    /// `approve` call and confirm the side effect doesn't re-run.
    #[derive(Debug, Clone)]
    struct SharedOutbox(Arc<InMemoryDispatchOutbox>);

    impl DispatchOutboxStore for SharedOutbox {
        fn kind(&self) -> &'static str {
            self.0.kind()
        }
        fn lookup(&self, key: &str) -> Result<DispatchLookup> {
            self.0.lookup(key)
        }
        fn record_intent(&self, key: &str) -> Result<()> {
            self.0.record_intent(key)
        }
        fn record_committed(&self, key: &str, outcome: &DispatchedOutcome) -> Result<()> {
            self.0.record_committed(key, outcome)
        }
    }

    /// Builds a runtime with the counting approval skill + a shared outbox,
    /// and submits one approval-requiring task. Returns the runtime, the
    /// shared outbox handle, the counter, the submitted task's id, and the
    /// `approval_id`.
    async fn build_approval_fixture(
        now: Timestamp,
    ) -> (
        ActionRuntime,
        Arc<InMemoryDispatchOutbox>,
        Arc<AtomicU64>,
        ActionTaskId,
        ApprovalId,
    ) {
        let runs = Arc::new(AtomicU64::new(0));
        let shared = Arc::new(InMemoryDispatchOutbox::new());
        let mut runtime =
            ActionRuntime::new().with_dispatch_outbox(Box::new(SharedOutbox(Arc::clone(&shared))));
        runtime
            .register_skill(CountingApprovalSkill {
                runs: Arc::clone(&runs),
            })
            .expect("register counting approval skill");

        let submitted = runtime
            .submit_task(
                SkillId::from_uuid(COUNTING_SKILL_UUID),
                json!({ "any": "payload" }),
                now,
            )
            .await
            .expect("submit");
        assert_eq!(submitted.status, TaskStatus::NeedsApproval);
        let approval_id = submitted.pending_approval.expect("approval granted");
        // The dispatch has not yet run the side effect (awaiting approval).
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "no side effect before approve"
        );

        (runtime, shared, runs, submitted.task_id, approval_id)
    }

    #[tokio::test]
    async fn approve_with_committed_outbox_entry_returns_prior_without_rerun() {
        // Scenario: the process crashed earlier AFTER `record_committed` but
        // BEFORE `pending.remove` → the approval is still pending, but the outbox
        // has a committed row for the key `approval-{id}`. A re-approval must NOT
        // re-run the side effect, but must return the stored outcome.
        let now = at(1_700_000_000);
        let (mut runtime, shared, runs, task_id, approval_id) = build_approval_fixture(now).await;

        // Pre-seed the outbox with a committed row for EXACTLY the key approve uses.
        let key = ActionRuntime::approval_dispatch_key(approval_id);
        let prior = DispatchedOutcome {
            task_id,
            status: TaskStatus::Done,
            pending_approval: None,
            error: None,
        };
        shared
            .record_committed(&key, &prior)
            .expect("seed committed");

        // approve → committed branch: returns the prior outcome without running.
        let approved = runtime.approve(approval_id, now).await.expect("approve");
        assert_eq!(approved.task_id, task_id);
        assert_eq!(approved.status, TaskStatus::Done);
        assert!(approved.pending_approval.is_none());
        // CRITICAL: the counter stays at 0 — the side effect did NOT re-run.
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "committed outbox entry must short-circuit before run_after_approval"
        );
    }

    #[tokio::test]
    async fn approve_with_intent_only_outbox_entry_fails_closed_without_rerun() {
        // Scenario: the process crashed in the intent-only window (intent on
        // disk, committed not written, the side effect possibly ran partially).
        // A re-approval is fail-closed (PolicyDenied) and does not re-run.
        let now = at(1_700_000_000);
        let (mut runtime, shared, runs, _task_id, approval_id) = build_approval_fixture(now).await;

        // Pre-seed the outbox with ONLY an intent row (no committed) → InProgress.
        let key = ActionRuntime::approval_dispatch_key(approval_id);
        shared.record_intent(&key).expect("seed intent");

        // approve → in-progress branch: fail-closed PolicyDenied, no side effect.
        let before = runs.load(Ordering::SeqCst);
        let err = runtime
            .approve(approval_id, now)
            .await
            .expect_err("intent-only must fail closed");
        assert!(
            matches!(err, ActionError::PolicyDenied(_)),
            "intent-only window must be PolicyDenied (fail-closed), got {err:?}"
        );
        // The counter stays unchanged — the side effect did NOT re-run.
        assert_eq!(
            runs.load(Ordering::SeqCst),
            before,
            "intent-only outbox entry must not re-run the side effect"
        );
    }
}
