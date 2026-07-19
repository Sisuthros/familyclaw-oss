//! `dispatch_redteam` — black box for proving exactly-once dispatch.
//!
//! This binary is run as a child process (like `continuity_daemon`) so that
//! "SIGKILL mid-dispatch" can be proven **across a real process boundary**. It
//! targets exactly the bug that GPT-5.5 revealed: the window in which the
//! [`ActionRuntime`]'s side effect (`submit_task`) has already happened but the
//! agent layer's durable journaling has NOT — a crash in that window causes
//! replay to run the side effect again (double-firing).
//!
//! ## Side-effect counter (proof)
//! The registered skill ([`CountingExecutor`]) **increments an on-disk counter
//! every time its `execute` is actually run**. The test reads the counter raw
//! and requires it to be exactly 1 — a double-fire would push it to 2.
//!
//! ## Modes (`--mode`)
//! - `old` — uses [`ActionRuntime::submit_task_as`] (the pre-fix, buggy path:
//!   NO outbox protection) → proves the bug DOES exist (counter = 2).
//! - `new` — uses [`ActionRuntime::submit_task_idempotent`] with the crash-safe
//!   outbox → proves the fix (counter = 1, outcome identical).
//!
//! ## Phases (`--phase`)
//! ### `submit_task` path (keys `turn-*`)
//! - `crash` — run the dispatch (side effect happens), record the outcome to
//!   the `--outcome-out` file, and **exit 137 BEFORE the agent gets a chance to
//!   journal the dispatch row**. This is the COMMITTED window: the outbox has
//!   already been fully written (intent + committed), only the agent layer's
//!   journal row is missing. A benign replay point (exactly-once value-identical).
//! - `crash_intent` — crash in the **INTENT-ONLY window**: `record_intent` is
//!   already on disk AND the side effect has already fired (counter = 1), but
//!   `record_committed` has NOT run yet. This is the genuinely dangerous window
//!   that proves the **at-most-once fail-closed** guarantee (cf. the module's
//!   [`CrashAfterIntentOutbox`]).
//! - `resume` — replays exactly what the agent's fresh-run branch does when the
//!   journal row is MISSING (because the crash prevented it): re-runs the SAME
//!   dispatch with the same idempotency key. After the COMMITTED window
//!   (`new` mode) the outbox returns the committed outcome without re-running
//!   the side effect; in `old` mode it is re-run (double-fire).
//! - `resume_intent` — replays after an intent-only crash: the outbox lookup
//!   returns `InProgress` → `submit_task_idempotent` returns
//!   [`PolicyDenied`](familyclaw_actions::ActionError::PolicyDenied) fail-closed,
//!   and the side effect does NOT re-run (counter stays at 1).
//!
//! ### Post-approval continuation dispatch path (keys `resume-{id}-dispatch-{k}`)
//! This proves the SAME at-most-once guarantee for the **continuation dispatch
//! key** that the agent's tool loop produces AFTER an approval is granted. When
//! a suspended turn is approved and the model requests **another** tool in
//! continuation, its dispatch is routed through `submit_task_idempotent` with
//! the key `resume-{approval_id}-dispatch-{k}` (derived directly from the
//! continuation approval's identifier + a running dispatch index). This key
//! shape is exactly what the production path builds (`drive_tool_loop`
//! constructs `{prefix}-dispatch-{k}` from the prefix `resume-{approval_id}`).
//! Previously this continuation's at-most-once was only proven by a same-process
//! unit test — these phases prove it **across a real process boundary**
//! (SIGKILL exit 137).
//! - `resume_continuation_crash` — run the continuation dispatch with the
//!   `resume-*-dispatch-*` key with the intent hook armed: `record_intent` is
//!   fsynced AND the side effect fires (counter = 1), the process aborts at the
//!   start of `record_committed` → **exits 137 in the INTENT-ONLY window**.
//!   Requires `--mode new` + `FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1`.
//! - `resume_continuation_resume` — a fresh process runs the SAME continuation
//!   dispatch with the SAME `resume-*-dispatch-*` key → the outbox lookup sees
//!   `InProgress` →
//!   `submit_task_idempotent` returns
//!   [`PolicyDenied`](familyclaw_actions::ActionError::PolicyDenied) fail-closed,
//!   and the side effect does NOT re-run (counter stays at 1).
//!
//! ### Approval path (keys `approval-*`)
//! This proves the SAME at-most-once guarantee for [`ActionRuntime::approve`]'s
//! side-effect window — the outbox key is `approval-{id}`, NOT `turn-*`. This
//! path needs a crash-safe **pending** surface (Wire stage) so that a fresh
//! process can load the pending approval from disk and **re-approve the same
//! `ApprovalId`**.
//! - `approve_crash_intent` — dispatch a task requiring approval, then call
//!   `approve()` with the intent hook armed: `run_after_approval` runs the
//!   side effect (counter = 1), `record_intent` is fsynced, but the process
//!   aborts at the start of `record_committed` → **exits 137 in the
//!   INTENT-ONLY window** (intent on disk, committed + `pending.remove` not done).
//! - `approve_crash_committed` — as above but the crash hook aborts
//!   **after** `record_committed` (committed fsynced) but BEFORE
//!   `pending.remove` → COMMITTED window on the approval path.
//! - `approve_resume` — a fresh process loads the pending approval from the
//!   durable surface (Wire), picks up the **same** `ApprovalId` and re-approves
//!   it: after an intent-only crash the outbox sees `InProgress` →
//!   [`PolicyDenied`](familyclaw_actions::ActionError::PolicyDenied) fail-closed
//!   (counter stays at 1); after a committed crash the outbox sees `Committed`
//!   → value-identical outcome (counter stays at 1).
//!
//! ## Crash hook — UNREACHABLE in production (security justification)
//! Intent-only and committed crashes are implemented via the
//! [`CrashAfterIntentOutbox`] wrapper, which delegates to the real
//! [`JournalDispatchOutbox`], except that its `record_committed` **aborts the
//! process** when armed via either environment variable:
//! - [`CRASH_AFTER_INTENT_ENV`] → abort **BEFORE** delegating (intent on disk,
//!   committed not written = INTENT-ONLY window).
//! - [`CRASH_AFTER_COMMITTED_ENV`] → abort **AFTER** delegating (committed
//!   fsynced, but `pending.remove` not run = COMMITTED window on the
//!   approval path).
//!
//! Since both `submit_task_idempotent` and `approve` call `record_intent` →
//! side effect → `record_committed` in this order, aborting around
//! `record_committed` leaves the state in exactly the desired window.
//!
//! The hook is **doubly gated and cannot fire in production**:
//! 1. **Compilation boundary:** [`CrashAfterIntentOutbox`] is defined ONLY in
//!    this red-team binary (`src/bin/`), NOT in the library. Production code
//!    always builds its outbox from [`JournalDispatchOutbox`] or
//!    [`InMemoryDispatchOutbox`](familyclaw_actions::dispatch_outbox::InMemoryDispatchOutbox)
//!    — this wrapper type does not exist in the library API, so it is
//!    structurally impossible to instantiate in production.
//! 2. **Runtime gate:** even if the type somehow ended up in use, the abort
//!    only fires when [`CRASH_AFTER_INTENT_ENV`] **or**
//!    [`CRASH_AFTER_COMMITTED_ENV`] = `"1"`. No production path sets either
//!    variable.
//!
//! ## Determinism
//! The clock is injected via `--clock` — the system clock is never read.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use familyclaw_actions::dispatch_outbox::{
    DispatchLookup, DispatchOutboxStore, DispatchedOutcome, JournalDispatchOutbox,
};
use familyclaw_actions::executor::{ActionExecutor, ActionRequest, ActionResult};
use familyclaw_actions::manifest::{default_input_schema, SkillManifest};
use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
use familyclaw_actions::skills::Skill;
use familyclaw_actions::{ActionError, ActionRuntime, SkillId, SubmitOutcome};
use familyclaw_core::{time, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Skill that increments a counter.
///
/// Every `execute` increments a side-effect counter that lives **on disk** —
/// this is the gauge that reveals a double-fire across the process boundary.
///
/// The skill is intentionally **auto-run** ([`ActionRisk::ReadOnly`] +
/// [`ApprovalPolicy::AutoIfReadOnly`]), so that `submit_task` RUNS the executor
/// (= the side effect) immediately on the first call — instead of waiting for
/// approval. This way the "external side effect" happens measurably on every
/// `submit_task` run, and a double-fire shows up in the counter directly.
#[derive(Debug)]
struct CountingExecutor {
    /// Path where the side-effect counter lives (read + written on every run).
    counter_path: PathBuf,
    /// In-process counter (diagnostics only; the actual proof is on disk).
    in_process: AtomicU64,
}

impl CountingExecutor {
    /// Fixed identifier so that `start` and `resume` refer to the same skill.
    const SKILL_UUID: Uuid = uuid::uuid!("11111111-2222-4333-8444-555566667777");

    fn skill_id() -> SkillId {
        SkillId::from_uuid(Self::SKILL_UUID)
    }

    fn new(counter_path: PathBuf) -> Self {
        Self {
            counter_path,
            in_process: AtomicU64::new(0),
        }
    }

    /// Increments the on-disk side-effect counter atomically (read → +1 → write).
    fn bump_disk_counter(&self) {
        let current = std::fs::read_to_string(&self.counter_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let _ = std::fs::write(&self.counter_path, (current + 1).to_string());
    }
}

#[async_trait]
impl ActionExecutor for CountingExecutor {
    async fn execute(&self, request: ActionRequest) -> familyclaw_actions::Result<ActionResult> {
        // SIDE EFFECT: increment the counter. This is the "external effect" that
        // must happen exactly once across a SIGKILL.
        self.in_process.fetch_add(1, Ordering::SeqCst);
        self.bump_disk_counter();
        Ok(ActionResult::success(
            "counter bumped",
            serde_json::json!({ "ok": true }),
            request.now,
        ))
    }
}

impl Skill for CountingExecutor {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "counting_side_effect".to_string(),
            version: "1.0.0".to_string(),
            description: "Kasvattaa sivuvaikutuslaskuria (auto-run, suoritetaan heti).".to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: None,
            output_hint: None,
            input_schema: default_input_schema(),
            publisher: None,
            signature: None,
        }
    }
}

/// Counter skill that requires approval (approval path side effect).
///
/// Identical to [`CountingExecutor`] EXCEPT that its risk class is
/// [`ActionRisk::WriteExternal`] → `submit_task` leaves the task waiting for
/// human approval instead of running it immediately. The side effect (counter
/// increment) therefore only happens when [`ActionRuntime::approve`] runs the
/// [`run_after_approval`](familyclaw_actions) branch — exactly the window that
/// the at-most-once guarantee covers via the `approval-{id}` key.
///
/// The counter lives **on disk** using the same mechanism as
/// [`CountingExecutor`], so a double-fire across an approval shows up in the
/// counter directly (1 → 2).
#[derive(Debug)]
struct ApprovalCountingExecutor {
    /// Path where the side-effect counter lives (shared shape with [`CountingExecutor`]).
    counter_path: PathBuf,
    /// In-process counter (diagnostics only; the actual proof is on disk).
    in_process: AtomicU64,
}

impl ApprovalCountingExecutor {
    /// Fixed identifier (different from [`CountingExecutor`]'s), so the
    /// approval path's skill is unambiguous in the registry.
    const SKILL_UUID: Uuid = uuid::uuid!("99999999-8888-4777-8666-555544443333");

    fn skill_id() -> SkillId {
        SkillId::from_uuid(Self::SKILL_UUID)
    }

    fn new(counter_path: PathBuf) -> Self {
        Self {
            counter_path,
            in_process: AtomicU64::new(0),
        }
    }

    /// Increments the on-disk side-effect counter atomically (read → +1 → write).
    fn bump_disk_counter(&self) {
        let current = std::fs::read_to_string(&self.counter_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let _ = std::fs::write(&self.counter_path, (current + 1).to_string());
    }
}

#[async_trait]
impl ActionExecutor for ApprovalCountingExecutor {
    async fn execute(&self, request: ActionRequest) -> familyclaw_actions::Result<ActionResult> {
        // SIDE EFFECT: increment the counter. On the approval path this only
        // runs in `approve()`'s `run_after_approval` branch — this is the
        // "external effect" that must happen at most once across a SIGKILL.
        self.in_process.fetch_add(1, Ordering::SeqCst);
        self.bump_disk_counter();
        Ok(ActionResult::success(
            "counter bumped (approval path)",
            serde_json::json!({ "ok": true }),
            request.now,
        ))
    }
}

impl Skill for ApprovalCountingExecutor {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "counting_side_effect_approval".to_string(),
            version: "1.0.0".to_string(),
            description: "Kasvattaa sivuvaikutuslaskuria (vaatii hyväksynnän).".to_string(),
            permissions: vec![SkillPermission::WriteExternal],
            // WriteExternal → requires human approval (not auto-run).
            risk: ActionRisk::WriteExternal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: None,
            output_hint: None,
            input_schema: default_input_schema(),
            publisher: None,
            signature: None,
        }
    }
}

/// Environment variable that **arms** the intent-only crash hook.
///
/// Only when this is `"1"` does [`CrashAfterIntentOutbox::record_committed`]
/// abort the process before delegating. No production path sets this — see the
/// module documentation (compilation boundary + runtime gate).
const CRASH_AFTER_INTENT_ENV: &str = "FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT";

/// Environment variable that **arms** the committed-window crash hook.
///
/// Only when this is `"1"` does [`CrashAfterIntentOutbox::record_committed`]
/// abort the process **AFTER delegating** (committed is already fsynced to
/// disk) but before the caller gets to `pending.remove`. This mimics the
/// COMMITTED window on the approval path. No production path sets this — see
/// the module documentation (compilation boundary + runtime gate).
const CRASH_AFTER_COMMITTED_ENV: &str = "FAMILYCLAW_REDTEAM_CRASH_AFTER_COMMITTED";

/// Exit code with which an intent-only crash exits (SIGKILL-style, like 137).
const CRASH_EXIT_CODE: i32 = 137;

/// Crash hook wrapper that forces either the **intent-only** or
/// **committed window** across a process boundary.
///
/// Delegates everything to the real [`JournalDispatchOutbox`] EXCEPT that
/// [`record_committed`](CrashAfterIntentOutbox::record_committed) **aborts the
/// process** when the hook is armed:
/// - [`CRASH_AFTER_INTENT_ENV`] = `"1"` → abort **BEFORE** delegating: committed
///   is NEVER written (intent on disk, side effect fired, committed not
///   written) — the INTENT-ONLY window that GPT-5.5 raised.
/// - [`CRASH_AFTER_COMMITTED_ENV`] = `"1"` → abort **AFTER** delegating:
///   committed is already fsynced to disk but the caller doesn't get to
///   `pending.remove` → COMMITTED window (benign replay point, value-identical).
///
/// Since both [`ActionRuntime::submit_task_idempotent`] and
/// [`ActionRuntime::approve`] call `record_intent` → side effect →
/// `record_committed` in this order, aborting around `record_committed` leaves
/// the state in exactly the desired window.
///
/// ## Unreachable in production
/// This type lives ONLY in the red-team binary (`src/bin/`), not in the
/// library API. Production always builds its outbox directly from
/// [`JournalDispatchOutbox`], so this wrapper type cannot be instantiated in
/// production. In addition, the abort is gated by a runtime environment
/// variable. Double protection → cannot fire in production.
#[derive(Debug)]
struct CrashAfterIntentOutbox {
    /// The real crash-safe outbox to which all non-aborting calls are delegated.
    inner: JournalDispatchOutbox,
    /// Armed state for the intent-only window (abort BEFORE delegating).
    armed_before: bool,
    /// Armed state for the committed window (abort AFTER delegating).
    armed_after: bool,
}

impl CrashAfterIntentOutbox {
    /// Wraps the real outbox and reads the arming state from the environment ONCE.
    fn new(inner: JournalDispatchOutbox) -> Self {
        let armed_before = std::env::var(CRASH_AFTER_INTENT_ENV).as_deref() == Ok("1");
        let armed_after = std::env::var(CRASH_AFTER_COMMITTED_ENV).as_deref() == Ok("1");
        Self {
            inner,
            armed_before,
            armed_after,
        }
    }
}

impl DispatchOutboxStore for CrashAfterIntentOutbox {
    fn kind(&self) -> &'static str {
        // The wrapper delegates everything to the crash-safe outbox → same kind id.
        self.inner.kind()
    }

    fn lookup(&self, key: &str) -> familyclaw_actions::Result<DispatchLookup> {
        self.inner.lookup(key)
    }

    fn record_intent(&self, key: &str) -> familyclaw_actions::Result<()> {
        // The intent delegates normally (fsync) — this is the row that remains
        // on disk after an intent-only crash.
        self.inner.record_intent(key)
    }

    fn record_committed(
        &self,
        key: &str,
        outcome: &DispatchedOutcome,
    ) -> familyclaw_actions::Result<()> {
        if self.armed_before {
            // INTENT-ONLY WINDOW: record_intent is already on disk AND the side
            // effect has already fired (the caller ran it before this). Abort
            // BEFORE delegating → committed is NEVER written. This is the
            // genuinely dangerous window.
            //
            // `std::process::exit(137)` mimics SIGKILL — the library never sees
            // the committed row.
            let _ = std::io::stderr().flush();
            eprintln!(
                "crash injected: AFTER record_intent + side effect, \
                 BEFORE record_committed (intent-only window)"
            );
            // Use an explicit exit code so the test can assert on 137.
            std::process::exit(CRASH_EXIT_CODE);
        }
        // Committed is delegated to the real outbox (fsync). After this, the
        // committed marker is on disk — the at-most-once guarantee holds from
        // here on.
        self.inner.record_committed(key, outcome)?;
        if self.armed_after {
            // COMMITTED WINDOW: committed is already fsynced, but the caller
            // (e.g. `approve`) hasn't yet gotten to `pending.remove`. Abort here
            // → re-approval sees the Committed row and returns the
            // value-identical outcome without re-running the side effect.
            let _ = std::io::stderr().flush();
            eprintln!(
                "crash injected: AFTER record_committed (committed on disk), \
                 BEFORE pending.remove (committed window)"
            );
            std::process::exit(CRASH_EXIT_CODE);
        }
        Ok(())
    }
}

/// Mode: old (buggy) or new (fixed) dispatch path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum Mode {
    /// `submit_task_as` — NO outbox idempotency (the pre-fix bug).
    Old,
    /// `submit_task_idempotent` — outbox-protected (the fix).
    New,
}

/// Phase: crash mid-flight or resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum Phase {
    /// COMMITTED window: run the dispatch (intent + side effect + committed),
    /// record the outcome, exit 137 BEFORE the agent layer's journaling.
    Crash,
    /// INTENT-ONLY window: run the dispatch but abort at the start of
    /// `record_committed` → intent on disk + side effect fired, committed not
    /// written. Requires `--mode new` (outbox-protected path) + the armed hook.
    CrashIntent,
    /// After the COMMITTED window: re-run the SAME dispatch (the agent's
    /// fresh-run branch with no journal row). The outbox returns the committed
    /// outcome.
    Resume,
    /// After the INTENT-ONLY window: re-run the SAME dispatch → the outbox
    /// lookup returns `InProgress`, so the expected outcome is `PolicyDenied`
    /// fail-closed (the side effect does NOT re-run).
    ResumeIntent,
    /// CONTINUATION DISPATCH PATH, INTENT-ONLY window: run the post-approval
    /// continuation dispatch with the key `resume-{approval_id}-dispatch-{k}`
    /// (= exactly the key that production's `drive_tool_loop` builds AFTER an
    /// approval is granted). Intent hook armed → `record_intent` + side effect
    /// (counter = 1), abort at the start of `record_committed` → exits 137.
    /// Requires `--mode new` + `FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1`. The
    /// key is supplied via `--key` in the `resume-*-dispatch-*` shape.
    ResumeContinuationCrash,
    /// After the CONTINUATION DISPATCH PATH: a fresh process runs the SAME
    /// continuation dispatch with the SAME `resume-*-dispatch-*` key → the
    /// outbox lookup returns `InProgress`, so the expected outcome is
    /// `PolicyDenied` fail-closed (the side effect does NOT re-run, counter
    /// stays at 1).
    ResumeContinuationResume,
    /// APPROVAL PATH, INTENT-ONLY window: dispatch a task requiring approval,
    /// record the `ApprovalId` to disk, then call `approve()` with the intent
    /// hook armed → `run_after_approval` runs the side effect (counter = 1),
    /// `record_intent` is fsynced, the process aborts at the start of
    /// `record_committed` → exits 137. Requires `--mode new`, durable pending
    /// (`--pending`) + task queue (`--task-queue`) and
    /// `FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1`.
    ApproveCrashIntent,
    /// APPROVAL PATH, COMMITTED window: as above but the hook aborts **after**
    /// `record_committed` (committed on disk) before `pending.remove` → exits
    /// 137. Requires `FAMILYCLAW_REDTEAM_CRASH_AFTER_COMMITTED=1`.
    ApproveCrashCommitted,
    /// After the APPROVAL PATH: a fresh process loads the pending approval
    /// from the durable surface (Wire), picks up the SAME `ApprovalId` and
    /// re-approves it. After an intent-only crash → `PolicyDenied` fail-closed
    /// (counter stays at 1); after a committed crash → value-identical
    /// `SubmitOutcome` (counter stays at 1). The hook is NOT armed in this phase.
    ApproveResume,
}

/// Command-line interface.
#[derive(Parser)]
#[command(
    name = "dispatch_redteam",
    about = "FamilyClaw exactly-once dispatch black box"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The only subcommand: `run` (phases are distinguished by `--phase`).
#[derive(Subcommand)]
enum Command {
    /// Run a single phase in the given mode.
    Run(RunArgs),
}

/// `run` arguments.
#[derive(Parser)]
struct RunArgs {
    /// Old (buggy) or new (fixed) path.
    #[arg(long, value_enum)]
    mode: Mode,
    /// Phase (`crash` / `resume`).
    #[arg(long, value_enum)]
    phase: Phase,
    /// Path to the outbox journal (crash-safe idempotency).
    #[arg(long)]
    outbox: PathBuf,
    /// Path to the side-effect counter (proof).
    #[arg(long)]
    counter: PathBuf,
    /// File to which the `crash` phase records the outcome (proof of value identity).
    #[arg(long)]
    outcome_out: PathBuf,
    /// Stable idempotency key. For `submit_task` phases the shape is the
    /// agent's `turn-{turn}-dispatch-{k}`; for continuation dispatch path
    /// phases (`resume_continuation_*`) the shape is
    /// `resume-{approval_id}-dispatch-{k}`, exactly what production's
    /// `drive_tool_loop` builds after approval.
    #[arg(long, default_value = "turn-0-dispatch-0")]
    key: String,
    /// Injected wall clock (RFC 3339).
    #[arg(long)]
    clock: String,
    /// Path to the crash-safe **pending approvals** surface (Wire stage).
    /// Required for approval-path phases (`approve_*`).
    #[arg(long)]
    pending: Option<PathBuf>,
    /// Path to the crash-safe **task queue** (durable queue). Required for
    /// approval-path phases (`approve_*`).
    #[arg(long)]
    task_queue: Option<PathBuf>,
}

/// On-disk shape of the outcome, for value-identity comparison.
#[derive(Debug, Serialize, Deserialize)]
struct OutcomeRecord {
    task_id: String,
    pending_approval: Option<String>,
    status: String,
}

impl OutcomeRecord {
    fn from_submit(outcome: &SubmitOutcome) -> Self {
        Self {
            task_id: outcome.task_id.to_string(),
            pending_approval: outcome.pending_approval.map(|a| a.to_string()),
            status: format!("{:?}", outcome.status),
        }
    }
}

/// Daemon error type.
#[derive(Debug, thiserror::Error)]
enum HarnessError {
    #[error("core error: {0}")]
    Core(#[from] familyclaw_core::FamilyClawError),
    #[error("action error: {0}")]
    Action(#[from] familyclaw_actions::ActionError),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

type HarnessResult<T> = std::result::Result<T, HarnessError>;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "dispatch_redteam error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> HarnessResult<()> {
    match cli.command {
        Command::Run(args) => run_phase(args).await,
    }
}

/// Builds the runtime: counter skill registered + (in new mode) the crash-safe
/// outbox from the given path.
///
/// In the `crash_intent` and `resume_continuation_crash` phases the crash-safe
/// outbox is wrapped in [`CrashAfterIntentOutbox`], which aborts the process at
/// the start of `record_committed` (intent-only window). All other phases use
/// the plain [`JournalDispatchOutbox`].
fn build_runtime(args: &RunArgs) -> HarnessResult<ActionRuntime> {
    let mut runtime = ActionRuntime::new();
    runtime.register_skill(CountingExecutor::new(args.counter.clone()))?;
    if args.mode == Mode::New {
        let outbox = JournalDispatchOutbox::open(&args.outbox)?;
        // The intent-only crash window phases (with both `turn-*` and
        // `resume-*-dispatch-*` keys) need the outbox wrapped in the crash
        // hook: `record_committed` aborts before delegating when armed.
        if matches!(
            args.phase,
            Phase::CrashIntent | Phase::ResumeContinuationCrash
        ) {
            runtime = runtime.with_dispatch_outbox(Box::new(CrashAfterIntentOutbox::new(outbox)));
        } else {
            runtime = runtime.with_dispatch_outbox(Box::new(outbox));
        }
    }
    Ok(runtime)
}

/// Enforces a required path argument (for approval-path phases).
fn require_path<'a>(value: Option<&'a PathBuf>, flag: &str) -> HarnessResult<&'a PathBuf> {
    value.ok_or_else(|| {
        HarnessError::Io(std::io::Error::other(format!(
            "hyväksyntäpolun vaihe vaatii argumentin {flag}"
        )))
    })
}

/// Builds the **crash-safe** runtime for the approval path: durable pending
/// (Wire) + durable task queue + crash-safe dispatch outbox.
///
/// This is the configuration that lets a fresh process load the pending
/// approval from disk ([`ActionRuntime::with_durable_stores`]) and re-approve
/// the SAME `ApprovalId` under at-most-once protection (the `approval-{id}`
/// key). In crash phases the outbox is wrapped in [`CrashAfterIntentOutbox`]
/// (intent or committed window depending on the environment variable); in the
/// resume phase the plain [`JournalDispatchOutbox`] is used.
async fn build_approval_runtime(args: &RunArgs) -> HarnessResult<ActionRuntime> {
    let pending = require_path(args.pending.as_ref(), "--pending")?;
    let task_queue = require_path(args.task_queue.as_ref(), "--task-queue")?;

    // `with_durable_stores` now itself opens the crash-safe journal outbox from
    // the given path (`args.outbox`), so the resume phase gets the crash-safe
    // outbox DIRECTLY without a separate open or chaining.
    let mut runtime = ActionRuntime::with_durable_stores(pending, task_queue, &args.outbox).await?;

    // Crash phases need the outbox WRAPPED with the crash hook (aborts around
    // record_committed). The constructor's default outbox can't do this, so
    // replace it explicitly with `with_dispatch_outbox` — this is exactly the
    // special case the override hook exists for. The outbox is opened a second
    // time from the SAME path, but the journal is an idempotent append-only
    // log (no truncate), so the double-open is harmless; this is a test binary,
    // not a production path (build_family/gateway do not double-open).
    if matches!(
        args.phase,
        Phase::ApproveCrashIntent | Phase::ApproveCrashCommitted
    ) {
        let outbox = JournalDispatchOutbox::open(&args.outbox)?;
        let wrapped: Box<dyn DispatchOutboxStore> = Box::new(CrashAfterIntentOutbox::new(outbox));
        runtime = runtime.with_dispatch_outbox(wrapped);
    }

    runtime.register_skill(ApprovalCountingExecutor::new(args.counter.clone()))?;
    Ok(runtime)
}

/// Runs the dispatch on the selected path (old vs new).
async fn dispatch(
    runtime: &mut ActionRuntime,
    args: &RunArgs,
    now: Timestamp,
) -> familyclaw_actions::Result<SubmitOutcome> {
    let payload = serde_json::json!({ "n": 1 });
    match args.mode {
        // OLD path: direct `submit_task_as` without an idempotency key. This is
        // the code that existed before the fix — it has NO outbox protection,
        // so a re-drive after a crash re-runs the side effect.
        Mode::Old => {
            runtime
                .submit_task_as("agent_a", CountingExecutor::skill_id(), payload, now)
                .await
        }
        // NEW path: idempotent dispatch with a stable key. The outbox returns
        // the committed outcome without re-running the side effect.
        Mode::New => {
            runtime
                .submit_task_idempotent(
                    &args.key,
                    "agent_a",
                    CountingExecutor::skill_id(),
                    payload,
                    now,
                )
                .await
        }
    }
}

async fn run_phase(args: RunArgs) -> HarnessResult<()> {
    let now = time::parse_rfc3339(&args.clock)?;

    // Approval-path phases use a different (crash-safe) configuration and a
    // separate skill → branch them off BEFORE building the submit-path runtime.
    if matches!(
        args.phase,
        Phase::ApproveCrashIntent | Phase::ApproveCrashCommitted | Phase::ApproveResume
    ) {
        return run_approval_phase(args, now).await;
    }

    let mut runtime = build_runtime(&args)?;

    match args.phase {
        Phase::ApproveCrashIntent | Phase::ApproveCrashCommitted | Phase::ApproveResume => {
            // These already branched off to `run_approval_phase` above; this
            // point should never be reached. Fail loudly rather than panicking.
            Err(HarnessError::Io(std::io::Error::other(
                "approval phase reached submit-path match — internal routing error",
            )))
        }
        Phase::Crash => {
            // COMMITTED window. The dispatch runs to completion (intent + side
            // effect + committed). Record the outcome for value-identity
            // comparison and exit 137 BEFORE the agent gets a chance to journal
            // the dispatch row.
            let outcome = dispatch(&mut runtime, &args, now).await?;
            write_outcome(&args.outcome_out, &outcome)?;
            eprintln!("crash injected: after committed, before dispatch journal append");
            std::process::exit(CRASH_EXIT_CODE);
        }
        Phase::CrashIntent => {
            // INTENT-ONLY window. `dispatch` never returns: the crash hook
            // aborts the process at the start of `record_committed` —
            // record_intent is already on disk and the side effect has already
            // fired. If the hook is NOT armed (the environment variable is
            // missing), this is a programming error — don't silently "succeed",
            // fail loudly.
            let _ = dispatch(&mut runtime, &args, now).await?;
            Err(HarnessError::Io(std::io::Error::other(
                "crash_intent phase returned without aborting — \
                 is FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1 set?",
            )))
        }
        Phase::Resume => {
            // After the COMMITTED window: re-run of the agent's fresh-run branch
            // (no journal row, since the crash prevented it). In `new` mode the
            // outbox neutralizes it; in `old` mode the side effect happens again.
            let outcome = dispatch(&mut runtime, &args, now).await?;
            let before = read_outcome(&args.outcome_out);
            let now_record = OutcomeRecord::from_submit(&outcome);
            let value_identical = before
                .as_ref()
                .is_some_and(|b| b.task_id == now_record.task_id);
            let result = serde_json::json!({
                "side_effect_count": read_counter(&args.counter),
                "value_identical": value_identical,
                "resumed_task_id": now_record.task_id,
                "crashed_task_id": before.map(|b| b.task_id),
            });
            println!("RESULT {result}");
            std::io::stdout().flush()?;
            Ok(())
        }
        Phase::ResumeIntent => {
            // After the INTENT-ONLY window: run the SAME dispatch with the same
            // key. The outbox lookup sees the intent without a committed →
            // InProgress → submit_task_idempotent returns PolicyDenied
            // fail-closed. Do NOT re-run the side effect. This is the core of
            // the at-most-once guarantee.
            let dispatch_result = dispatch(&mut runtime, &args, now).await;
            let policy_denied = matches!(dispatch_result, Err(ActionError::PolicyDenied(_)));
            let denied_message = match &dispatch_result {
                Err(ActionError::PolicyDenied(msg)) => Some(msg.clone()),
                _ => None,
            };
            // Print a single-line RESULT JSON for the harness (counter MUST stay at 1).
            let result = serde_json::json!({
                "side_effect_count": read_counter(&args.counter),
                "policy_denied": policy_denied,
                "denied_message": denied_message,
                "other_outcome": dispatch_result.ok().map(|o| OutcomeRecord::from_submit(&o).task_id),
            });
            println!("RESULT {result}");
            std::io::stdout().flush()?;
            Ok(())
        }
        Phase::ResumeContinuationCrash => {
            // CONTINUATION DISPATCH, INTENT-ONLY window. Identical to
            // `CrashIntent` EXCEPT that the idempotency key is
            // `resume-{approval_id}-dispatch-{k}` (supplied via `--key`),
            // instead of `turn-*`. This is exactly the key that production's
            // `drive_tool_loop` builds AFTER an approval is granted (from the
            // prefix `resume-{approval_id}` + a running dispatch index).
            // `dispatch` never returns: the crash hook aborts the process at
            // the start of `record_committed` — `record_intent` is already on
            // disk and the side effect has already fired. If the hook is NOT
            // armed, that's a programming error → fail loudly.
            assert_resume_key_shape(&args.key)?;
            let _ = dispatch(&mut runtime, &args, now).await?;
            Err(HarnessError::Io(std::io::Error::other(
                "resume_continuation_crash phase returned without aborting — \
                 is FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1 set?",
            )))
        }
        Phase::ResumeContinuationResume => {
            // CONTINUATION DISPATCH, after the INTENT-ONLY window: a fresh
            // process runs the SAME continuation dispatch with the SAME
            // `resume-*-dispatch-*` key. The outbox lookup sees the intent
            // without a committed → InProgress → submit_task_idempotent
            // returns PolicyDenied fail-closed. The side effect does NOT
            // re-run → at-most-once for the continuation dispatch key.
            assert_resume_key_shape(&args.key)?;
            let dispatch_result = dispatch(&mut runtime, &args, now).await;
            let policy_denied = matches!(dispatch_result, Err(ActionError::PolicyDenied(_)));
            let denied_message = match &dispatch_result {
                Err(ActionError::PolicyDenied(msg)) => Some(msg.clone()),
                _ => None,
            };
            let result = serde_json::json!({
                "side_effect_count": read_counter(&args.counter),
                "policy_denied": policy_denied,
                "denied_message": denied_message,
                "dispatch_key": args.key,
                "other_outcome": dispatch_result.ok().map(|o| OutcomeRecord::from_submit(&o).task_id),
            });
            println!("RESULT {result}");
            std::io::stdout().flush()?;
            Ok(())
        }
    }
}

/// Ensures that the continuation dispatch key has the shape
/// `resume-{id}-dispatch-{k}`.
///
/// This is the same key shape that production's `drive_tool_loop` builds
/// AFTER an approval is granted (from the prefix `resume-{approval_id}` +
/// `-dispatch-{k}`). This guard keeps the red-team phases honest: if the key
/// doesn't follow the shape, the proof wouldn't actually apply to the
/// continuation dispatch key — fail loudly rather than silently.
fn assert_resume_key_shape(key: &str) -> HarnessResult<()> {
    if key.starts_with("resume-") && key.contains("-dispatch-") {
        return Ok(());
    }
    Err(HarnessError::Io(std::io::Error::other(format!(
        "resume_continuation phase requires a key of shape \
         'resume-{{id}}-dispatch-{{k}}' (the exact shape drive_tool_loop builds \
         after approval); got {key:?}"
    ))))
}

/// Runs the **approval path** phases across a real process boundary.
///
/// All three phases share the same crash-safe configuration
/// ([`build_approval_runtime`]): durable pending (Wire) + durable task queue +
/// crash-safe dispatch outbox. The idempotency key is `approval-{id}` (NOT
/// `turn-*`).
///
/// - `approve_crash_intent` / `approve_crash_committed`: dispatch a task
///   requiring approval, record the `ApprovalId` + outcome to disk, then call
///   `approve()` with the hook armed → the process aborts around
///   `record_committed` (intent or committed window) and exits 137.
/// - `approve_resume`: load the pending approval from the durable surface,
///   pick up the SAME `ApprovalId` and re-approve it. Print a single-line
///   RESULT JSON.
async fn run_approval_phase(args: RunArgs, now: Timestamp) -> HarnessResult<()> {
    let mut runtime = build_approval_runtime(&args).await?;

    match args.phase {
        Phase::ApproveCrashIntent | Phase::ApproveCrashCommitted => {
            // 1) Dispatch a task requiring approval (WriteExternal → NeedsApproval).
            //    The side effect does NOT fire yet — it waits for approval.
            let submitted = runtime
                .submit_task_as(
                    "agent_a",
                    ApprovalCountingExecutor::skill_id(),
                    serde_json::json!({ "n": 1 }),
                    now,
                )
                .await?;
            let approval_id = submitted.pending_approval.ok_or_else(|| {
                HarnessError::Io(std::io::Error::other(
                    "submit ei jättänyt tehtävää odottamaan hyväksyntää \
                     (odotettiin NeedsApproval)",
                ))
            })?;
            // Record the outcome + ApprovalId to disk: the resume phase compares
            // against this (value identity) and verifies that the SAME approval
            // was loaded.
            write_outcome(&args.outcome_out, &submitted)?;

            // 2) Approve → run_after_approval runs the side effect (counter = 1),
            //    record_intent is fsynced, then the crash hook aborts around
            //    record_committed. `approve` doesn't return normally.
            let _ = runtime.approve(approval_id, now).await?;
            // If the hook was NOT armed, execution reaches here → programming error.
            Err(HarnessError::Io(std::io::Error::other(
                "approve crash phase returned without aborting — is \
                 FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT / _AFTER_COMMITTED=1 set?",
            )))
        }
        Phase::ApproveResume => {
            // Load the pending approval from the durable surface (Wire stage):
            // this is the point where the SAME ApprovalId is reconstructed from
            // disk in the new process.
            let pending = runtime.try_pending_approvals()?;
            let approval_id = pending.first().map(|p| p.approval_id).ok_or_else(|| {
                HarnessError::Io(std::io::Error::other(
                    "tuore prosessi ei löytänyt odottavaa hyväksyntää durable-pinnalta \
                     (Wire-vaihe rikki?)",
                ))
            })?;

            // Re-approve the SAME ApprovalId → outbox key approval-{id}.
            // After an intent-only crash: InProgress → PolicyDenied.
            // After a committed crash: Committed → value-identical outcome.
            let approve_result = runtime.approve(approval_id, now).await;
            let policy_denied = matches!(approve_result, Err(ActionError::PolicyDenied(_)));
            let denied_message = match &approve_result {
                Err(ActionError::PolicyDenied(msg)) => Some(msg.clone()),
                _ => None,
            };

            // Value identity for the committed window: compare against the
            // outcome recorded before the crash.
            let before = read_outcome(&args.outcome_out);
            let resumed = approve_result.as_ref().ok().map(OutcomeRecord::from_submit);
            let value_identical = match (&before, &resumed) {
                (Some(b), Some(r)) => b.task_id == r.task_id,
                _ => false,
            };

            let result = serde_json::json!({
                "side_effect_count": read_counter(&args.counter),
                "policy_denied": policy_denied,
                "denied_message": denied_message,
                "value_identical": value_identical,
                "reloaded_approval_id": approval_id.to_string(),
                "resumed_task_id": resumed.as_ref().map(|r| r.task_id.clone()),
                "resumed_status": resumed.as_ref().map(|r| r.status.clone()),
            });
            println!("RESULT {result}");
            std::io::stdout().flush()?;
            Ok(())
        }
        // Submit and continuation path phases never end up here (already
        // branched off in run_phase).
        Phase::Crash
        | Phase::CrashIntent
        | Phase::Resume
        | Phase::ResumeIntent
        | Phase::ResumeContinuationCrash
        | Phase::ResumeContinuationResume => Err(HarnessError::Io(std::io::Error::other(
            "submit/continuation phase reached approval-path handler — internal routing error",
        ))),
    }
}

/// Writes the outcome to disk (proof of value identity).
fn write_outcome(path: &Path, outcome: &SubmitOutcome) -> HarnessResult<()> {
    let record = OutcomeRecord::from_submit(outcome);
    std::fs::write(path, serde_json::to_string(&record)?)?;
    Ok(())
}

/// Reads a previously recorded outcome (if any).
fn read_outcome(path: &Path) -> Option<OutcomeRecord> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Reads the side-effect counter raw (0 if the file doesn't exist).
fn read_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}
