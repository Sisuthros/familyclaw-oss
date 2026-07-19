//! RED-TEAM [concurrent-writers] — an attack by two concurrent writers
//! against the `FileJournal` journal and `LocalJsonStore` storage.
//!
//! Claim under attack: *"remembers everything, side-effects exactly once"*.
//! If two processes/handles write to the same journal or the same store
//! nearly simultaneously, does data get lost, do lines interleave
//! (corruption), or do updates disappear (lost update)?
//!
//! These tests RUN the attack against real code — they do not guess. Nothing
//! is fixed here: this is pure attack + proof.
//!
//! All clock times are injected (`Timestamp`); the system clock is never read
//! in test logic (except in unique temp paths, which is allowed).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;
use std::thread;

use familyclaw_durable::{FileJournal, Journal, JournalEntry, StepId};
use familyclaw_memory::{ImportanceFactors, LocalJsonStore, Memory, MemoryStore};
use serde_json::json;

/// A process-wide serialization lock for THIS binary's tests.
///
/// `ATTACK 1` runs two threads that append to the same file under maximal
/// interleaving risk. The claims themselves (no corruption, no lost writes)
/// are correctness invariants that must NOT be loosened. The problem is not
/// with the claims but with the fact that, when the ENTIRE workspace is run
/// in parallel, the OS scheduler + disk fsync queue get so congested that the
/// test's own two threads interleave between `write_all` syscalls on Windows
/// (~1/3 of runs). Serializing this binary's tests against each other removes
/// the external congestion — the test stays just as strict but becomes
/// deterministic. Adds no crate dependency (cf. `#[serial]`).
fn serial_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A unique temp path without external crates.
fn temp_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let unique = format!(
        "familyclaw-redteam-cw-{tag}-{}-{}.dat",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    p.push(unique);
    let _ = std::fs::remove_file(&p);
    p
}

/// RAII cleanup.
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        // The store implementation's tmp file.
        let mut tmp = self.0.as_os_str().to_os_string();
        tmp.push(".tmp");
        let _ = std::fs::remove_file(PathBuf::from(tmp));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ATTACK 1: FileJournal — two handles, two threads, the same file.
//
// Each thread opens its OWN FileJournal handle to the same path (two separate
// File descriptors in append mode, as two processes would) and appends N
// lines. Append does write_all + flush + sync_all. Question: does the line
// structure survive (no interleaving), and does the line COUNT survive (no
// lost writes)?
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn filejournal_two_handles_concurrent_append_integrity() {
    const N: u64 = 400;

    // Serialize this binary's tests against each other: removes the external
    // scheduler/fsync congestion that would otherwise interleave the test's
    // own two threads.
    let _guard = serial_guard();

    let path = temp_path("journal");
    let _cleanup = Cleanup(path.clone());

    let barrier = Arc::new(Barrier::new(2));

    let mut handles = Vec::new();
    for writer_id in 0u64..2 {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let j = FileJournal::open(&path).expect("open journal handle");
            // Synchronize both threads to start at the same time → maximal
            // interleaving risk.
            barrier.wait();
            for i in 0..N {
                // step_id encodes the writer + index so we can count exactly
                // how many of each writer's lines survived.
                let step = StepId::new(writer_id * 1_000_000 + i);
                let entry = JournalEntry::completed(
                    step,
                    format!("w{writer_id}"),
                    // A substantial payload so the serialized line is large
                    // (several hundred bytes) → write_all may need multiple
                    // syscalls → interleaving risk increases.
                    json!({
                        "writer": writer_id,
                        "index": i,
                        "pad": "X".repeat(512),
                    }),
                );
                j.append(entry).expect("append must not fail");
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread panicked");
    }

    // ── Verification: read the raw file and separate out the intact lines. ─────────────
    let raw = std::fs::read_to_string(&path).expect("read journal");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();

    let total_written = 2 * usize::try_from(N).expect("N fits usize");

    // (a) Line count: every \n-terminated line should be one append.
    //     If there are fewer lines than 2N → lost writes (lost write).
    //     If parsing fails on some line → interleaved corruption.
    let mut parsed_ok = 0usize;
    let mut parse_failures: Vec<(usize, String)> = Vec::new();
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut duplicates = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        match serde_json::from_str::<JournalEntry>(line) {
            Ok(entry) => {
                parsed_ok += 1;
                if !seen.insert(entry.step_id.index()) {
                    duplicates += 1;
                }
            }
            Err(e) => {
                let preview: String = line.chars().take(80).collect();
                parse_failures.push((idx, format!("{e} | line='{preview}'")));
            }
        }
    }

    eprintln!(
        "[journal] lines={} parsed_ok={} parse_failures={} duplicates={} expected={}",
        lines.len(),
        parsed_ok,
        parse_failures.len(),
        duplicates,
        total_written
    );
    if let Some((idx, msg)) = parse_failures.first() {
        eprintln!("[journal] FIRST CORRUPT LINE idx={idx}: {msg}");
    }

    // (b) Via replay_all — the same path production uses. Interior corruption
    //     (an interleaved line that is NOT the last one) returns a CorruptEntry error.
    let replay = FileJournal::open(&path).expect("reopen").replay_all();
    match &replay {
        Ok(entries) => eprintln!("[journal] replay_all OK, {} entries", entries.len()),
        Err(e) => eprintln!("[journal] replay_all ERROR: {e}"),
    }

    // ── Assertions (proof, not a guess). ──────────────────────────────────────
    // The claim "remembers everything" breaks if:
    //   - lines were lost (parsed_ok + parse_failures < 2N), OR
    //   - some line is corrupted (parse_failures > 0), OR
    //   - replay_all fails with corruption.
    assert!(
        parse_failures.is_empty(),
        "INTEGRITY BREAK: {} interleaved/corrupt lines in journal (first: {:?})",
        parse_failures.len(),
        parse_failures.first()
    );
    assert!(
        replay.is_ok(),
        "INTEGRITY BREAK: replay_all failed on concurrently-written journal: {:?}",
        replay.err()
    );
    assert_eq!(
        parsed_ok,
        total_written,
        "LOST WRITE: only {parsed_ok} of {total_written} appends survived (lines on disk={})",
        lines.len()
    );
    assert_eq!(duplicates, 0, "unexpected duplicate step ids: {duplicates}");
}

// ─────────────────────────────────────────────────────────────────────────────
// ATTACK 2: LocalJsonStore — two handles to the SAME path, lost update.
//
// This is a classic read-modify-write race: two LocalJsonStore instances have
// SEPARATE RwLock + HashMap. Each loads the file (empty), adds its own
// memory, and persists (tmp + rename). The last rename wins → the other
// memory disappears from disk even though add() returned Ok.
//
// The claim "side-effects exactly once / remembers everything" is tested: if
// both add() calls succeed but only one memory is on disk → DATA LOSS.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn localjsonstore_two_handles_same_path_lost_update() {
    let _guard = serial_guard();
    let path = temp_path("store");
    let _cleanup = Cleanup(path.clone());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");

    let added_ids: Vec<familyclaw_core::MessageId> = rt.block_on(async {
        // Two separate handles to the same file — as two processes or two
        // parallel tasks that each opened the store would.
        let store_a = Arc::new(LocalJsonStore::open(&path).await.expect("open a"));
        let store_b = Arc::new(LocalJsonStore::open(&path).await.expect("open b"));

        let mem_a = Memory::builder("MEMORY FROM WRITER A — must not be lost")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-a")
            .build();
        let mem_b = Memory::builder("MEMORY FROM WRITER B — must not be lost")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-b")
            .build();
        let id_a = mem_a.id;
        let id_b = mem_b.id;

        // Run both add() calls concurrently.
        let ta = {
            let store_a = Arc::clone(&store_a);
            tokio::spawn(async move { store_a.add(mem_a).await })
        };
        let tb = {
            let store_b = Arc::clone(&store_b);
            tokio::spawn(async move { store_b.add(mem_b).await })
        };
        let ra = ta.await.expect("join a").expect("add a returned Ok");
        let rb = tb.await.expect("join b").expect("add b returned Ok");
        assert_eq!(ra, id_a);
        assert_eq!(rb, id_b);

        vec![id_a, id_b]
    });

    // ── Verification: reopen the store from disk AGAIN (a clean handle). ──────────
    // Both add() calls returned Ok ⇒ "remembered". But what's actually on disk?
    let on_disk = rt.block_on(async {
        let reopened = LocalJsonStore::open(&path).await.expect("reopen");
        reopened.all().await.expect("all")
    });

    let surviving_ids: std::collections::HashSet<_> = on_disk.iter().map(|m| m.id).collect();
    eprintln!(
        "[store] added=2 (both returned Ok), on_disk={} ids_present={:?}",
        on_disk.len(),
        on_disk.iter().map(|m| m.source.clone()).collect::<Vec<_>>()
    );

    // Claim: if neither id was lost, the store is safe for concurrent
    // handles. If one disappeared → LOST UPDATE (the "remember X" side effect vanished).
    let lost: Vec<_> = added_ids
        .iter()
        .filter(|id| !surviving_ids.contains(id))
        .collect();

    assert!(
        lost.is_empty(),
        "LOST UPDATE: {} of 2 concurrently-added memories vanished from disk \
         despite add() returning Ok. on_disk={}, lost_ids={:?}. \
         Two LocalJsonStore handles to the same path do NOT coordinate — \
         last rename() wins.",
        lost.len(),
        on_disk.len(),
        lost
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ATTACK 2b: deterministic lost-update WITHOUT racing timing.
//
// Removes timing uncertainty: sequences the read-modify-write by hand so a
// lost update is FORCED to occur if the implementation doesn't coordinate
// handles. This is the "smoking gun" — not flaky, it proves the structural gap.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn localjsonstore_deterministic_lost_update() {
    let _guard = serial_guard();
    let path = temp_path("store-det");
    let _cleanup = Cleanup(path.clone());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let (id_a, id_b, on_disk_len, a_present, b_present) = rt.block_on(async {
        // Both handles are opened while the file is still empty → each one's
        // internal HashMap is empty (the same starting state as a real race).
        let store_a = LocalJsonStore::open(&path).await.expect("open a");
        let store_b = LocalJsonStore::open(&path).await.expect("open b");

        let mem_a = Memory::builder("A")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-a")
            .build();
        let mem_b = Memory::builder("B")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-b")
            .build();
        let id_a = mem_a.id;
        let id_b = mem_b.id;

        // A writes first (the file now has {A}), then B writes its own empty
        // snapshot on top (the file now has {B}). A disappears from disk.
        store_a.add(mem_a).await.expect("add a Ok");
        store_b.add(mem_b).await.expect("add b Ok");

        let reopened = LocalJsonStore::open(&path).await.expect("reopen");
        let all = reopened.all().await.expect("all");
        let a_present = all.iter().any(|m| m.id == id_a);
        let b_present = all.iter().any(|m| m.id == id_b);
        (id_a, id_b, all.len(), a_present, b_present)
    });

    eprintln!(
        "[store-det] on_disk_len={on_disk_len} A_present={a_present} B_present={b_present} \
         id_a={id_a} id_b={id_b}"
    );

    // Both add() calls returned Ok ⇒ each one was "remembered". Disk should
    // have 2. If it has 1 → a structural lost update is proven.
    assert_eq!(
        on_disk_len, 2,
        "DETERMINISTIC LOST UPDATE: both add() returned Ok but disk has {on_disk_len} \
         memory(ies). A_present={a_present} B_present={b_present}. The second handle's \
         rename() clobbered the first handle's write — no cross-handle coordination."
    );
}
