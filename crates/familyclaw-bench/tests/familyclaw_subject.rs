//! Integration tests for [`FamilyClawSubject`] across a real `continuity_daemon`
//! child process.
//!
//! These tests prove continuity **across a real process boundary** (design
//! §4): the daemon runs as a separate process
//! (`CARGO_BIN_EXE_continuity_daemon`), is killed at a crash point, and
//! restarted. The clock is injected, so results are deterministic.

use familyclaw_bench::{CrashPoint, FamilyClawSubject, Subject, Task};
use familyclaw_core::time;

/// Locates the `continuity_daemon` binary and sets it in the environment.
///
/// `CARGO_BIN_EXE_continuity_daemon` is only available to tests in the SAME
/// package; the daemon lives in the `familyclaw-agent` package, so it's
/// located from the test binary's directory (`target/<profile>/deps`) parent
/// (`target/<profile>`), where Cargo places workspace binaries.
fn set_daemon_env() {
    let exe = std::env::current_exe().expect("current_exe");
    // exe = target/<profile>/deps/familyclaw_subject-<hash>(.exe)
    let deps = exe.parent().expect("deps dir");
    let profile_dir = deps.parent().expect("profile dir");
    let mut bin = profile_dir.join("continuity_daemon");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    assert!(
        bin.exists(),
        "continuity_daemon binary not found at {} — run `cargo build -p familyclaw-agent --bin continuity_daemon` first",
        bin.display()
    );
    std::env::set_var("CONTINUITY_DAEMON_BIN", &bin);
}

/// Fixed injected clock (reproducibility).
fn clock() -> familyclaw_core::Timestamp {
    time::from_unix_secs(1_717_000_000).expect("valid clock")
}

fn three_step_task() -> Task {
    Task::new(
        "continuity-demo",
        "three durable steps",
        vec!["a".into(), "b".into(), "c".into()],
    )
}

#[tokio::test]
async fn clean_run_resumes_without_reexecuting_side_effects() {
    set_daemon_env();
    let mut subject = FamilyClawSubject::from_env().expect("subject");
    let task = three_step_task();

    let handle = subject.start_task(&task, clock()).await.expect("start");
    subject
        .kill(&handle, CrashPoint::Clean)
        .await
        .expect("clean run");

    let report = subject.restart(clock()).await.expect("restart");
    assert!(report.resumed_clean, "clean run should resume cleanly");
    assert_eq!(
        report.side_effects_reexecuted, 0,
        "no side effects re-executed on a clean resume"
    );
}

#[tokio::test]
async fn mid_write_torn_line_resumes_clean() {
    set_daemon_env();
    let mut subject = FamilyClawSubject::from_env().expect("subject");
    let task = three_step_task();

    let handle = subject.start_task(&task, clock()).await.expect("start");
    // Crash mid-write of the last line (torn last line).
    subject
        .kill(&handle, CrashPoint::MidWrite)
        .await
        .expect("mid_write crash");

    let report = subject.restart(clock()).await.expect("restart");
    // Resume completes the remaining steps and reaches a clean end state.
    assert!(report.resumed_clean, "torn last line must resume clean");
    assert_eq!(report.side_effects_reexecuted, 0);
}

#[tokio::test]
async fn before_write_crash_loses_no_committed_work_on_resume() {
    set_daemon_env();
    let mut subject = FamilyClawSubject::from_env().expect("subject");
    let task = three_step_task();

    let handle = subject.start_task(&task, clock()).await.expect("start");
    subject
        .kill(&handle, CrashPoint::BeforeWrite)
        .await
        .expect("before_write crash");

    // Nothing reached disk → resume runs all steps fresh.
    let report = subject.restart(clock()).await.expect("restart");
    assert!(report.resumed_clean);
    assert!(!report.was_replaying, "empty journal → not replaying");
}

#[tokio::test]
async fn recall_finds_persisted_memory_across_processes() {
    set_daemon_env();
    let mut subject = FamilyClawSubject::from_env().expect("subject");
    let task = three_step_task();

    let handle = subject.start_task(&task, clock()).await.expect("start");
    subject.kill(&handle, CrashPoint::Clean).await.expect("run");

    let hits = subject
        .recall("completed step", clock())
        .await
        .expect("recall");
    assert!(!hits.is_empty(), "memory must survive the process boundary");
}

#[tokio::test]
async fn sleep_cycle_keeps_protected_core_intact() {
    set_daemon_env();
    let mut subject = FamilyClawSubject::from_env().expect("subject");
    let task = three_step_task();

    let handle = subject.start_task(&task, clock()).await.expect("start");
    subject.kill(&handle, CrashPoint::Clean).await.expect("run");

    let summary = subject.sleep_cycle(clock()).await.expect("sleep");
    assert!(
        summary.protected_core_intact,
        "dreaming must never lose identity anchors"
    );
}

/// ADR §4: Dual-write fix — crash after journal write but before memory write.
/// Verifies that resume correctly persists the memory for the step that was
/// recorded in the journal but whose memory write was interrupted.
#[tokio::test]
async fn crash_after_journal_before_memory_resumes_with_memory() {
    set_daemon_env();
    let mut subject = FamilyClawSubject::from_env().expect("subject");
    let task = three_step_task();

    let handle = subject.start_task(&task, clock()).await.expect("start");
    // Crash at MidWrite (after journal, before memory for step 2).
    subject
        .kill(&handle, CrashPoint::MidWrite)
        .await
        .expect("mid_write crash");

    // Resume should replay and persist memory for all recorded steps.
    let report = subject.restart(clock()).await.expect("restart");
    assert!(
        report.resumed_clean,
        "resume must be clean after mid_write crash"
    );

    // Verify memory exists for all 3 steps (turn_key makes it idempotent).
    let hits = subject
        .recall("completed step", clock())
        .await
        .expect("recall");
    assert_eq!(
        hits.len(),
        3,
        "all 3 steps must have memory persisted after resume"
    );
}
