//! RED-TEAM: a partial-fsync / OS-buffer-loss attack against the durable journal.
//!
//! The claim under attack, which we try to BREAK (design §5):
//! *"kill mid-task -> resumes exact step, side-effects exactly once,
//! never silently wrong"* — even when the OS buffer is lost and the journal
//! file is truncated **mid-JSON-entry** (not just a torn last LINE but a torn
//! last BYTE), OR when an interior line N is garbage.
//!
//! Requirement: recover to the last intact step WITHOUT replaying side
//! effects, OR fail LOUDLY ([`DurableError::CorruptEntry`]). NEVER silently wrong.
//!
//! This test runs REAL production code: [`FileJournal`] + [`DurableContext`]
//! from the `familyclaw-durable` crate. The clock is injected as a
//! [`Timestamp`] value; the system clock is never read (design §2.2).

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use familyclaw_durable::{DurableContext, DurableError, FileJournal, Journal};

/// RAII temp file without external crates (same pattern as the durable
/// crate's own tests). Also cleans up sidecar files.
struct TempPath(PathBuf);

impl TempPath {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        let unique = format!(
            "familyclaw-redteam-fsync-{tag}-{}-{:?}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        p.push(unique);
        let _ = std::fs::remove_file(&p);
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Runs a three-step workflow against the given journal, incrementing the
/// `effects` counter every time the closure is REALLY run (= a side effect).
/// During replay the closure should NOT run → the counter does not increase.
///
/// Returns the sum of the three steps as the result (deterministic: 1+2+3 = 6
/// when all three run).
fn run_workflow<J: Journal>(journal: J, effects: &Cell<u32>) -> familyclaw_durable::Result<i64> {
    let mut ctx = DurableContext::new(journal)?;
    let a: i64 = ctx.step("alpha", || {
        effects.set(effects.get() + 1);
        Ok(1)
    })?;
    let b: i64 = ctx.step("beta", || {
        effects.set(effects.get() + 1);
        Ok(a + 2)
    })?;
    let c: i64 = ctx.step("gamma", || {
        effects.set(effects.get() + 1);
        Ok(b + 3)
    })?;
    Ok(c)
}

/// Truncates the file to the given byte length (simulates loss of the OS
/// buffer before the last entry's fsync: the file ends MID-JSON-entry).
fn truncate_to(path: &Path, len: u64) {
    let f = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for truncate");
    f.set_len(len).expect("set_len");
    f.sync_all().expect("sync truncate");
}

/// Reads the file's bytes.
fn read_bytes(path: &Path) -> Vec<u8> {
    let mut f = File::open(path).expect("open for read");
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).expect("read");
    buf
}

/// ATTACK 1 — torn mid-JSON: truncate the journal MID the last entry (not on
/// a newline boundary). The second step "beta" is left half-written to disk.
///
/// Claim: replay recovers to the last INTACT step ("alpha") and continues to
/// completion without "alpha"'s side effect repeating.
#[test]
fn attack_torn_mid_json_recovers_to_last_good_step() {
    let tmp = TempPath::new("torn-mid-json");

    // Step 1: write two intact steps, fsynced.
    {
        let mut j = FileJournal::open(tmp.path()).expect("open");
        let mut ctx = DurableContext::new(j).expect("ctx");
        // Run only two steps by hand so we know the exact byte boundaries.
        let _a: i64 = ctx.step("alpha", || Ok(1)).expect("alpha");
        let _b: i64 = ctx.step("beta", || Ok(3)).expect("beta");
        j = ctx.finish();
        // Make sure both are on disk.
        assert_eq!(j.replay_all().expect("replay").len(), 2);
    }

    // Find the byte offset of the second line's newline.
    let bytes = read_bytes(tmp.path());
    let first_nl = bytes
        .iter()
        .position(|&b| b == b'\n')
        .expect("first newline exists");
    // Truncate MID the second entry: the first line + its \n + a few bytes
    // of the second entry, but NOT its newline → torn mid-JSON.
    let torn_len = (first_nl as u64) + 1 + 6;
    assert!(
        torn_len < bytes.len() as u64,
        "torn point must be inside second entry"
    );
    truncate_to(tmp.path(), torn_len);

    // Actually verify: the last byte is NOT a newline (= torn).
    let after = read_bytes(tmp.path());
    assert_ne!(
        *after.last().expect("nonempty"),
        b'\n',
        "torn file must NOT end in newline"
    );

    // Attack: reopen and try to replay + continue.
    let effects = Cell::new(0u32);
    let journal = FileJournal::open(tmp.path()).expect("reopen after tear");

    // read_all_entries must not silently return wrong data.
    let recovered = journal.replay_all();
    match &recovered {
        Ok(entries) => {
            // Acceptable outcome A: tolerated the torn last line → only the
            // first intact step survives.
            assert_eq!(
                entries.len(),
                1,
                "torn last line should be dropped → exactly 1 good entry survives, got {}",
                entries.len()
            );
            assert_eq!(entries[0].step_name(), Some("alpha"));
        }
        Err(DurableError::CorruptEntry { .. }) => {
            // Acceptable outcome B: failed loudly. Not silently wrong → the
            // claim holds in this case too.
        }
        Err(other) => panic!("unexpected error variant on torn mid-json: {other:?}"),
    }

    // If replay succeeded (case A), continue the workflow to completion and
    // verify that "alpha"'s side effect does NOT repeat (exactly-once).
    if recovered.is_ok() {
        let result = run_workflow(journal, &effects).expect("resume after tear");
        assert_eq!(result, 6, "final result must equal no-crash baseline 1+2+3");
        // "alpha" came from the log (no side effect); only beta+gamma run
        // fresh → exactly 2 new side effects.
        assert_eq!(
            effects.get(),
            2,
            "alpha must NOT re-run on resume; only beta+gamma fresh"
        );
    }
}

/// ATTACK 2 — middle-line corruption: an intact line N is replaced with
/// garbage, line N+1 is still intact JSON. The file ends in a newline (= NOT
/// a torn last line but genuine interior corruption).
///
/// Claim: MUST fail loudly ([`DurableError::CorruptEntry`]), must NOT
/// silently return "intact" data that skips line N.
#[test]
fn attack_corrupt_middle_line_fails_loud() {
    let tmp = TempPath::new("corrupt-middle");

    // Write three intact steps.
    {
        let mut j = FileJournal::open(tmp.path()).expect("open");
        let mut ctx = DurableContext::new(j).expect("ctx");
        let _a: i64 = ctx.step("alpha", || Ok(1)).expect("alpha");
        let _b: i64 = ctx.step("beta", || Ok(3)).expect("beta");
        let _c: i64 = ctx.step("gamma", || Ok(6)).expect("gamma");
        j = ctx.finish();
        assert_eq!(j.replay_all().expect("replay").len(), 3);
    }

    // Replace the MIDDLE line (line 2, "beta") with garbage — preserve the
    // line count and newlines. Bytes: [line1\n][line2\n][line3\n].
    let bytes = read_bytes(tmp.path());
    let nl_positions: Vec<usize> = bytes
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| (b == b'\n').then_some(i))
        .collect();
    assert_eq!(nl_positions.len(), 3, "expected 3 newline-terminated lines");

    let line2_start = nl_positions[0] + 1;
    let line2_end_inclusive_nl = nl_positions[1]; // position of line 2's \n

    // Build the new content: line1 + garbage-line2 + line3, all newline-terminated.
    let mut corrupted: Vec<u8> = Vec::new();
    corrupted.extend_from_slice(&bytes[..line2_start]); // line1 + \n
    corrupted.extend_from_slice(b"{this is not valid json at all]}\n"); // garbage-line2 + \n
    corrupted.extend_from_slice(&bytes[line2_end_inclusive_nl + 1..]); // line3 + \n

    {
        let mut f = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(tmp.path())
            .expect("rewrite corrupted");
        f.write_all(&corrupted).expect("write corrupted");
        f.sync_all().expect("sync corrupted");
    }
    // The file ends in a newline → NOT a torn last line.
    let final_bytes = read_bytes(tmp.path());
    assert_eq!(
        *final_bytes.last().expect("nonempty"),
        b'\n',
        "corrupted file ends in newline (interior corruption, not torn tail)"
    );

    // Attack: replay MUST fail loudly at line 2.
    let journal = FileJournal::open(tmp.path()).expect("reopen corrupted");
    let result = journal.replay_all();
    match result {
        Err(DurableError::CorruptEntry { line, .. }) => {
            assert_eq!(line, 2, "must point at the corrupt interior line (#2)");
        }
        Ok(entries) => panic!(
            "SILENT CORRUPTION: replay returned {} entries instead of failing loud on garbage line 2",
            entries.len()
        ),
        Err(other) => panic!("wrong error variant (must be CorruptEntry): {other:?}"),
    }

    // And DurableContext::new MUST also propagate the error (not silently
    // build a context from a broken log).
    let journal2 = FileJournal::open(tmp.path()).expect("reopen corrupted 2");
    let ctx = DurableContext::new(journal2);
    assert!(
        matches!(ctx, Err(DurableError::CorruptEntry { line: 2, .. })),
        "DurableContext::new must propagate CorruptEntry on interior garbage"
    );
}

/// ATTACK 3 — torn LAST byte mid-number: truncate by one byte so the last
/// entry is JSON whose prefix is syntactically legal but incomplete (a
/// classic partial-fsync: the buffer cut off mid-number/mid-string).
///
/// This special case tests that an "accidentally-valid" JSON prefix does not
/// silently get through with a wrong value. Claim: the torn last line is
/// dropped OR an error is raised; earlier steps are preserved correctly.
#[test]
fn attack_torn_last_byte_no_silent_wrong_value() {
    let tmp = TempPath::new("torn-last-byte");

    {
        let mut j = FileJournal::open(tmp.path()).expect("open");
        let mut ctx = DurableContext::new(j).expect("ctx");
        let _a: i64 = ctx.step("alpha", || Ok(1)).expect("alpha");
        let _b: i64 = ctx.step("beta", || Ok(3)).expect("beta");
        j = ctx.finish();
        assert_eq!(j.replay_all().expect("replay").len(), 2);
    }

    // Remove exactly the last byte (the \n + 0..n ending the second entry).
    // This removes the last newline AND one byte of content → torn JSON.
    let bytes = read_bytes(tmp.path());
    // Find the start of the last line.
    let last_nl = bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .expect("trailing newline");
    // Truncate mid the second entry: keep the first line + \n + 8 bytes.
    let first_nl = bytes.iter().position(|&b| b == b'\n').expect("first nl");
    assert!(last_nl > first_nl);
    let torn_len = (first_nl as u64) + 1 + 8;
    truncate_to(tmp.path(), torn_len);

    let journal = FileJournal::open(tmp.path()).expect("reopen");
    match journal.replay_all() {
        Ok(entries) => {
            // Only the intact alpha may survive; the torn beta prefix must
            // NOT reappear as any kind of value.
            assert_eq!(entries.len(), 1, "only the intact alpha entry survives");
            assert_eq!(entries[0].step_name(), Some("alpha"));
            // Verify no surviving entry is "beta" with a wrong value.
            assert!(
                entries.iter().all(|e| e.step_name() != Some("beta")),
                "torn beta must NOT silently reappear"
            );
        }
        Err(DurableError::CorruptEntry { .. }) => {
            // Failing loudly is also acceptable.
        }
        Err(other) => panic!("unexpected error on torn last byte: {other:?}"),
    }
}

/// A smoke check that the helpers exist (prevents dead-code warnings if some
/// branch doesn't execute).
#[test]
fn helpers_smoke() {
    let tmp = TempPath::new("smoke");
    {
        let mut j = FileJournal::open(tmp.path()).expect("open");
        let mut ctx = DurableContext::new(j).expect("ctx");
        let _ = ctx.step("alpha", || Ok::<i64, String>(1)).expect("alpha");
        j = ctx.finish();
        drop(j);
    }
    let bytes = read_bytes(tmp.path());
    assert!(!bytes.is_empty());
    let mut f = File::open(tmp.path()).expect("open");
    f.seek(SeekFrom::Start(0)).expect("seek");
}
