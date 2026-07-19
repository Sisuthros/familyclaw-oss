//! # familyclaw-actions
//!
//! The **action and proof pipeline** of the `FamilyClaw` platform: the layer
//! that turns an agent's intent into a safe, verifiable, and rememberable
//! action. The crate implements the following pipeline:
//!
//! ```text
//! observe → plan → request approval (if needed) → execute action
//!         → verify → persist proof → remember → report
//! ```
//!
//! - **observe** — perceive the situation and gather context.
//! - **plan** — pick a skill ([`registry`]) and form an action task ([`task`]).
//! - **request approval** — ask for human approval ([`approval`]) if the
//!   policy ([`policy`]) requires it; an approval is TTL-bound,
//!   single-use, and bound to a payload hash.
//! - **execute** — run the approved action ([`executor`]) via the skill.
//! - **verify** — check the result's validity against postconditions.
//! - **persist proof** — assemble a redacted proof bundle ([`proof`]).
//! - **remember** — persist a trace to memory (a separate substrate layer).
//! - **report** — record an audit event ([`audit`]) and report the result.
//!
//! Skills can also be published as MCP tools ([`mcp`]), and the pipeline's
//! full end-to-end behavior is covered by evals ([`evals`]).
//!
//! ## OSS boundary (Layer A)
//! This crate is publishable. It contains only **generic types** — no real
//! providers, souls, API keys, tokens, IP addresses, or personal paths. It
//! includes **two genuine reference skills**
//! ([`FsReadAllowlisted`] local file reads,
//! [`WebFetchSkill`](skills::WebFetchSkill) read-only
//! HTTP GET with SSRF guarding) that do real work without keys; the rest of
//! the skills are **example templates** that show the skill contract, into
//! which you wire your own provider (no real Gmail/GitHub network calls
//! out of the box). Proof bundles **redact** values that look like secrets
//! before persisting.
//!
//! ## Design principles
//! - **No `unwrap()`/`expect()`/`panic!()` on the production path.** All
//!   errors flow through the [`ActionError`] and [`Result`] types.
//! - **Determinism:** pure logic takes the timestamp as an injected value
//!   ([`familyclaw_core::time::Timestamp`]) — the clock is never read
//!   inside the logic.
//! - **Typed identifiers** ([`SkillId`], [`ActionTaskId`], [`ApprovalId`],
//!   [`ProofBundleId`], [`ActionId`], [`AuditEventId`]) prevent mix-ups at
//!   compile time.
//!
//! ## Modules
//! - [`manifest`] — skill manifests (description, schema, capabilities).
//! - [`registry`] — skill registry (mock skills).
//! - [`policy`] — policy: permission + approval requirement.
//! - [`approval`] — human approval (TTL, nonce, payload binding).
//! - [`audit`] — tamper-evident audit log.
//! - [`task`] — action task state and lifecycle.
//! - [`executor`] — execution of an approved action.
//! - [`proof`] — redacted proof bundle.
//! - [`mcp`] — publishing skills as MCP tools.
//! - [`pending_store`] — crash-safe storage surface for pending approvals.
//! - [`skills`] — realistic mock skills + the full pipeline ([`skills::Pipeline`]).
//! - [`facade`] — an operator surface ([`ActionRuntime`]) over the whole pipeline.
//! - [`evals`] — end-to-end evals.
//! - [`ids`] — typed identifiers.
//! - [`error`] — [`ActionError`], [`Result`].
//!
//! ## Operator CLI
//! The crate also includes a thin command-line binary
//! (`src/bin/familyclaw-actions-cli.rs`) that uses the [`ActionRuntime`]
//! facade: `list-skills`, `submit-task`, `approve`, `status`, `proof`.

pub mod approval;
pub mod audit;
pub mod dispatch_outbox;
pub mod error;
pub mod evals;
pub mod executor;
pub mod facade;
pub mod ids;
pub mod manifest;
pub mod mcp;
pub mod pending_store;
pub mod policy;
pub mod proof;
pub mod registry;
pub mod resource_budget;
pub mod skills;
pub mod task;

pub use audit::{
    ActionAuditEvent, AuditAction, AuditCollector, AuditKind, AuditLog, ExecAuditEvent,
};
pub use dispatch_outbox::{
    DispatchLookup, DispatchOutboxStore, DispatchedOutcome, InMemoryDispatchOutbox,
    JournalDispatchOutbox,
};
pub use error::{ActionError, Result};
pub use executor::{ActionExecutor, ActionRequest, ActionResult, ActionStatus, MockActionExecutor};
pub use facade::{ActionRuntime, PendingApproval, SkillSummary, SubmitOutcome};
pub use ids::{ActionId, ActionTaskId, ApprovalId, AuditEventId, ProofBundleId, SkillId};
pub use mcp::{
    call_with_policy, McpToolCall, McpToolDescriptor, McpToolProvider, McpToolResult,
    MockMcpProvider,
};
pub use pending_store::{
    DangerousToolRateLimiter, InMemoryPendingStore, JournalPendingStore, PendingApprovalStore,
    PendingCapacity, PendingRecord,
};
pub use proof::{
    build_proof, redact_free_text, redact_value, redact_value_deep, sha256_hex, ProofBundle,
    RedactionReport, VerificationResult,
};
pub use resource_budget::{AcquireOutcome, BudgetLimits, ResourceBudget, ResourceLease};
#[allow(deprecated)]
pub use skills::MockSkill;
pub use skills::{
    DiscordThreadSummaryMock, EmailTriageMock, FilePatchApply, FilePatchMock, FileWriteAllowlisted,
    FileWriteConfig, FsReadAllowlisted, FsReadConfig, GithubIssueDraftMock, GithubIssueSkill,
    MemoryRecord, Pipeline, PipelineOutcome, ResearchSkill, ShellExec, ShellExecConfig, ShellMode,
    Skill, SpawnSubagentSkill, SubagentSpawner, WebSearchSkill,
};

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Whether the whole crate is in its scaffold stage (all pipeline modules wired up).
///
/// A temporary boolean that keeps the scaffold modules' placeholders "alive"
/// (prevents `dead_code` warnings under CI's `-D warnings` gate) until the
/// real module implementations replace them.
#[must_use]
pub const fn all_modules_scaffolded() -> bool {
    manifest::SCAFFOLDED
        && registry::SCAFFOLDED
        && policy::SCAFFOLDED
        && approval::SCAFFOLDED
        && audit::SCAFFOLDED
        && dispatch_outbox::SCAFFOLDED
        && task::SCAFFOLDED
        && executor::SCAFFOLDED
        && proof::SCAFFOLDED
        && mcp::SCAFFOLDED
        && pending_store::SCAFFOLDED
        && skills::SCAFFOLDED
        && facade::SCAFFOLDED
        && evals::SCAFFOLDED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn scaffold_is_wired() {
        assert!(all_modules_scaffolded());
    }

    #[test]
    fn public_ids_are_reexported() {
        // If any re-export is removed, this test fails to compile.
        let _s: SkillId = SkillId::new();
        let _t: ActionTaskId = ActionTaskId::new();
        let _a: ApprovalId = ApprovalId::new();
        let _p: ProofBundleId = ProofBundleId::new();
        let _ac: ActionId = ActionId::new();
        let _e: AuditEventId = AuditEventId::new();
    }

    #[test]
    fn public_error_is_reexported() {
        let err: ActionError = ActionError::UnknownSkill("skill_a".into());
        let res: Result<()> = Err(err);
        assert!(res.is_err());
    }
}
