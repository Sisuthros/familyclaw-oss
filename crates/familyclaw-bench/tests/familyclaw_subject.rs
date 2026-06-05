//! Integraatiotestit [`FamilyClawSubject`]:lle aidon `continuity_daemon`
//! -lapsiprosessin yli.
//!
//! Nämä testit todistavat jatkuvuuden **aidon prosessirajan yli** (design §4):
//! daemon ajetaan erillisenä prosessina (`CARGO_BIN_EXE_continuity_daemon`),
//! tapetaan kaatumispisteessä ja käynnistetään uudelleen. Kello injektoidaan,
//! joten tulokset ovat deterministisiä.

use familyclaw_bench::{CrashPoint, FamilyClawSubject, Subject, Task};
use familyclaw_core::time;

/// Paikantaa `continuity_daemon`-binäärin ja asettaa sen ympäristöön.
///
/// `CARGO_BIN_EXE_continuity_daemon` on saatavilla vain SAMAN paketin testeille;
/// daemon on `familyclaw-agent`-paketissa, joten paikannetaan se testibinäärin
/// hakemiston (`target/<profile>/deps`) yläkansiosta (`target/<profile>`), johon
/// Cargo asettaa workspace-binäärit.
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

/// Kiinteä injektoitu kello (reprodusoitavuus).
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
    // Kaada kesken viimeisen rivin kirjoituksen (torn last line).
    subject
        .kill(&handle, CrashPoint::MidWrite)
        .await
        .expect("mid_write crash");

    let report = subject.restart(clock()).await.expect("restart");
    // Resume täyttää loput askeleet ja saavuttaa puhtaan lopputilan.
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

    // Mikään ei ehtinyt levylle → resume ajaa kaikki askeleet tuoreena.
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
