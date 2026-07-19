//! RED-TEAM: exactly-once dispatch across a SIGKILL mid-dispatch.
//!
//! Attack (GPT-5.5's discovery): the agent layer runs [`ActionRuntime`]'s
//! side effect (`submit_task`) BEFORE it journals the dispatch row. If the
//! process is killed (SIGKILL) within that window — the side effect has
//! already happened, the journal row does not exist — replay/restart thinks
//! the step never ran and **re-runs the side effect** (double-fire).
//!
//! This is run **across a genuine process boundary**: a separate
//! `dispatch_redteam` process exits with exit code 137 (SIGKILL-style) right
//! in that window, and a second process attempts to resume. The clock is
//! injected → deterministic.
//!
//! ## Two windows, both proven across the process boundary
//! - **COMMITTED window** (`crash` → `resume`): the outbox has already been
//!   fully written (intent + committed), only the agent layer's journal row
//!   is missing. Replay returns a **value-identical** outcome without
//!   re-running the side effect → **exactly-once**.
//! - **INTENT-ONLY window** (`crash_intent` → `resume_intent`): the process
//!   is killed after `record_intent` AND the side effect but BEFORE
//!   `record_committed`. Replay sees `InProgress` → `submit_task_idempotent`
//!   returns `PolicyDenied` fail-closed, and the side effect does not re-run
//!   → **at-most-once**. This is the genuinely dangerous window that GPT-5.5
//!   raised; previously it had only been proven by an in-process unit test.
//!
//! ## Three claims
//! - **Old path (`--mode old`, `submit_task_as` without an outbox):** the bug
//!   DOES exist → side-effect counter = 2 (double-fire), outcome not
//!   identical. This proves the test DOES catch the bug (would fail on
//!   unfixed code).
//! - **New path, COMMITTED window (`--mode new`, `submit_task_idempotent` +
//!   crash-resilient outbox):** bug FIXED → side-effect counter = 1 (exactly
//!   once), and the resumed outcome is **value-identical** to the crashed one
//!   (same `task_id`).
//! - **New path, INTENT-ONLY window:** crash after the intent but before the
//!   committed record → replay is `PolicyDenied` and the counter stays at 1
//!   (at-most-once).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locates the `dispatch_redteam` binary from the same profile directory.
fn harness_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let deps = exe.parent().expect("deps dir");
    let profile_dir = deps.parent().expect("profile dir");
    let mut bin = profile_dir.join("dispatch_redteam");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    assert!(
        bin.exists(),
        "dispatch_redteam binary not found at {} — build it first \
         (cargo build -p familyclaw-actions --bin dispatch_redteam)",
        bin.display()
    );
    bin
}

/// Fixed injected clock (RFC 3339) — for reproducibility.
const CLOCK: &str = "2024-05-29T18:13:20+00:00"; // = unix 1_717_000_000

/// Unique temp directory for this attack run.
fn tempdir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "familyclaw-redteam-dispatch-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Runs the harness process and returns (`exit_ok`, stdout, stderr).
fn run(bin: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin)
        .args(args)
        .output()
        .expect("spawn harness");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Runs the harness process with the given environment variables and returns
/// (`exit_code`, `exit_ok`, stdout, stderr).
///
/// Needed for the intent-only crash, which is armed via
/// `FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1`. `exit_code` is returned
/// separately so that we can require exactly 137 (SIGKILL-style).
fn run_env(
    bin: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (Option<i32>, bool, String, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn harness");
    (
        out.status.code(),
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Extracts the stdout's `RESULT <json>` line, parsed as a Value.
fn result_json(stdout: &str) -> serde_json::Value {
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("RESULT "))
        .unwrap_or_else(|| panic!("no RESULT line in stdout: {stdout:?}"));
    serde_json::from_str(line).expect("parse RESULT json")
}

/// Builds the arguments for a single `run` phase.
fn phase_args<'a>(
    mode: &'a str,
    phase: &'a str,
    outbox: &'a str,
    counter: &'a str,
    outcome: &'a str,
) -> Vec<&'a str> {
    vec![
        "run",
        "--mode",
        mode,
        "--phase",
        phase,
        "--outbox",
        outbox,
        "--counter",
        counter,
        "--outcome-out",
        outcome,
        "--clock",
        CLOCK,
    ]
}

/// Reads the side-effect counter raw (0 if the file does not exist).
fn read_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// **OLD path proves the bug:** `submit_task_as` without an outbox → crash
/// mid-dispatch + re-drive RE-RUNS THE SIDE EFFECT (double-fire).
///
/// This test VERIFIES that the red-team harness genuinely exposes the bug: if
/// the fix were removed (reverting to `submit_task_as`), the new-path test
/// would fail — here we prove that the old code produces a counter of 2.
#[test]
fn old_path_double_fires_side_effect_across_crash() {
    let bin = harness_bin();
    let dir = tempdir("old");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let (ob, ct, oc) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
    );

    // Phase 1 (crash): run the dispatch (side effect +1), exit 137 before journaling.
    let (ok1, _o1, e1) = run(&bin, &phase_args("old", "crash", &ob, &ct, &oc));
    assert!(!ok1, "crash phase must exit non-zero. stderr={e1}");
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect ran once before crash"
    );

    // Phase 2 (resume): re-drive the SAME dispatch — no idempotence on the old path.
    let (ok2, o2, e2) = run(&bin, &phase_args("old", "resume", &ob, &ct, &oc));
    assert!(ok2, "resume phase must succeed. stderr={e2}");
    let report = result_json(&o2);
    eprintln!("[old resume] {report}");

    // PROOF OF THE BUG: the side effect fired TWICE (crash window + re-drive).
    assert_eq!(
        report["side_effect_count"], 2,
        "OLD path: side effect double-fires across the crash window (THIS is the bug)"
    );
    assert_eq!(
        read_counter(&counter),
        2,
        "disk counter confirms double-fire"
    );
    // And the outcome is not identical (a new random task_id on re-drive).
    assert_eq!(
        report["value_identical"],
        serde_json::Value::Bool(false),
        "OLD path re-run produces a DIFFERENT task_id (not value-identical)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **NEW path proves the fix:** `submit_task_idempotent` + crash-resilient
/// outbox → crash mid-dispatch + re-drive does NOT re-run the side effect,
/// and the resumed outcome is value-identical to the crashed one.
#[test]
fn new_path_side_effect_exactly_once_and_value_identical() {
    let bin = harness_bin();
    let dir = tempdir("new");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let (ob, ct, oc) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
    );

    // Phase 1 (crash): run the idempotent dispatch (intent + side effect + committed),
    // exit 137 before the agent layer can journal the dispatch row.
    let (ok1, _o1, e1) = run(&bin, &phase_args("new", "crash", &ob, &ct, &oc));
    assert!(!ok1, "crash phase must exit non-zero. stderr={e1}");
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect ran once before crash"
    );

    // Phase 2 (resume): re-drive the SAME dispatch with the same idempotence key.
    // The outbox returns the committed outcome without re-running the side effect.
    let (ok2, o2, e2) = run(&bin, &phase_args("new", "resume", &ob, &ct, &oc));
    assert!(ok2, "resume phase must succeed. stderr={e2}");
    let report = result_json(&o2);
    eprintln!("[new resume] {report}");

    // PROOF OF THE FIX 1 (exactly-once): the side effect fired EXACTLY ONCE.
    assert_eq!(
        report["side_effect_count"], 1,
        "NEW path: side effect must NOT re-fire after the crash (exactly-once)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms side effect ran exactly once"
    );

    // PROOF OF THE FIX 2 (value identity): resumed outcome = crashed outcome.
    assert_eq!(
        report["value_identical"],
        serde_json::Value::Bool(true),
        "NEW path resume must return the value-identical SubmitOutcome (same task_id)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Environment variable that arms the intent-only crash hook in the harness binary.
const CRASH_AFTER_INTENT_ENV: &str = "FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT";

/// Reads the outbox journal raw (empty if the file does not exist).
fn read_outbox(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// **INTENT-ONLY WINDOW proves at-most-once fail-closed across a GENUINE process boundary.**
///
/// This closes the caveat that GPT-5.5's adversarial review raised: the earlier
/// `crash` phase only exited AFTER `record_committed` (a benign committed-replay
/// case). The genuinely dangerous window is after `record_intent` AND the side
/// effect but BEFORE `record_committed` — there, replay MUST return
/// `InProgress` → `PolicyDenied`, and the side effect must not fire again.
///
/// Phase 1 (`crash_intent`, hook armed): `record_intent` to disk, the side effect
/// fires (counter = 1), then the process aborts at the start of `record_committed`
/// → exits 137. On disk: intent marker PRESENT, committed marker ABSENT.
///
/// Phase 2 (`resume_intent`, same key): the outbox lookup sees the intent without
/// a committed record → `InProgress` → `submit_task_idempotent` returns
/// `PolicyDenied`. The counter STAYS at 1 (the side effect does NOT re-run) →
/// at-most-once.
///
/// The test would fail if the ordering were wrong: if `record_intent` came
/// AFTER the side effect, replay would not detect the in-progress state and
/// would double-fire (or silently re-run) → this test catches that.
#[test]
fn intent_window_crash_is_at_most_once() {
    let bin = harness_bin();
    let dir = tempdir("intent-window");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let (ob, ct, oc) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
    );

    // Phase 1 (crash_intent): armed hook → abort at the start of record_committed.
    let (code1, ok1, _o1, e1) = run_env(
        &bin,
        &phase_args("new", "crash_intent", &ob, &ct, &oc),
        &[(CRASH_AFTER_INTENT_ENV, "1")],
    );
    assert!(
        !ok1,
        "crash_intent phase must NOT exit success. stderr={e1}"
    );
    assert_eq!(
        code1,
        Some(137),
        "crash_intent must exit 137 (SIGKILL-style) from the crash hook. stderr={e1}"
    );

    // The side effect fired EXACTLY ONCE before the crash (CountingExecutor's external
    // on-disk marker). This proves the crash happened AFTER the side effect.
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect fired exactly once before the intent-only crash"
    );

    // On disk: intent marker PRESENT, committed marker ABSENT → the
    // intent-only state is verified across a genuine process boundary (not an in-process simulation).
    let on_disk = read_outbox(&outbox);
    assert!(
        on_disk.contains("dispatch_intent"),
        "intent marker must be present on disk after intent-only crash. disk={on_disk:?}"
    );
    assert!(
        !on_disk.contains("dispatch_committed"),
        "committed marker must be ABSENT on disk (crash hit before record_committed). \
         disk={on_disk:?}"
    );

    // Phase 2 (resume_intent): fresh process, same key → InProgress →
    // PolicyDenied fail-closed, and the side effect does not re-run.
    let (code2, ok2, o2, e2) = run_env(
        &bin,
        &phase_args("new", "resume_intent", &ob, &ct, &oc),
        // NOT armed on resume — the hook is not used in the resume_intent phase.
        &[],
    );
    assert!(
        ok2,
        "resume_intent phase must exit success (code={code2:?}). stderr={e2}"
    );
    let report = result_json(&o2);
    eprintln!("[intent-window resume] {report}");

    // AT-MOST-ONCE PROOF 1: replay is PolicyDenied fail-closed (not a silent re-run).
    assert_eq!(
        report["policy_denied"],
        serde_json::Value::Bool(true),
        "INTENT-ONLY replay must be PolicyDenied (fail-closed), not a silent re-run"
    );

    // AT-MOST-ONCE PROOF 2: the counter STAYS at 1 — the side effect did NOT fire again.
    assert_eq!(
        report["side_effect_count"], 1,
        "INTENT-ONLY replay must NOT re-fire the side effect (at-most-once)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms the side effect fired AT MOST once (1, never 2)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Builds the `run` phase arguments with an explicit idempotence key.
///
/// `phase_args` uses the binary's default key (`turn-0-dispatch-0`); the
/// continuation dispatch path's (`resume_continuation_*`) phases need a key
/// of the form `resume-{id}-dispatch-{k}`, so those are given here explicitly
/// via `--key`.
fn phase_args_keyed<'a>(
    mode: &'a str,
    phase: &'a str,
    outbox: &'a str,
    counter: &'a str,
    outcome: &'a str,
    key: &'a str,
) -> Vec<&'a str> {
    let mut args = phase_args(mode, phase, outbox, counter, outcome);
    args.push("--key");
    args.push(key);
    args
}

/// Continuation dispatch key — exactly the form that production's `drive_tool_loop`
/// builds AFTER an approval is granted (`resume-{approval_id}-dispatch-{k}`).
/// `approval_id` is a fixed UUID here for determinism.
const RESUME_KEY: &str = "resume-00000000-0000-4000-8000-0000000000ab-dispatch-0";

/// **CONTINUATION DISPATCH PATH, INTENT-ONLY WINDOW proves at-most-once fail-closed
/// across a genuine process boundary — with the key `resume-{id}-dispatch-{k}` (NOT
/// `turn-*` and not `approval-*`).**
///
/// This closes the last caveat: earlier cross-process proofs covered
/// `submit_task` keys (`turn-*`) and approval keys (`approval-{id}`), BUT NOT
/// the post-approval continuation dispatch key. Scenario: an interrupted turn
/// is approved → the model requests ANOTHER tool on continuation → crash inside
/// that second tool's dispatch window (intent on disk + side effect fired,
/// committed not written) → a fresh process resumes → the side effect must NOT
/// fire again.
///
/// In production this dispatch is run by `drive_tool_loop`, which builds the
/// key `{prefix}-dispatch-{k}` from the prefix `resume-{approval_id}` (see
/// `agent.rs` around lines 2129 and 1798). Here the key is built directly in
/// this same form — that is exactly what matters for proving the outbox dedup.
///
/// Phase 1 (`resume_continuation_crash`, hook armed): `record_intent` is
/// fsynced with the resume key, the side effect fires (counter = 1), then the
/// process aborts at the start of `record_committed` → exits 137. On disk:
/// intent marker PRESENT with key `resume-*`, committed marker ABSENT.
///
/// Phase 2 (`resume_continuation_resume`, same key): the outbox lookup sees
/// the intent without a committed record → `InProgress` →
/// `submit_task_idempotent` returns `PolicyDenied`. The counter STAYS at 1 →
/// at-most-once.
///
/// **Mutation proof:** this specifically proves the idempotence of the resume
/// key — if the continuation dispatch used `submit_task_as` instead of
/// `submit_task_idempotent` (i.e. no outbox key at all), the re-drive would
/// re-run the side effect → counter = 2 and `policy_denied = false`. Compare
/// the `--mode old` contrast phase below, which runs exactly that
/// non-idempotent path with the same resume key and double-fires.
#[test]
fn resume_continuation_intent_crash_is_at_most_once() {
    let bin = harness_bin();
    let dir = tempdir("resume-continuation-intent");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let (ob, ct, oc) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
    );

    // Phase 1 (resume_continuation_crash): armed hook → abort at the start of
    // record_committed, with the resume key.
    let (code1, ok1, _o1, e1) = run_env(
        &bin,
        &phase_args_keyed(
            "new",
            "resume_continuation_crash",
            &ob,
            &ct,
            &oc,
            RESUME_KEY,
        ),
        &[(CRASH_AFTER_INTENT_ENV, "1")],
    );
    assert!(
        !ok1,
        "resume_continuation_crash phase must NOT exit success. stderr={e1}"
    );
    assert_eq!(
        code1,
        Some(137),
        "resume_continuation_crash must exit 137 (SIGKILL-style) from the crash hook. stderr={e1}"
    );

    // The side effect fired EXACTLY ONCE before the crash.
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect fired exactly once before the resume-continuation intent-only crash"
    );

    // On disk: intent marker PRESENT with key resume-*, committed marker ABSENT.
    let on_disk = read_outbox(&outbox);
    assert!(
        on_disk.contains("dispatch_intent"),
        "intent marker must be present on disk after resume-continuation intent-only crash. \
         disk={on_disk:?}"
    );
    assert!(
        on_disk.contains("resume-") && on_disk.contains("-dispatch-"),
        "outbox key must be resume-*-dispatch-* (NOT turn-* nor approval-*) on the \
         resume-continuation path. disk={on_disk:?}"
    );
    assert!(
        !on_disk.contains("dispatch_committed"),
        "committed marker must be ABSENT on disk (crash hit before record_committed). \
         disk={on_disk:?}"
    );

    // Phase 2 (resume_continuation_resume): fresh process, same resume key →
    // InProgress → PolicyDenied fail-closed, and the side effect does not re-run.
    let (code2, ok2, o2, e2) = run_env(
        &bin,
        &phase_args_keyed(
            "new",
            "resume_continuation_resume",
            &ob,
            &ct,
            &oc,
            RESUME_KEY,
        ),
        // NOT armed on resume — the hook is not used.
        &[],
    );
    assert!(
        ok2,
        "resume_continuation_resume phase must exit success (code={code2:?}). stderr={e2}"
    );
    let report = result_json(&o2);
    eprintln!("[resume-continuation resume] {report}");

    // AT-MOST-ONCE PROOF 1: replay is PolicyDenied fail-closed (not a silent re-run).
    assert_eq!(
        report["policy_denied"],
        serde_json::Value::Bool(true),
        "RESUME-CONTINUATION intent-only replay must be PolicyDenied (fail-closed), \
         not a silent re-run"
    );

    // AT-MOST-ONCE PROOF 2: the counter STAYS at 1 — the side effect did NOT fire again.
    assert_eq!(
        report["side_effect_count"], 1,
        "RESUME-CONTINUATION intent-only replay must NOT re-fire the side effect (at-most-once)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms the resume-continuation side effect fired AT MOST once (1, never 2)"
    );

    // Key-shape proof: the fresh process specifically ran the resume-*-dispatch-* key.
    assert_eq!(
        report["dispatch_key"], RESUME_KEY,
        "the dedup key proven across the crash is the resume-continuation key shape"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **MUTATION PROOF for the continuation dispatch path:** the same resume key,
/// but on the non-idempotent path (`--mode old`, `submit_task_as`) → crash +
/// re-drive RE-RUNS THE SIDE EFFECT (double-fire, counter = 2).
///
/// This is direct proof that the test above
/// (`resume_continuation_intent_crash_is_at_most_once`) WOULD FAIL if the
/// continuation dispatch used `submit_task_as` instead of
/// `submit_task_idempotent` — i.e. that it genuinely proves the resume key's
/// idempotence rather than passing by accident. (`--mode old` bypasses the
/// outbox key entirely; `--key` is ignored on the old path, but is supplied in
/// resume form for honesty.)
#[test]
fn resume_continuation_old_path_double_fires() {
    let bin = harness_bin();
    let dir = tempdir("resume-continuation-mutation");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let (ob, ct, oc) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
    );

    // `--mode old` uses the `crash`/`resume` phases (submit_task_as, NOT the outbox).
    // The key is supplied in resume form to emphasize that the ID is the same — the
    // old path simply does NOT dedup it.
    let (ok1, _o1, e1) = run(
        &bin,
        &phase_args_keyed("old", "crash", &ob, &ct, &oc, RESUME_KEY),
    );
    assert!(!ok1, "crash phase must exit non-zero. stderr={e1}");
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect ran once before crash"
    );

    let (ok2, o2, e2) = run(
        &bin,
        &phase_args_keyed("old", "resume", &ob, &ct, &oc, RESUME_KEY),
    );
    assert!(ok2, "resume phase must succeed. stderr={e2}");
    let report = result_json(&o2);
    eprintln!("[resume-continuation mutation] {report}");

    // MUTATION PROOF: without the idempotence key, the side effect fires TWICE.
    assert_eq!(
        report["side_effect_count"], 2,
        "NON-IDEMPOTENT resume continuation double-fires across the crash (the mutation \
         the idempotent test guards against)"
    );
    assert_eq!(
        read_counter(&counter),
        2,
        "disk counter confirms the non-idempotent path double-fires"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Distinguishing claim: the old and new paths differ EXACTLY here — the old
/// path double-fires (2), the new one does not (1). This is the entire fix's
/// essence in one line.
#[test]
fn old_double_fires_but_new_does_not() {
    let bin = harness_bin();

    // OLD → 2
    let dir_old = tempdir("contrast-old");
    let ob = dir_old.join("o.jsonl").to_string_lossy().into_owned();
    let ct = dir_old.join("c.txt").to_string_lossy().into_owned();
    let oc = dir_old.join("oc.json").to_string_lossy().into_owned();
    let _ = run(&bin, &phase_args("old", "crash", &ob, &ct, &oc));
    let _ = run(&bin, &phase_args("old", "resume", &ob, &ct, &oc));
    let old_count = read_counter(Path::new(&ct));

    // NEW → 1
    let dir_new = tempdir("contrast-new");
    let ob = dir_new.join("o.jsonl").to_string_lossy().into_owned();
    let ct = dir_new.join("c.txt").to_string_lossy().into_owned();
    let oc = dir_new.join("oc.json").to_string_lossy().into_owned();
    let _ = run(&bin, &phase_args("new", "crash", &ob, &ct, &oc));
    let _ = run(&bin, &phase_args("new", "resume", &ob, &ct, &oc));
    let new_count = read_counter(Path::new(&ct));

    assert_eq!(old_count, 2, "old path double-fires");
    assert_eq!(new_count, 1, "new path fires exactly once");
    assert!(
        old_count > new_count,
        "the fix strictly reduces side-effect count under the crash window \
         (old={old_count}, new={new_count})"
    );

    let _ = std::fs::remove_dir_all(&dir_old);
    let _ = std::fs::remove_dir_all(&dir_new);
}

// ============================================================================
// APPROVAL PATH (`approval-*` keys) — at-most-once across a genuine process boundary
// ============================================================================
//
// This closes a review finding: the earlier cross-process proof covered only
// `submit_task` keys (`turn-*`), NOT approval keys (`approval-{id}`).
// `ActionRuntime::approve` is idempotent through the SAME dispatch outbox
// (key `approval-{id}`: lookup → record_intent → run_after_approval
// (side effect) → record_committed → pending.remove), and here that is proven
// across a GENUINE SIGKILL (exit 137) — durable pending (the Wire phase) lets
// a fresh process load the SAME pending approval from disk and re-approve it.

/// Environment variable that arms the committed-window crash hook in the harness.
const CRASH_AFTER_COMMITTED_ENV: &str = "FAMILYCLAW_REDTEAM_CRASH_AFTER_COMMITTED";

/// Builds the approval path's `run` phase arguments (`--pending` + `--task-queue`
/// added on top of `phase_args`, since durable pending = the Wire phase).
#[allow(clippy::too_many_arguments)]
fn approval_phase_args<'a>(
    phase: &'a str,
    outbox: &'a str,
    counter: &'a str,
    outcome: &'a str,
    pending: &'a str,
    task_queue: &'a str,
) -> Vec<&'a str> {
    vec![
        "run",
        "--mode",
        "new",
        "--phase",
        phase,
        "--outbox",
        outbox,
        "--counter",
        counter,
        "--outcome-out",
        outcome,
        "--pending",
        pending,
        "--task-queue",
        task_queue,
        "--clock",
        CLOCK,
    ]
}

/// **APPROVAL PATH, INTENT-ONLY WINDOW proves at-most-once fail-closed across a
/// genuine process boundary — with key `approval-{id}` (NOT `turn-*`).**
///
/// This is the direct closing of the review finding: the earlier cross-process
/// proof covered only `submit_task` keys. Now the same SIGKILL proof applies
/// to [`ActionRuntime::approve`]'s side-effect window.
///
/// Phase 1 (`approve_crash_intent`, hook armed): dispatch a task requiring
/// approval → approve it → `run_after_approval` runs the side effect
/// (counter = 1), `record_intent` is fsynced, the process aborts at the start
/// of `record_committed` → exits 137. On disk: intent marker PRESENT,
/// committed marker ABSENT, the pending approval still on the durable surface.
///
/// Phase 2 (`approve_resume`, fresh process): durable pending is loaded from
/// disk (Wire), the SAME `ApprovalId` is picked up and re-approved → the
/// outbox lookup sees `InProgress` → `approve` returns `PolicyDenied`
/// fail-closed. The counter STAYS at 1 (the side effect does NOT re-run) →
/// at-most-once.
///
/// The test would fail if `approve` were NOT idempotent (outbox bypassed):
/// re-approving would re-run `run_after_approval` → counter = 2. (A separate
/// mutation proof is done by removing the outbox branch from `approve`.)
#[test]
#[allow(clippy::too_many_lines)]
fn approval_path_intent_crash_is_at_most_once() {
    let bin = harness_bin();
    let dir = tempdir("approval-intent");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let pending = dir.join("pending.jsonl");
    let task_queue = dir.join("tasks.jsonl");
    let (ob, ct, oc, pd, tq) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
        pending.to_string_lossy().into_owned(),
        task_queue.to_string_lossy().into_owned(),
    );

    // Phase 1 (approve_crash_intent): armed intent hook → abort at the start of
    // record_committed after the approval.
    let (code1, ok1, _o1, e1) = run_env(
        &bin,
        &approval_phase_args("approve_crash_intent", &ob, &ct, &oc, &pd, &tq),
        &[(CRASH_AFTER_INTENT_ENV, "1")],
    );
    assert!(
        !ok1,
        "approve_crash_intent phase must NOT exit success. stderr={e1}"
    );
    assert_eq!(
        code1,
        Some(137),
        "approve_crash_intent must exit 137 (SIGKILL-style) from the crash hook. stderr={e1}"
    );

    // The side effect fired EXACTLY ONCE before the crash (approve ran
    // run_after_approval, which bumped the counter).
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect fired exactly once before the approval intent-only crash"
    );

    // On disk: intent marker PRESENT with key approval-*, committed marker ABSENT.
    let on_disk = read_outbox(&outbox);
    assert!(
        on_disk.contains("dispatch_intent"),
        "intent marker must be present on disk after approval intent-only crash. disk={on_disk:?}"
    );
    assert!(
        on_disk.contains("approval-"),
        "outbox key must be approval-* (NOT turn-*) on the approval path. disk={on_disk:?}"
    );
    assert!(
        !on_disk.contains("dispatch_committed"),
        "committed marker must be ABSENT on disk (crash hit before record_committed). \
         disk={on_disk:?}"
    );
    // The pending approval PERSISTED on the durable surface (Wire) → resume can load it.
    let pending_on_disk = std::fs::read_to_string(&pending).unwrap_or_default();
    assert!(
        pending_on_disk.contains("pending_approval_put"),
        "durable pending must still hold the approval after the intent-only crash \
         (Wire phase). pending={pending_on_disk:?}"
    );

    // Phase 2 (approve_resume): fresh process, loads the same ApprovalId from disk,
    // re-approves → InProgress → PolicyDenied fail-closed.
    let (code2, ok2, o2, e2) = run_env(
        &bin,
        &approval_phase_args("approve_resume", &ob, &ct, &oc, &pd, &tq),
        // NOT armed on resume — the hook is not used.
        &[],
    );
    assert!(
        ok2,
        "approve_resume phase must exit success (code={code2:?}). stderr={e2}"
    );
    let report = result_json(&o2);
    eprintln!("[approval-intent resume] {report}");

    // AT-MOST-ONCE PROOF 1: the re-approval is PolicyDenied fail-closed.
    assert_eq!(
        report["policy_denied"],
        serde_json::Value::Bool(true),
        "APPROVAL intent-only replay must be PolicyDenied (fail-closed), not a silent re-run"
    );

    // AT-MOST-ONCE PROOF 2: the counter STAYS at 1 — the side effect did NOT fire again.
    assert_eq!(
        report["side_effect_count"], 1,
        "APPROVAL intent-only replay must NOT re-fire the side effect (at-most-once)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms the approval side effect fired AT MOST once (1, never 2)"
    );

    // Wire-phase proof: the fresh process genuinely loaded the SAME approval from disk.
    assert!(
        report["reloaded_approval_id"].is_string(),
        "fresh process must have reloaded the durable pending approval id"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **APPROVAL PATH, COMMITTED WINDOW proves value-identical replay across a
/// genuine process boundary — with key `approval-{id}`.**
///
/// Phase 1 (`approve_crash_committed`, hook armed): approve → the side effect
/// fires (counter = 1), `record_committed` is fsynced, the process aborts ONLY
/// after the committed record but BEFORE `pending.remove` → exits 137. On
/// disk: intent + committed PRESENT.
///
/// Phase 2 (`approve_resume`): a fresh process loads the same `ApprovalId` →
/// the outbox lookup sees `Committed` → returns the value-identical
/// `SubmitOutcome` (same `task_id`, status Done) without re-running the side
/// effect. Counter = 1.
#[test]
#[allow(clippy::too_many_lines)]
fn approval_path_committed_crash_is_value_identical() {
    let bin = harness_bin();
    let dir = tempdir("approval-committed");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let pending = dir.join("pending.jsonl");
    let task_queue = dir.join("tasks.jsonl");
    let (ob, ct, oc, pd, tq) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
        pending.to_string_lossy().into_owned(),
        task_queue.to_string_lossy().into_owned(),
    );

    // Phase 1 (approve_crash_committed): the hook aborts ONLY after the committed record.
    let (code1, ok1, _o1, e1) = run_env(
        &bin,
        &approval_phase_args("approve_crash_committed", &ob, &ct, &oc, &pd, &tq),
        &[(CRASH_AFTER_COMMITTED_ENV, "1")],
    );
    assert!(
        !ok1,
        "approve_crash_committed phase must NOT exit success. stderr={e1}"
    );
    assert_eq!(
        code1,
        Some(137),
        "approve_crash_committed must exit 137 from the crash hook. stderr={e1}"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect fired exactly once before the committed-window crash"
    );

    // On disk: intent + committed PRESENT, key approval-*.
    let on_disk = read_outbox(&outbox);
    assert!(
        on_disk.contains("dispatch_intent") && on_disk.contains("dispatch_committed"),
        "both intent and committed markers must be on disk (committed window). disk={on_disk:?}"
    );
    assert!(
        on_disk.contains("approval-"),
        "outbox key must be approval-* on the approval path. disk={on_disk:?}"
    );

    // Phase 2 (approve_resume): same ApprovalId → Committed → value-identical.
    let (code2, ok2, o2, e2) = run_env(
        &bin,
        &approval_phase_args("approve_resume", &ob, &ct, &oc, &pd, &tq),
        &[],
    );
    assert!(
        ok2,
        "approve_resume phase must exit success (code={code2:?}). stderr={e2}"
    );
    let report = result_json(&o2);
    eprintln!("[approval-committed resume] {report}");

    // PROOF OF THE FIX 1 (value identity): re-approve returns the same task_id.
    assert_eq!(
        report["value_identical"],
        serde_json::Value::Bool(true),
        "APPROVAL committed replay must return the value-identical SubmitOutcome (same task_id)"
    );
    assert_eq!(
        report["policy_denied"],
        serde_json::Value::Bool(false),
        "APPROVAL committed replay must NOT be denied (it returns the committed outcome)"
    );

    // PROOF OF THE FIX 2 (at-most-once): the counter STAYS at 1.
    assert_eq!(
        report["side_effect_count"], 1,
        "APPROVAL committed replay must NOT re-fire the side effect (exactly-once dispatch)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms the approval side effect fired exactly once"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
