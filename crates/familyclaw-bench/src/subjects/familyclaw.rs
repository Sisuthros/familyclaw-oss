//! [`FamilyClawSubject`] — runs the `continuity_daemon` binary as a black box.
//!
//! This is the first [`Subject`] implementation (design §2.1). It does NOT
//! call FamilyClaw crates directly but runs the `continuity_daemon` binary
//! as separate child processes — proving continuity **across a real process
//! boundary** (the same pattern as `familyclaw-agent/src/bin/crash_replay.rs`).
//! This way the benchmark measures what a skeptic can run themselves, not an
//! in-process library call.
//!
//! ## Lifecycle
//! 1. [`start_task`](FamilyClawSubject::start_task) — reserves a temporary
//!    journal + store path and stores the task (does not yet run the daemon).
//! 2. [`kill`](FamilyClawSubject::kill) — runs `continuity_daemon start
//!    --crash-at <point>`, which writes state and exits at the crash point
//!    (`Clean` runs to completion).
//! 3. [`restart`](FamilyClawSubject::restart) — runs `resume`, which rebuilds
//!    context from the journal, replays completed steps, and finalizes.
//! 4. [`recall`](FamilyClawSubject::recall) — runs `recall` against the
//!    persisted store.
//! 5. [`sleep_cycle`](FamilyClawSubject::sleep_cycle) — runs `sleep` (one
//!    [`DreamCycle`](familyclaw_dream::DreamCycle)).
//!
//! ## Reproducibility
//! The clock is injected into every daemon call as a `--clock <rfc3339>`
//! argument ([`Timestamp`]) — the daemon never reads the system clock. Same
//! input → same result.
//!
//! ## Binary resolution
//! Tests use the `CARGO_BIN_EXE_continuity_daemon` environment variable
//! (set by Cargo). Otherwise the path can be given explicitly, or via the
//! `CONTINUITY_DAEMON_BIN` environment variable; as a last-resort fallback,
//! `continuity_daemon` is assumed to be on `PATH`.

use std::path::PathBuf;
use std::process::Output;

use async_trait::async_trait;
use familyclaw_core::{time, Timestamp};
use serde::Deserialize;

use crate::error::{BenchError, Result};
use crate::subject::{
    CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Subject, Task,
};

/// Environment variable used to override the daemon binary's path.
const DAEMON_BIN_ENV: &str = "CONTINUITY_DAEMON_BIN";

/// Environment variable Cargo sets for the binary's path during tests.
const CARGO_BIN_ENV: &str = "CARGO_BIN_EXE_continuity_daemon";

/// The FamilyClaw subject, which runs the `continuity_daemon` binary as a
/// child process.
///
/// Holds the temporary journal + store paths and the task state between two
/// daemon calls. Paths live under
/// [`tempdir`](FamilyClawSubject::tempdir) and are cleaned up when the
/// subject is dropped.
#[derive(Debug)]
pub struct FamilyClawSubject {
    /// The daemon binary's path.
    daemon: PathBuf,
    /// The temporary directory the journal + store are written to.
    tempdir: PathBuf,
    /// The journal file's path.
    journal: PathBuf,
    /// The memory store's path.
    store: PathBuf,
    /// The active task (set in [`start_task`](FamilyClawSubject::start_task)).
    task: Option<Task>,
    /// The subject's stable name for the scorecard.
    name: String,
}

/// The daemon's `resume` output (parsed from the stdout RESULT line).
#[derive(Debug, Deserialize)]
struct ResumeOutput {
    steps_replayed: usize,
    was_replaying: bool,
    fresh_steps: usize,
    resumed_clean: bool,
}

/// A single recall hit from the daemon.
#[derive(Debug, Deserialize)]
struct RecallHitOutput {
    content: String,
    relevance: f32,
}

/// The daemon's `recall` output.
#[derive(Debug, Deserialize)]
struct RecallOutput {
    hits: Vec<RecallHitOutput>,
}

/// The daemon's `sleep` output.
#[derive(Debug, Deserialize)]
struct SleepOutput {
    scanned: usize,
    merged: usize,
    dropped: usize,
    dates_absolutized: usize,
    strengthened: usize,
    archived: usize,
    protected_core_intact: bool,
}

impl FamilyClawSubject {
    /// Builds a subject with the given daemon binary path and temporary
    /// directory.
    ///
    /// In most cases prefer [`from_env`](FamilyClawSubject::from_env), which
    /// locates the binary from the environment automatically.
    #[must_use]
    pub fn new(daemon: impl Into<PathBuf>, tempdir: impl Into<PathBuf>) -> Self {
        let tempdir = tempdir.into();
        let journal = tempdir.join("continuity.journal.jsonl");
        let store = tempdir.join("continuity.store.json");
        Self {
            daemon: daemon.into(),
            tempdir,
            journal,
            store,
            task: None,
            name: "familyclaw".to_string(),
        }
    }

    /// Builds a subject by locating the daemon binary from the environment
    /// and creating a unique temporary directory.
    ///
    /// Binary resolution order:
    /// 1. `CONTINUITY_DAEMON_BIN` (explicit override),
    /// 2. `CARGO_BIN_EXE_continuity_daemon` (Cargo tests),
    /// 3. `continuity_daemon` (`PATH` fallback).
    ///
    /// # Errors
    /// [`BenchError::Io`] if the temporary directory cannot be created.
    pub fn from_env() -> Result<Self> {
        let daemon = resolve_daemon_bin();
        let tempdir = make_tempdir()?;
        Ok(Self::new(daemon, tempdir))
    }

    /// Returns the temporary directory in use.
    #[must_use]
    pub fn tempdir(&self) -> &std::path::Path {
        &self.tempdir
    }

    /// Removes any prior journal + store files (a fresh starting state).
    ///
    /// Uses [`remove_file_retry`] rather than a bare `remove_file`: on Windows
    /// a just-reaped `continuity_daemon` child can leave the OS holding its
    /// file handle for a few milliseconds after exit, so a plain removal
    /// intermittently fails with `ERROR_ACCESS_DENIED` (os error 5). The
    /// bounded backoff clears that transient lock deterministically; on other
    /// platforms the first attempt always succeeds.
    fn reset_state(&self) -> Result<()> {
        for p in [&self.journal, &self.store] {
            if p.exists() {
                remove_file_retry(p)?;
            }
        }
        Ok(())
    }

    /// Runs a daemon subcommand and returns its [`Output`].
    async fn run_daemon(&self, args: &[String]) -> Result<Output> {
        let daemon = self.daemon.clone();
        let owned: Vec<String> = args.to_vec();
        // A synchronous `std::process::Command` on a blocking thread, so the
        // async context doesn't stall (same pattern as the crash_replay
        // full run).
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&daemon).args(&owned).output()
        })
        .await
        .map_err(|e| BenchError::subject(format!("daemon join failed: {e}")))??;
        Ok(output)
    }

    /// Parses the `RESULT <json>` line from the daemon's stdout into the
    /// given type.
    fn parse_result<T: for<'de> Deserialize<'de>>(output: &Output) -> Result<T> {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .find_map(|l| l.strip_prefix("RESULT "))
            .ok_or_else(|| {
                let stderr = String::from_utf8_lossy(&output.stderr);
                BenchError::subject(format!(
                    "daemon produced no RESULT line (stderr: {})",
                    stderr.trim()
                ))
            })?;
        let parsed: T = serde_json::from_str(line)?;
        Ok(parsed)
    }

    /// The active task, or an error if
    /// [`start_task`](FamilyClawSubject::start_task) hasn't been called.
    fn require_task(&self) -> Result<&Task> {
        self.task
            .as_ref()
            .ok_or_else(|| BenchError::subject("no active task — call start_task first"))
    }

    /// Runs the `start` command with the given crash point (`None` = clean).
    async fn spawn_start(&self, point: Option<CrashPoint>, clock: Timestamp) -> Result<Output> {
        let task = self.require_task()?;
        let steps = task.steps.len().max(1);
        let mut args = vec![
            "start".to_string(),
            "--journal".to_string(),
            path_arg(&self.journal),
            "--store".to_string(),
            path_arg(&self.store),
            "--task".to_string(),
            task.id.clone(),
            "--steps".to_string(),
            steps.to_string(),
            "--clock".to_string(),
            time::to_rfc3339(clock),
        ];
        if let Some(point) = point {
            args.push("--crash-at".to_string());
            args.push(crash_point_arg(point).to_string());
        }
        self.run_daemon(&args).await
    }
}

#[async_trait]
impl Subject for FamilyClawSubject {
    async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
        // A fresh starting state for every task (determinism).
        self.reset_state()?;
        self.task = Some(task.clone());
        // Token = journal path (a subject-specific opaque reference).
        Ok(RunHandle::new(task.id.clone(), path_arg(&self.journal)))
    }

    async fn kill(&mut self, _handle: &RunHandle, point: CrashPoint) -> Result<()> {
        let clock = time::now();
        match point {
            CrashPoint::Clean => {
                // No crash: run the task to completion cleanly.
                let out = self.spawn_start(None, clock).await?;
                if !out.status.success() {
                    return Err(BenchError::subject(format!(
                        "clean start failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    )));
                }
            }
            CrashPoint::MidReplay => {
                // MidReplay requires a completed journal: first run a clean
                // start, then crash mid-replay by re-entering it.
                let clean = self.spawn_start(None, clock).await?;
                if !clean.status.success() {
                    return Err(BenchError::subject(
                        "mid_replay setup (clean start) failed".to_string(),
                    ));
                }
                // This exits with a non-zero code — expected.
                let _ = self.spawn_start(Some(CrashPoint::MidReplay), clock).await?;
            }
            // BeforeWrite / MidWrite / CorruptedJournal: the daemon exits at the point.
            other => {
                let _ = self.spawn_start(Some(other), clock).await?;
                if other == CrashPoint::CorruptedJournal {
                    // CorruptedJournal: the daemon has no dedicated support —
                    // simulate it by corrupting a non-final line in the
                    // journal, if there are enough lines.
                    corrupt_middle_line(&self.journal)?;
                }
            }
        }
        Ok(())
    }

    async fn restart(&mut self, clock: Timestamp) -> Result<RestartReport> {
        let task = self.require_task()?;
        let steps = task.steps.len().max(1);
        let args = vec![
            "resume".to_string(),
            "--journal".to_string(),
            path_arg(&self.journal),
            "--store".to_string(),
            path_arg(&self.store),
            "--task".to_string(),
            task.id.clone(),
            "--steps".to_string(),
            steps.to_string(),
            "--clock".to_string(),
            time::to_rfc3339(clock),
        ];
        let output = self.run_daemon(&args).await?;
        if !output.status.success() {
            return Err(BenchError::subject(format!(
                "resume failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let parsed: ResumeOutput = Self::parse_result(&output)?;
        Ok(RestartReport {
            steps_replayed: parsed.steps_replayed,
            was_replaying: parsed.was_replaying,
            // Fresh steps are a normal part of resume — NOT repeated side
            // effects. side_effects_reexecuted is always 0 as long as
            // resume is clean; an unclean resume raises it.
            side_effects_reexecuted: if parsed.resumed_clean {
                0
            } else {
                parsed.fresh_steps
            },
            resumed_clean: parsed.resumed_clean,
        })
    }

    async fn recall(&mut self, query: &str, clock: Timestamp) -> Result<Vec<RecallHit>> {
        let args = vec![
            "recall".to_string(),
            "--store".to_string(),
            path_arg(&self.store),
            "--query".to_string(),
            query.to_string(),
            "--clock".to_string(),
            time::to_rfc3339(clock),
        ];
        let output = self.run_daemon(&args).await?;
        if !output.status.success() {
            return Err(BenchError::subject(format!(
                "recall failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let parsed: RecallOutput = Self::parse_result(&output)?;
        Ok(parsed
            .hits
            .into_iter()
            .map(|h| RecallHit::new(h.content, h.relevance))
            .collect())
    }

    async fn sleep_cycle(&mut self, clock: Timestamp) -> Result<DreamSummary> {
        // Liveness check (design §3 S3): run the sleep cycle over a FRESH,
        // intact state. The harness runs scenarios sequentially against the
        // same subject, so a prior scenario (e.g. S1 CorruptedJournal) may
        // have left a corrupted journal. Reset and seed a clean completed
        // run, so `sleep` always reads a valid journal + store — not a
        // leftover corruption from a prior scenario.
        if let Some(task) = self.task.clone() {
            self.reset_state()?;
            let clean = self.spawn_start(None, clock).await?;
            if !clean.status.success() {
                return Err(BenchError::subject(format!(
                    "sleep_cycle setup (clean start) failed: {}",
                    String::from_utf8_lossy(&clean.stderr).trim()
                )));
            }
            // Keep `task` active (reset doesn't clear it, but make sure).
            self.task = Some(task);
        }
        // Make sure the journal exists (sleep reads conflicts from it).
        if !self.journal.exists() {
            std::fs::write(&self.journal, b"")?;
        }
        let args = vec![
            "sleep".to_string(),
            "--journal".to_string(),
            path_arg(&self.journal),
            "--store".to_string(),
            path_arg(&self.store),
            "--clock".to_string(),
            time::to_rfc3339(clock),
        ];
        let output = self.run_daemon(&args).await?;
        if !output.status.success() {
            return Err(BenchError::subject(format!(
                "sleep failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let parsed: SleepOutput = Self::parse_result(&output)?;
        Ok(DreamSummary {
            scanned: parsed.scanned,
            merged: parsed.merged,
            dropped: parsed.dropped,
            dates_absolutized: parsed.dates_absolutized,
            strengthened: parsed.strengthened,
            archived: parsed.archived,
            protected_core_intact: parsed.protected_core_intact,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for FamilyClawSubject {
    fn drop(&mut self) {
        // Clean up the temporary directory. Errors are ignored (best-effort),
        // but the retry wrapper first rides out any transient Windows file
        // lock the just-exited daemon child may still hold (ERROR_ACCESS_DENIED
        // / ERROR_SHARING_VIOLATION) so the tempdir is actually removed instead
        // of being leaked on the first ACCESS_DENIED.
        let _ = remove_dir_all_retry(&self.tempdir);
    }
}

/// Maximum number of removal retries when Windows briefly keeps a just-exited
/// child's file handle open. On non-Windows platforms the transient error
/// codes never occur, so the first attempt succeeds and this loop is a no-op.
const REMOVE_RETRIES: u32 = 10;

/// `true` when an OS error is a **transient** Windows file lock that a short
/// backoff can clear: `ERROR_ACCESS_DENIED` (os error 5) or
/// `ERROR_SHARING_VIOLATION` (os error 32), raised while the reaped daemon
/// child's handle has not yet been released by the kernel.
fn is_transient_lock(err: &std::io::Error) -> bool {
    matches!(err.kind(), std::io::ErrorKind::PermissionDenied)
        || matches!(err.raw_os_error(), Some(5 | 32))
}

/// [`std::fs::remove_file`] with a bounded backoff over transient Windows file
/// locks. Returns the final error if every attempt fails (so a genuine,
/// non-transient failure still surfaces).
fn remove_file_retry(path: &std::path::Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) if attempt < REMOVE_RETRIES && is_transient_lock(&e) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(20 * u64::from(attempt)));
            }
            Err(e) => return Err(e),
        }
    }
}

/// [`std::fs::remove_dir_all`] with the same bounded backoff as
/// [`remove_file_retry`]. A missing directory is treated as success.
fn remove_dir_all_retry(path: &std::path::Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) if attempt < REMOVE_RETRIES && is_transient_lock(&e) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(20 * u64::from(attempt)));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Locates the daemon binary from the environment (env override → Cargo → PATH fallback).
fn resolve_daemon_bin() -> PathBuf {
    if let Ok(explicit) = std::env::var(DAEMON_BIN_ENV) {
        return PathBuf::from(explicit);
    }
    if let Ok(cargo) = std::env::var(CARGO_BIN_ENV) {
        return PathBuf::from(cargo);
    }
    PathBuf::from("continuity_daemon")
}

/// Creates a unique temporary directory for a bench run.
fn make_tempdir() -> Result<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "familyclaw-bench-{}-{}",
        std::process::id(),
        uniq_suffix()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Produces a coarse unique suffix for the directory name (no clock
/// dependency for determinism — only used to isolate directories across
/// concurrent runs).
fn uniq_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:08x}")
}

/// Converts a path into a command-line argument (UTF-8; non-UTF-8 paths are lossy).
fn path_arg(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// [`CrashPoint`] as the daemon's `--crash-at` value (snake_case).
fn crash_point_arg(point: CrashPoint) -> &'static str {
    match point {
        CrashPoint::BeforeWrite => "before_write",
        CrashPoint::MidWrite => "mid_write",
        CrashPoint::MidReplay => "mid_replay",
        // CorruptedJournal/Clean aren't passed to the daemon directly —
        // handled on the caller side. Return a safe default.
        CrashPoint::CorruptedJournal | CrashPoint::Clean => "clean",
    }
}

/// Corrupts a NON-final line of the journal (the CorruptedJournal attack, design §5).
///
/// This is genuine corruption (as opposed to a torn last line): if there are
/// at least two lines, the first line is replaced with garbage. `replay_from`
/// returns a [`CorruptEntry`](familyclaw_durable::DurableError::CorruptEntry)
/// for this — which resume treats as an error (no silent data loss).
fn corrupt_middle_line(journal: &std::path::Path) -> Result<()> {
    if !journal.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(journal)?;
    let mut lines: Vec<String> = contents.lines().map(ToString::to_string).collect();
    if lines.len() < 2 {
        return Ok(());
    }
    lines[0] = "{ this is a corrupted middle line".to_string();
    let mut rebuilt = lines.join("\n");
    rebuilt.push('\n');
    std::fs::write(journal, rebuilt)?;
    Ok(())
}
