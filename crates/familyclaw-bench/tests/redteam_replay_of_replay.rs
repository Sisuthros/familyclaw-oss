//! RED-TEAM: crashing MID-replay — repeatedly (resume the resume).
//!
//! Attack (design §5, the first bullet): *"crash during replay-of-replay
//! (resume the resume)"*. Start a task, crash, restart into replay mode,
//! **crash again mid-replay**, restart a third time — and continuity must
//! still hold: the end state matches a crash-free run and side effects
//! (memory records) happen **exactly once**.
//!
//! This is run **across a real process boundary**: each "crash" is a separate
//! `continuity_daemon` process that exits with code 137 (SIGKILL-style). The
//! clock is injected on every call → deterministic.
//!
//! Core question: can a crash that INTERRUPTS replay — repeated — corrupt the
//! journal or cause a duplicate record / a lost step?

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locates the `continuity_daemon` binary in the same profile directory.
fn daemon_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let deps = exe.parent().expect("deps dir");
    let profile_dir = deps.parent().expect("profile dir");
    let mut bin = profile_dir.join("continuity_daemon");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    assert!(
        bin.exists(),
        "continuity_daemon binary not found at {} — build it first",
        bin.display()
    );
    bin
}

/// Fixed injected clock (RFC 3339) — reproducibility.
const CLOCK: &str = "2024-05-29T18:13:20+00:00"; // = unix 1_717_000_000

/// A unique temp directory for this attack run.
fn tempdir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "familyclaw-redteam-replay2-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Runs a daemon subcommand and returns (`exit_ok`, stdout, stderr).
fn run(bin: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin).args(args).output().expect("spawn daemon");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Extracts stdout's `RESULT <json>` line as a parsed Value.
fn result_json(stdout: &str) -> serde_json::Value {
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("RESULT "))
        .unwrap_or_else(|| panic!("no RESULT line in stdout: {stdout:?}"));
    serde_json::from_str(line).expect("parse RESULT json")
}

/// Counts the journal's `step_completed` lines (torn incomplete lines don't parse).
fn count_completed_lines(journal: &Path) -> usize {
    let Ok(contents) = std::fs::read_to_string(journal) else {
        return 0;
    };
    contents
        .lines()
        .filter(|l| {
            // Journal line: {"step_id":N,"timestamp":..,"kind":{"kind":"step_completed",..}}
            // — `kind` is a NESTED object, so we read `kind.kind`.
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| {
                    v.get("kind")
                        .and_then(|k| k.get("kind"))
                        .and_then(|k| k.as_str().map(str::to_owned))
                })
                .as_deref()
                == Some("step_completed")
        })
        .count()
}

/// Counts memories belonging to `task` in the store JSON (tag `task:<id>`).
///
/// Reads the store as raw JSON — does not trust the daemon's own counter, so
/// a duplicate record shows up even if the daemon reports "clean".
fn count_task_memories(store: &Path, task: &str) -> usize {
    let Ok(contents) = std::fs::read_to_string(store) else {
        return 0;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return 0;
    };
    let want = format!("task:{task}");
    // The store could be either { "<id>": <memory> } or { "memories": {...} }
    // etc. Recursively search for all objects that have the right tag.
    let mut count = 0usize;
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(map) => {
                let has_tag = map
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .is_some_and(|arr| arr.iter().any(|t| t.as_str() == Some(&want)));
                if has_tag {
                    count += 1;
                }
                for (_k, v) in map {
                    stack.push(v);
                }
            }
            serde_json::Value::Array(arr) => stack.extend(arr),
            _ => {}
        }
    }
    count
}

/// Args helper: common `start` scaffolding.
fn start_args<'a>(
    journal: &'a str,
    store: &'a str,
    task: &'a str,
    steps: &'a str,
    crash_at: Option<&'a str>,
) -> Vec<&'a str> {
    let mut v = vec![
        "start",
        "--journal",
        journal,
        "--store",
        store,
        "--task",
        task,
        "--steps",
        steps,
        "--clock",
        CLOCK,
    ];
    if let Some(point) = crash_at {
        v.push("--crash-at");
        v.push(point);
    }
    v
}

/// ATTACK: crash MID-replay twice in a row, then resume.
///
/// Sequence (each line = a separate process across a real boundary):
/// 1. `start --crash-at mid_write`  → a partial journal (2 intact + torn).
/// 2. `start --crash-at mid_replay` → re-enter replay, crash mid-way (#2 crash).
/// 3. `start --crash-at mid_replay` → re-enter replay AGAIN, crash (#3 crash).
/// 4. `resume`                       → must resume cleanly, side effects 1x.
#[test]
fn replay_of_replay_thrice_resumes_clean_with_side_effects_once() {
    let bin = daemon_bin();
    let dir = tempdir("thrice");
    let journal = dir.join("j.jsonl");
    let store = dir.join("s.json");
    let jp = journal.to_string_lossy().into_owned();
    let sp = store.to_string_lossy().into_owned();
    let task = "replay2-attack";
    let steps = "3";

    // ── Crash #1: mid_write → torn last line; steps 0,1 committed. ──
    let (ok1, _o1, e1) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_write")));
    assert!(
        !ok1,
        "mid_write must exit non-zero (injected crash). stderr={e1}"
    );
    let committed_after_c1 = count_completed_lines(&journal);
    let mem_after_c1 = count_task_memories(&store, task);
    eprintln!("[c1 mid_write] committed_lines={committed_after_c1} memories={mem_after_c1}");
    assert_eq!(
        committed_after_c1, 2,
        "mid_write should leave exactly 2 committed steps (0,1) + a torn line"
    );

    // ── Crash #2: mid_replay → re-enter replay, exit mid-way. ──
    // This is the FIRST crash that interrupts replay.
    let (ok2, _o2, e2) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_replay")));
    assert!(!ok2, "mid_replay must exit non-zero. stderr={e2}");
    let committed_after_c2 = count_completed_lines(&journal);
    let mem_after_c2 = count_task_memories(&store, task);
    eprintln!(
        "[c2 mid_replay] committed_lines={committed_after_c2} memories={mem_after_c2} stderr={}",
        e2.trim()
    );

    // ── Crash #3: mid_replay AGAIN → replay-of-replay. ──
    // A second crash that interrupts replay, IN A ROW — "resume the resume".
    let (ok3, _o3, e3) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_replay")));
    assert!(!ok3, "second mid_replay must exit non-zero. stderr={e3}");
    let committed_after_c3 = count_completed_lines(&journal);
    let mem_after_c3 = count_task_memories(&store, task);
    eprintln!(
        "[c3 mid_replay#2] committed_lines={committed_after_c3} memories={mem_after_c3} stderr={}",
        e3.trim()
    );

    // INVARIANT 1: repeated replay crashes must NOT ADD lines to the journal
    // (replay only replays, never writes) — and must not corrupt intact ones.
    assert_eq!(
        committed_after_c3, committed_after_c1,
        "replay-of-replay crashes must NOT append/lose committed steps \
         (c1={committed_after_c1} c3={committed_after_c3})"
    );

    // INVARIANT 2: repeated replay crashes must not write memories (the
    // mid_replay path does not persist) — no duplicate records.
    assert_eq!(
        mem_after_c3, mem_after_c1,
        "replay-of-replay must not write memories (c1={mem_after_c1} c3={mem_after_c3})"
    );

    // ── Final resume: must resume cleanly. ──
    let resume_args = vec![
        "resume",
        "--journal",
        &jp,
        "--store",
        &sp,
        "--task",
        task,
        "--steps",
        steps,
        "--clock",
        CLOCK,
    ];
    let (okr, or, er) = run(&bin, &resume_args);
    assert!(okr, "final resume must succeed. stderr={er}");
    let report = result_json(&or);
    eprintln!("[resume] {report}");

    assert_eq!(
        report["resumed_clean"],
        serde_json::Value::Bool(true),
        "after replay-of-replay, final resume must reach the clean end state"
    );
    assert_eq!(
        report["was_replaying"],
        serde_json::Value::Bool(true),
        "journal had committed steps → resume must enter replay mode"
    );

    // INVARIANT 3 (side-effects exactly once): EXACTLY `steps` memories in the
    // store, no more (duplicate record) and no fewer (lost step). Read the
    // store raw — don't trust the daemon's counter.
    let final_mem = count_task_memories(&store, task);
    eprintln!("[final] memories={final_mem} expected={steps}");
    assert_eq!(
        final_mem,
        steps.parse::<usize>().unwrap(),
        "side-effects exactly once: store must hold exactly {steps} task memories \
         after replay-of-replay (got {final_mem})"
    );

    // ── The claim HELD: replay-of-replay + the first resume produced a clean
    //    end state, side effects exactly once. Mechanism: mid_replay only
    //    replays (writes nothing to the journal or the store), and the torn
    //    line is filtered out of the replay vector (`is_step` filter +
    //    tolerant last-line parser).
    //
    // A seam was previously found underneath this (the torn line was not
    // removed → a fresh append merged onto the same line → permanent
    // corruption). This is now FIXED at the root cause (`FileJournal::open`
    // heals on open by truncating the newline-less stub) and proven closed in
    // the test `torn_write_then_resume_keeps_journal_readable_seam_closed`.
    let _ = std::fs::remove_dir_all(&dir);
}

/// REGRESSION (seam closed): torn-write → resume no longer corrupts the journal.
///
/// **Previous seam (now fixed, `familyclaw-durable/src/file.rs`):** `FileJournal`
/// tolerated a torn last line on read but left it on disk. Resume would
/// `append` a fresh step line onto the SAME physical line (the stub was
/// missing its `\n`) → causing interior corruption that killed every
/// subsequent reopen/replay.
///
/// **Fix (root cause):** `FileJournal::open` heals the file on open: a
/// newline-less, unparsable stub is truncated away BEFORE the write handle is
/// opened, so every append starts from a clean line boundary. The stub is
/// always an un-fsynced, uncommitted write → discarding it is safe.
///
/// This test proves the attack NO LONGER breaks the claim:
/// 1. `mid_write` → torn line.
/// 2. 1st resume → clean (as before).
/// 3. the journal has NO fused line (no two `step_id`s on one line).
/// 4. 2nd resume → STILL SUCCEEDS (before: it died with `CorruptEntry`).
/// 5. resume idempotence: side effects exactly once (3 memories, no more).
#[test]
fn torn_write_then_resume_keeps_journal_readable_seam_closed() {
    let bin = daemon_bin();
    let dir = tempdir("seam-closed");
    let journal = dir.join("j.jsonl");
    let store = dir.join("s.json");
    let jp = journal.to_string_lossy().into_owned();
    let sp = store.to_string_lossy().into_owned();
    let task = "torn-seam";
    let steps = "3";

    // mid_write → torn last line.
    let (ok1, _o1, _e1) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_write")));
    assert!(!ok1, "mid_write must crash");

    let resume_args = vec![
        "resume",
        "--journal",
        &jp,
        "--store",
        &sp,
        "--task",
        task,
        "--steps",
        steps,
        "--clock",
        CLOCK,
    ];

    // First resume: SUCCEEDS and reaches a clean end state.
    let (okr, or, er) = run(&bin, &resume_args);
    assert!(okr, "first resume succeeds. stderr={er}");
    assert_eq!(
        result_json(&or)["resumed_clean"],
        serde_json::Value::Bool(true)
    );

    // FIX INVARIANT: no physical line may contain two step_ids — i.e. the
    // torn stub + the fresh line did NOT fuse (heal-on-open truncated the stub).
    let contents = std::fs::read_to_string(&journal).expect("read journal");
    let merged_garbage = contents
        .lines()
        .any(|l| l.matches("\"step_id\"").count() >= 2);
    assert!(
        !merged_garbage,
        "SEAM MUST BE CLOSED: no physical line may fuse two entries. journal=\n{contents}"
    );

    // PROOF: the second resume STILL SUCCEEDS — the journal remains readable.
    let (okr2, stdout_r2, er2) = run(&bin, &resume_args);
    assert!(
        okr2,
        "FIXED: second resume must succeed because the journal is no longer corrupt. stderr={er2}"
    );
    assert!(
        !er2.contains("corrupt journal entry"),
        "second resume must NOT hit CorruptEntry, got stderr: {er2}"
    );
    assert_eq!(
        result_json(&stdout_r2)["resumed_clean"],
        serde_json::Value::Bool(true),
        "second resume must also reach the clean end state"
    );

    // IDEMPOTENCE: after two resumes, EXACTLY `steps` memories — no duplicate.
    let final_mem = count_task_memories(&store, task);
    assert_eq!(
        final_mem,
        steps.parse::<usize>().unwrap(),
        "side-effects exactly once across two resumes (got {final_mem})"
    );
    eprintln!("[SEAM CLOSED] both resumes clean, journal readable, {final_mem} memories");

    let _ = std::fs::remove_dir_all(&dir);
}

/// VARIANT: start with a CLEAN, complete journal, then crash mid-replay
/// three times in a row, then resume.
///
/// This isolates the "replay-of-replay" attack from the `mid_write` remnant:
/// here the journal is COMPLETE (3 steps), and each `mid_replay` re-enters a
/// full replay and crashes mid-way. Resume must not lose anything or produce
/// new memories (all 3 are already in the store).
#[test]
fn full_journal_replay_crashes_thrice_then_resume_is_noop_clean() {
    let bin = daemon_bin();
    let dir = tempdir("full");
    let journal = dir.join("j.jsonl");
    let store = dir.join("s.json");
    let jp = journal.to_string_lossy().into_owned();
    let sp = store.to_string_lossy().into_owned();
    let task = "replay2-full";
    let steps = "4";

    // A clean full run → 4 steps + 4 memories.
    let (ok0, _o0, e0) = run(&bin, &start_args(&jp, &sp, task, steps, None));
    assert!(ok0, "clean start must succeed. stderr={e0}");
    let committed0 = count_completed_lines(&journal);
    let mem0 = count_task_memories(&store, task);
    assert_eq!(committed0, 4, "clean start commits all 4 steps");
    assert_eq!(mem0, 4, "clean start persists 4 memories");

    // Crash mid-replay THREE times in a row.
    for round in 1..=3 {
        let (ok, _o, e) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_replay")));
        assert!(
            !ok,
            "mid_replay round {round} must exit non-zero. stderr={e}"
        );
        let committed = count_completed_lines(&journal);
        let mem = count_task_memories(&store, task);
        eprintln!(
            "[full mid_replay round {round}] committed={committed} mem={mem} stderr={}",
            e.trim()
        );
        assert_eq!(
            committed, committed0,
            "round {round}: replay crash must not change committed step count"
        );
        assert_eq!(
            mem, mem0,
            "round {round}: replay crash must not write/duplicate memories"
        );
    }

    // Resume: everything already in the log → clean, no new memories.
    let resume_args = vec![
        "resume",
        "--journal",
        &jp,
        "--store",
        &sp,
        "--task",
        task,
        "--steps",
        steps,
        "--clock",
        CLOCK,
    ];
    let (okr, or, er) = run(&bin, &resume_args);
    assert!(okr, "resume must succeed. stderr={er}");
    let report = result_json(&or);
    eprintln!("[full resume] {report}");
    assert_eq!(report["resumed_clean"], serde_json::Value::Bool(true));
    let final_mem = count_task_memories(&store, task);
    assert_eq!(
        final_mem, 4,
        "side-effects exactly once after triple replay-crash (got {final_mem})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
