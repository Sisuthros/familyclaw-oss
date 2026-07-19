//! [`FileJournal`] — crash-resistant append-only JSONL journal.
//!
//! Every [`JournalEntry`] is written as a single JSON line (`\n`-terminated)
//! at the end of the file. The write is flushed and fsynced
//! ([`std::fs::File::sync_all`]) before [`append`](crate::Journal::append)
//! returns, so a completed step is on disk even after a sudden crash.
//!
//! ## Crash resistance
//! If the process crashes mid-write, the last line can be left incomplete
//! (missing the `\n` terminator, or truncated JSON). [`replay_from`](crate::Journal::replay_from) tolerates
//! **exactly this one case**: the file's *last* line, if its parse fails AND
//! it is missing the line terminator, is silently discarded as an incomplete
//! write. Any *earlier* corrupted line is genuine corruption and is returned
//! as [`crate::DurableError::CorruptEntry`].
//!
//! ## Self-healing on open (heal-on-open)
//! **Tolerating** an incomplete last line during reads is not enough — if it
//! is not removed from disk, the next [`append`](crate::Journal::append) will
//! attach to the SAME physical line (because the fragment is missing its
//! `\n`), causing the fragment and the fresh line to fuse into a single
//! internal corruption that breaks all subsequent reads. That's why
//! [`FileJournal::open`] **truncates** such a newline-less fragment on open:
//! the fragment is always an unfinished (unfsynced) write that never
//! completed, so discarding it is both safe AND necessary for append to
//! continue from a clean line boundary.
//!
//! ## Compaction — [`FileJournal::rewrite`]
//! An append-only log grows without bound if the state built on top of it
//! (e.g. pending approvals, resumable turns) records `put`/`delete` rows:
//! deleted and superseded rows remain as dead rows in the log, causing the
//! file to bloat and replay to become O(n) in row count.
//! [`FileJournal::rewrite`] replaces **the entire log** with a given set of
//! rows **atomically**: the rows are first written to a temporary file
//! created in the same directory (flush + fsync), after which the temporary
//! file is **renamed** over the live file (`fs::rename`, atomic on the same
//! filesystem). If the process crashes mid-compaction, the live file is
//! still in its old (intact) state — a half-written log never results. It is
//! the caller's **responsibility** to give `rewrite` exactly the rows that
//! describe the desired end state (typically just the live records, with
//! dead tombstones dropped).
//!
//! ## Compaction without a TOCTOU gap — [`FileJournal::compact_with`]
//! [`FileJournal::rewrite`] replaces the log atomically with respect to
//! disk, but if the caller reads the state (replay) BEFORE `rewrite` and
//! **releases the lock in between**, a time-of-check-to-time-of-use gap
//! arises: a concurrent append can write to the old file just before
//! `rewrite` overwrites it with a snapshot taken before the gap → the append
//! **disappears silently**. [`FileJournal::compact_with`] closes the gap by
//! holding the **same** file lock for the entire read→filter→swap operation,
//! so no append can slip in between. The caller supplies a `build` closure
//! that receives the read rows and returns the ones to keep; the swap itself
//! is identical to `rewrite`'s.
//!
//! ## Object Safety
//! Methods take `&self` so the trait is `dyn`-compatible. The file handle is
//! guarded by a `Mutex<File>`.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::entry::{JournalEntry, StepId};
use crate::error::{DurableError, Result};
use crate::journal::Journal;

/// Disk-backed append-only JSONL journal.
///
/// Holds an open file handle for writing and remembers the path for reading.
/// Opening creates the file if it does not exist; an existing file is
/// continued (append mode). The file handle is guarded by a Mutex so the
/// trait is `dyn`-compatible (`&self` methods).
#[derive(Debug)]
pub struct FileJournal {
    path: PathBuf,
    file: Mutex<File>,
}

impl FileJournal {
    /// Opens (or creates) the journal at the given path in append mode.
    ///
    /// Rows in an existing file are preserved — new rows are appended at the
    /// end.
    ///
    /// # Errors
    /// [`DurableError::Io`] if the file cannot be opened/created.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        // Self-heal BEFORE opening the write handle: if a crash left a
        // newline-less fragment at the end of the file, it is truncated away.
        // Otherwise the next append would attach to the same physical line and
        // permanently corrupt the journal (internal corruption). See the
        // module doc "heal-on-open".
        heal_torn_trailing_fragment(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// Returns the journal's file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads and parses all rows, tolerating an incomplete last line
    /// (a crash artifact). The returned rows are in file order.
    fn read_all_entries(&self) -> Result<Vec<JournalEntry>> {
        // Poison recovery instead of letting `unwrap()` panic: if some other
        // thread panicked while holding the lock, the file handle is still
        // valid (nothing is left half-done on the `read_all_entries` path).
        // `into_inner()` takes ownership of the handle without panicking →
        // does not violate the error.rs:5 invariant ("no unwrap/expect/panic
        // on the production path").
        let _file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The actual parsing does not use the `self.file` handle but opens a
        // fresh read handle from the path — `parse_entries_from_path` therefore
        // does NOT lock `self.file` again (if it did, the lock would already be
        // held and std Mutex is NOT reentrant → deadlock). The lock is held for
        // the duration of this call so the read is consistent with respect to
        // concurrent appends.
        Self::parse_entries_from_path(&self.path)
    }

    /// Parses **all** journal rows from the given path, tolerating an
    /// incomplete last line (a crash artifact). The returned rows are in
    /// file order.
    ///
    /// ## No locking — intentionally
    /// This helper **does not** lock the `self.file` mutex: it opens its own
    /// fresh read handle from the path. The reason is non-reentrancy: both
    /// `read_all_entries` and [`compact_with`](FileJournal::compact_with)
    /// already lock `self.file` before calling this, and std [`Mutex`] **is
    /// not reentrant** — if this tried to lock the lock again from within the
    /// same thread, the result would be a **deadlock**. That's why parsing is
    /// factored out into a lockless helper that both lock holders can call
    /// safely.
    fn parse_entries_from_path(path: &Path) -> Result<Vec<JournalEntry>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        // Collect (line number, content) so an incomplete last line can be
        // identified reliably.
        let mut raw_lines: Vec<(u64, String)> = Vec::new();
        let mut had_trailing_newline = true;
        let mut line_no: u64 = 0;
        for line in reader.lines() {
            let line = line?;
            line_no += 1;
            // `BufRead::lines` strips the `\n`; we don't directly know whether
            // the last line had a line terminator. That is determined separately
            // below.
            raw_lines.push((line_no, line));
        }

        // Determine whether the file ended with a line terminator: if not, the
        // last line is a potentially incomplete write.
        if let Some(last_byte) = last_byte_of(path)? {
            had_trailing_newline = last_byte == b'\n';
        }

        let total = raw_lines.len();
        let mut entries = Vec::with_capacity(total);
        for (idx, (line_no, content)) in raw_lines.into_iter().enumerate() {
            let is_last = idx + 1 == total;
            // Skip empty lines (e.g. an extra trailing newline).
            if content.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<JournalEntry>(&content) {
                Ok(entry) => entries.push(entry),
                Err(parse_err) => {
                    if is_last && !had_trailing_newline {
                        // The classic crash artifact: the last line was left
                        // incomplete and has no terminating line break. Discard
                        // it silently.
                        continue;
                    }
                    return Err(DurableError::corrupt(
                        line_no,
                        format!("invalid json: {parse_err}"),
                    ));
                }
            }
        }
        Ok(entries)
    }

    /// Replaces **the entire log** with the given rows atomically (compaction).
    ///
    /// Purpose: state built on top of an append-only log (e.g. pending
    /// approvals / resumable turns) accumulates dead rows (deletions and
    /// replacements) that replay still has to read. This method rewrites the
    /// log to contain **only** the given rows — the caller typically supplies
    /// just the live records, so dead rows disappear and the file shrinks.
    ///
    /// ## Atomicity (never corrupts the live file)
    /// 1. The rows are written to a temporary file created **in the same
    ///    directory** (`<path>.compact-<pid>-<time>.tmp`).
    /// 2. The temporary file is flushed and **fsynced** ([`File::sync_all`]).
    /// 3. The temporary file is **renamed** over the live file
    ///    ([`std::fs::rename`]) — a same-filesystem rename is atomic: a reader
    ///    sees either the old or the new file, never a half-written one.
    /// 4. The internal write handle is swapped to point at the new (renamed)
    ///    file in append mode, so future [`append`](Journal::append) calls
    ///    continue after the compacted log.
    ///
    /// If the process crashes **before** the rename, the live file is
    /// untouched (the old intact state is preserved) and the temporary file
    /// is left orphaned (harmless; the next `rewrite` overwrites its own
    /// unique name). If it crashes **after the rename**, the new compacted
    /// file is already in place and intact. In neither case do live rows get
    /// lost.
    ///
    /// Rows are written in the given order; [`StepId`]s are preserved as-is
    /// (the caller may renumber them before the call if a tight 0..N sequence
    /// is desired). An empty `entries` clears the log entirely.
    ///
    /// # Errors
    /// [`DurableError::Io`] if creating, writing, fsyncing, or renaming the
    /// temporary file, or opening the new handle, fails;
    /// [`DurableError::Serde`] if serializing any row fails. On error, the
    /// live file is left unchanged (the rename only happens once the temp
    /// file is intact on disk).
    pub fn rewrite(&self, entries: &[JournalEntry]) -> Result<()> {
        // The lock is held for the entire swap: append must not write to the
        // old handle between the rename and the handle swap.
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Atomic swap in a shared helper (same logic as in `compact_with`).
        // `&mut *file` gives the helper the contents of the already-held lock,
        // so it does NOT lock `self.file` again (std Mutex is not reentrant).
        self.atomic_swap_locked(&mut file, entries)
    }

    /// Compacts the log **atomically against appends**: locks `self.file`
    /// **once**, reads the entire current log while holding the lock, hands
    /// the rows to the `build` closure (which returns the rows to keep), and
    /// performs the same atomic temp + fsync + rename + handle-swap as
    /// [`rewrite`](FileJournal::rewrite) — **all under the same,
    /// still-held lock**. Returns the number of dropped rows (read − kept,
    /// floored at zero).
    ///
    /// ## Why atomic against appends (closing the TOCTOU gap)
    /// The previous compaction approach read the state
    /// ([`replay_all`](Journal::replay_all)), **released the lock**, built
    /// the live rows, and only then called [`rewrite`](FileJournal::rewrite)
    /// (which locked again). Between the lock release and the rewrite, a
    /// concurrent append could write to the **old** file — and the rewrite
    /// would overwrite it with the snapshot taken before the gap, so the
    /// append **disappeared silently**. `compact_with` removes the gap:
    /// because the lock is held for the reading, building, AND swap, no
    /// append can slip in between — appends either complete before the lock
    /// is acquired (and show up in `build`'s rows) or queue up for the
    /// compacted log after the swap.
    ///
    /// ## Non-reentrancy (why `build` must not call back in)
    /// The lock is already held when `build` runs, and std [`Mutex`] **is not
    /// reentrant**. That's why this method does NOT call `read_all_entries`
    /// or [`rewrite`](FileJournal::rewrite) internally (both would lock
    /// `self.file` again → **deadlock**). Instead it uses the lockless
    /// parsing helper `parse_entries_from_path` and the lockless swap helper
    /// `atomic_swap_locked` (private helpers that do not lock `self.file`).
    /// The `build` closure must ALSO not call any locking method of this
    /// journal (`append`, `replay_*`, `rewrite`, `compact_with`) — that would
    /// lead to the same deadlock. By contract, `build` performs only pure
    /// row filtering/renumbering.
    ///
    /// ## Atomicity against disk
    /// Same guarantee as [`rewrite`](FileJournal::rewrite): the rows are
    /// first written to a temporary file (flush + fsync), which is then
    /// renamed atomically over the live file. If the process crashes before
    /// the rename, the live file is still in its intact old state; if it
    /// crashes after the rename, the new compacted file is already in place.
    /// Live rows are not lost in either case.
    ///
    /// # Errors
    /// [`DurableError::Io`] if reading, writing the temporary file, fsyncing,
    /// renaming, or opening the new handle fails; [`DurableError::Serde`] if
    /// serializing a row fails; or the error returned by the `build` closure,
    /// as-is. On error, the live file is left unchanged.
    pub fn compact_with<F>(&self, build: F) -> Result<usize>
    where
        F: FnOnce(Vec<JournalEntry>) -> Result<Vec<JournalEntry>>,
    {
        // The lock is taken ONCE and held for the entire read→filter→swap
        // operation. This is the crux of the whole TOCTOU fix: appends cannot
        // slip in between.
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Read the entire current log with the lockless helper (the lock is
        // ALREADY held → we must not call `read_all_entries`, which would lock
        // again).
        let current = Self::parse_entries_from_path(&self.path)?;
        let read_count = current.len();

        // The caller builds the rows to keep (filters dead ones, renumbers
        // StepIds). The error is returned as-is; the live file is still
        // untouched (the swap has not happened yet).
        let kept = build(current)?;
        let kept_count = kept.len();

        // Atomic swap under the same, still-held lock.
        self.atomic_swap_locked(&mut file, &kept)?;

        Ok(read_count.saturating_sub(kept_count))
    }

    /// Performs the atomic temp + fsync + rename + handle-swap **assuming the
    /// caller already holds the `self.file` lock** (`file` = that lock guard).
    ///
    /// Factored out as a shared helper so that [`rewrite`](FileJournal::rewrite)
    /// and [`compact_with`](FileJournal::compact_with) perform exactly the
    /// same swap. **Does not lock `self.file` again** — std [`Mutex`] is not
    /// reentrant, so the lock is taken only once by the caller and handed in
    /// here as a guard. Steps of the swap:
    /// 1. Serialize all rows into memory (if serde fails, disk is not touched).
    /// 2. Write to a temporary file **in the same directory** (flush + fsync).
    /// 3. Atomically rename the temporary file over the live file.
    /// 4. Swap the write handle to point at the new file in append mode.
    fn atomic_swap_locked(
        &self,
        file: &mut std::sync::MutexGuard<'_, File>,
        entries: &[JournalEntry],
    ) -> Result<()> {
        // 1: serialize ALL rows before touching disk.
        let mut buf = String::new();
        for entry in entries {
            let line = serde_json::to_string(entry)?;
            buf.push_str(&line);
            buf.push('\n');
        }

        // Temporary file in the SAME directory (rename is atomic only within
        // the same filesystem). A unique name prevents collisions between
        // concurrent compactions.
        let tmp_path = self.compaction_tmp_path();

        // 2: write temp + flush + fsync.
        let write_result = (|| -> Result<()> {
            let mut tmp = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)?;
            tmp.write_all(buf.as_bytes())?;
            tmp.flush()?;
            tmp.sync_all()?;
            Ok(())
        })();
        if let Err(e) = write_result {
            // The temp file may have been left incomplete — clean up, live file untouched.
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }

        // 3: atomic rename temp → live file.
        if let Err(e) = std::fs::rename(&tmp_path, &self.path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(DurableError::Io(e));
        }

        // 4: swap the write handle to point at the new file in append mode.
        let new_handle = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        **file = new_handle;

        // Directory fsync is not portable on Windows; rename + temp-fsync
        // provide a sufficient guarantee (rename is atomic, temp data is on disk).
        Ok(())
    }

    /// Builds a unique temporary file path for compaction in the same
    /// directory as the live log (so the rename is atomic).
    fn compaction_tmp_path(&self) -> PathBuf {
        use std::fmt::Write as _;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut name = self.path.file_name().map_or_else(
            || "journal".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        // `write!` to a String cannot fail → the result is deliberately ignored.
        let _ = write!(name, ".compact-{}-{nanos}.tmp", std::process::id());
        match self.path.parent() {
            Some(dir) => dir.join(name),
            None => PathBuf::from(name),
        }
    }
}

/// Truncates a newline-less fragment left by a crash from the end of the file.
///
/// Heal-on-open (see the module doc): a crash mid-[`append`] can leave an
/// incomplete, **newline-less** line at the end of the file. Such a line was
/// never fsynced to completion, so it is not a committed step — and unless it
/// is removed from disk, the next append will attach after it on the SAME
/// physical line and produce permanent internal corruption.
///
/// The behavior is **conservative**: the file is truncated only when
/// 1. the file does not end with `\n` (i.e. the last line is potentially
///    incomplete), AND
/// 2. that last (newline-less) line does NOT parse as an intact
///    [`JournalEntry`].
///
/// If the last line parses intact but is only missing the `\n` (entirely
/// possible if the write got through the body but not the terminating `\n` —
/// in practice `append` writes the line + `\n` as a single `write_all` call,
/// but we are being cautious), the line is NOT truncated — it is a valid step
/// and is preserved. In that case only the missing `\n` is appended, so the
/// next append starts from a clean line without breaking the intact step.
///
/// # Errors
/// [`DurableError::Io`] if reading or truncating the file fails.
fn heal_torn_trailing_fragment(path: &Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    // Nonexistent or empty file: nothing to heal.
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        // The file doesn't exist yet — open will create it later, nothing to heal.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(DurableError::Io(e)),
    };
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }

    // Does the file end with a line terminator? If so, the last line is
    // cleanly terminated and no healing is needed.
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }

    // The file does not end with `\n` → find the start of the last line (the
    // byte after the previous `\n`) by scanning backward. Read the whole file;
    // journals are line-based and not arbitrarily large in a single read.
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.read_to_end(&mut bytes)?;

    // Start offset of the last line = the byte after the last `\n` (or 0).
    let last_line_start = match bytes.iter().rposition(|&b| b == b'\n') {
        Some(pos) => pos + 1,
        None => 0,
    };
    let last_line = &bytes[last_line_start..];

    // An empty last line (e.g. just whitespace): no step to parse, but also
    // no fragment that append would corrupt — leave it alone.
    if last_line.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }

    // Does the last (newline-less) line parse as an intact entry?
    if serde_json::from_slice::<JournalEntry>(last_line).is_ok() {
        // An intact step missing only the terminating `\n`: keep the line, add
        // the `\n` so the next append starts from a clean line.
        file.seek(SeekFrom::End(0))?;
        file.write_all(b"\n")?;
    } else {
        // An incomplete fragment: truncate it away entirely → the file ends at
        // the previous intact line (its `\n`) or becomes empty. Append
        // continues cleanly.
        let new_len = u64::try_from(last_line_start).unwrap_or(0);
        file.set_len(new_len)?;
    }
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Returns the file's last byte, or `None` if the file is empty.
fn last_byte_of(path: &Path) -> Result<Option<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut buf = [0u8; 1];
    file.read_exact(&mut buf)?;
    Ok(Some(buf[0]))
}

impl Journal for FileJournal {
    fn append(&self, entry: JournalEntry) -> Result<()> {
        // Serialize first: if serde fails, disk is not touched.
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        // Poison recovery: if the lock holder panicked, the file handle is
        // still valid (append is an atomic write_all + flush + fsync, with no
        // partial state that would require honoring the mutex poison).
        // `into_inner()` returns the handle without panicking → complies with
        // the error.rs:5 invariant.
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        file.write_all(line.as_bytes())?;
        file.flush()?;
        // fsync: guarantees the row is physically on disk before returning —
        // this is the crux of the whole crash-resistance guarantee.
        file.sync_all()?;
        Ok(())
    }

    fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>> {
        let all = self.read_all_entries()?;
        Ok(all.into_iter().filter(|e| e.step_id >= from).collect())
    }

    fn replay_all(&self) -> Result<Vec<JournalEntry>> {
        self.read_all_entries()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;
    use serde_json::json;

    /// A small RAII temp file without external crates.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let unique = format!(
                "familyclaw-durable-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            );
            p.push(unique);
            // Ensure a clean start.
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

    #[test]
    fn open_create_append_replay_roundtrip() {
        let tmp = TempPath::new("roundtrip");
        let j = FileJournal::open(tmp.path()).expect("open");
        assert!(j.is_empty().expect("empty"));

        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append a");
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append b");

        let all = j.replay_all().expect("replay");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
    }

    #[test]
    fn reopen_persists_entries() {
        let tmp = TempPath::new("persist");
        {
            let j = FileJournal::open(tmp.path()).expect("open 1");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append");
        }
        // Uusi kahva samaan tiedostoon — simuloi prosessin restartin.
        let j2 = FileJournal::open(tmp.path()).expect("open 2");
        let all = j2.replay_all().expect("replay");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].step_name(), Some("a"));
    }

    #[test]
    fn append_continues_existing_file() {
        let tmp = TempPath::new("continue");
        {
            let j = FileJournal::open(tmp.path()).expect("open 1");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append");
        }
        let j2 = FileJournal::open(tmp.path()).expect("open 2");
        j2.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append");
        assert_eq!(j2.replay_all().expect("replay").len(), 2);
    }

    #[test]
    fn replay_from_filters() {
        let tmp = TempPath::new("from");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "s", json!(i)))
                .expect("append");
        }
        let tail = j.replay_from(StepId::new(2)).expect("replay_from");
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].step_id, StepId::new(2));
    }

    #[test]
    fn tolerates_truncated_last_line_after_crash() {
        let tmp = TempPath::new("truncated");
        {
            let j = FileJournal::open(tmp.path()).expect("open");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append");
        }
        // Simulate a crash mid-write: append incomplete JSON WITHOUT a
        // terminating line break.
        {
            let mut raw = OpenOptions::new()
                .append(true)
                .open(tmp.path())
                .expect("reopen raw");
            raw.write_all(b"{\"step_id\":1,\"timestamp\":\"2026")
                .expect("write partial");
            raw.flush().expect("flush");
        }
        // Replay tolerates the incomplete last line: only the intact first row is returned.
        let j = FileJournal::open(tmp.path()).expect("reopen journal");
        let all = j.replay_all().expect("replay tolerant");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].step_name(), Some("a"));
    }

    #[test]
    fn rejects_corrupt_interior_line() {
        let tmp = TempPath::new("corrupt-interior");
        {
            // Write an intact line, then a garbage line WITH a terminating
            // line break (= internal corruption, not an incomplete write), then an intact line.
            let good = serde_json::to_string(&JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("ser");
            let good2 =
                serde_json::to_string(&JournalEntry::completed(StepId::new(1), "b", json!(2)))
                    .expect("ser");
            let mut raw = OpenOptions::new()
                .create(true)
                .append(true)
                .open(tmp.path())
                .expect("open raw");
            raw.write_all(format!("{good}\n{{garbage}}\n{good2}\n").as_bytes())
                .expect("write");
            raw.flush().expect("flush");
        }
        let j = FileJournal::open(tmp.path()).expect("open");
        let err = j.replay_all().expect_err("interior corruption must error");
        match err {
            DurableError::CorruptEntry { line, .. } => assert_eq!(line, 2),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// REGRESSION (red-team `replay_after_torn_write_leaves_journal_permanently_corrupt`):
    /// torn-write → open heals the fragment → append continues from a CLEAN
    /// line boundary → a new reopen + replay succeeds without `CorruptEntry`.
    ///
    /// Before the fix: open only tolerated the fragment during reads but left
    /// it on disk; the next append attached to the same line and produced
    /// internal corruption that broke every subsequent reopen. Now the
    /// fragment is truncated on open.
    #[test]
    fn heals_torn_trailing_fragment_so_append_does_not_corrupt() {
        let tmp = TempPath::new("heal-torn");
        // Step 1: two intact steps to disk.
        {
            let j = FileJournal::open(tmp.path()).expect("open 1");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append a");
            j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
                .expect("append b");
        }
        // Step 2: simulate a crash mid-write — a newline-less fragment.
        {
            let mut raw = OpenOptions::new()
                .append(true)
                .open(tmp.path())
                .expect("reopen raw");
            raw.write_all(b"{\"step_id\":2,\"timestamp\":\"2026")
                .expect("write partial");
            raw.flush().expect("flush");
        }

        // Step 3: open HEALS the fragment (truncation), then appends step c.
        {
            let j = FileJournal::open(tmp.path()).expect("open 2 heals");
            // Immediately after opening, the file ends with an intact line (\n).
            let after_open = std::fs::read_to_string(tmp.path()).expect("read");
            assert!(
                after_open.ends_with('\n'),
                "heal must leave file ending in newline, got:\n{after_open}"
            );
            // Healed: only two intact steps remain, the fragment is gone.
            assert_eq!(j.replay_all().expect("replay after heal").len(), 2);
            j.append(JournalEntry::completed(StepId::new(2), "c", json!(3)))
                .expect("append c onto healed boundary");
        }

        // Step 4: reopen + replay SUCCEEDS — no internal corruption.
        let j = FileJournal::open(tmp.path()).expect("open 3");
        let all = j.replay_all().expect("replay must not be CorruptEntry");
        assert_eq!(all.len(), 3, "two intact + fresh step c = 3 intact rows");
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
        assert_eq!(all[2].step_name(), Some("c"));

        // No physical line contains two step_id keys (= no fusion).
        let contents = std::fs::read_to_string(j.path()).expect("read final");
        assert!(
            !contents
                .lines()
                .any(|l| l.matches("\"step_id\"").count() >= 2),
            "no physical line may fuse two entries:\n{contents}"
        );
    }

    /// Healing must NOT truncate an intact last line that is only missing `\n`.
    /// In that case only the missing line break is appended and the step is preserved.
    #[test]
    fn heal_preserves_intact_last_line_missing_only_newline() {
        let tmp = TempPath::new("heal-intact");
        // Write one intact entry RAW, WITHOUT a terminating `\n`.
        {
            let good = serde_json::to_string(&JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("ser");
            let mut raw = OpenOptions::new()
                .create(true)
                .append(true)
                .open(tmp.path())
                .expect("open raw");
            raw.write_all(good.as_bytes()).expect("write");
            raw.flush().expect("flush");
        }
        // open: the line is intact → preserved, only `\n` is appended.
        let j = FileJournal::open(tmp.path()).expect("open heals newline");
        assert_eq!(
            j.replay_all().expect("replay").len(),
            1,
            "the intact line is preserved"
        );
        // Append continues cleanly.
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append b");
        let all = j.replay_all().expect("replay 2");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
    }

    #[test]
    fn empty_file_replays_empty() {
        let tmp = TempPath::new("empty");
        let j = FileJournal::open(tmp.path()).expect("open");
        assert!(j.replay_all().expect("replay").is_empty());
        assert_eq!(j.len().expect("len"), 0);
    }

    #[test]
    fn path_accessor_returns_open_path() {
        let tmp = TempPath::new("path");
        let j = FileJournal::open(tmp.path()).expect("open");
        assert_eq!(j.path(), tmp.path());
    }

    /// REGRESSION (error.rs:5 invariant "no unwrap/expect/panic on the production path"):
    /// when another thread panics while holding the file mutex, the lock is poisoned.
    /// `append` and `read_all_entries`/`replay_all` now use
    /// `unwrap_or_else(|e| e.into_inner())` → they RECOVER from the poison
    /// instead of panicking. The file handle remains valid, so the round-trip succeeds.
    // ---- rewrite (compaction) ----

    #[test]
    fn rewrite_replaces_log_with_given_entries() {
        let tmp = TempPath::new("rewrite-basic");
        let j = FileJournal::open(tmp.path()).expect("open");
        // Five rows initially.
        for i in 0..5 {
            j.append(JournalEntry::completed(StepId::new(i), "old", json!(i)))
                .expect("append");
        }
        assert_eq!(j.replay_all().expect("replay").len(), 5);

        // Compact to two rows.
        let kept = vec![
            JournalEntry::completed(StepId::ZERO, "keep-a", json!(1)),
            JournalEntry::completed(StepId::new(1), "keep-b", json!(2)),
        ];
        j.rewrite(&kept).expect("rewrite");

        let all = j.replay_all().expect("replay after rewrite");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("keep-a"));
        assert_eq!(all[1].step_name(), Some("keep-b"));
    }

    #[test]
    fn rewrite_result_survives_reopen() {
        let tmp = TempPath::new("rewrite-reopen");
        {
            let j = FileJournal::open(tmp.path()).expect("open 1");
            for i in 0..4 {
                j.append(JournalEntry::completed(StepId::new(i), "x", json!(i)))
                    .expect("append");
            }
            let kept = vec![JournalEntry::completed(StepId::ZERO, "only", json!(9))];
            j.rewrite(&kept).expect("rewrite");
        }
        // Restart: the compacted form persisted to disk.
        let j2 = FileJournal::open(tmp.path()).expect("open 2");
        let all = j2.replay_all().expect("replay");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].step_name(), Some("only"));
    }

    #[test]
    fn append_after_rewrite_continues_on_compacted_log() {
        let tmp = TempPath::new("rewrite-append");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "x", json!(i)))
                .expect("append");
        }
        j.rewrite(&[JournalEntry::completed(StepId::ZERO, "base", json!(0))])
            .expect("rewrite");
        // Append after compaction continues from a clean line boundary.
        j.append(JournalEntry::completed(StepId::new(1), "next", json!(1)))
            .expect("append after rewrite");
        let all = j.replay_all().expect("replay");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("base"));
        assert_eq!(all[1].step_name(), Some("next"));
        // No physical line fuses two entries.
        let contents = std::fs::read_to_string(j.path()).expect("read");
        assert!(
            !contents
                .lines()
                .any(|l| l.matches("\"step_id\"").count() >= 2),
            "no physical line may fuse two entries:\n{contents}"
        );
    }

    #[test]
    fn rewrite_to_empty_clears_log() {
        let tmp = TempPath::new("rewrite-empty");
        let j = FileJournal::open(tmp.path()).expect("open");
        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        j.rewrite(&[]).expect("rewrite empty");
        assert!(j.replay_all().expect("replay").is_empty());
        assert_eq!(std::fs::read_to_string(j.path()).expect("read").len(), 0);
    }

    #[test]
    fn rewrite_leaves_no_temp_file_behind() {
        let tmp = TempPath::new("rewrite-no-temp");
        let j = FileJournal::open(tmp.path()).expect("open");
        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        j.rewrite(&[JournalEntry::completed(StepId::ZERO, "b", json!(2))])
            .expect("rewrite");

        // THIS log's temp file must not linger in the directory (the rename moved it).
        // The scan is restricted to this test's file-name prefix so that
        // temp files from other tests running concurrently don't interfere.
        let dir = j.path().parent().expect("parent dir");
        let own_name = j
            .path()
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        let leftover: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&own_name) && n.contains(".compact-") && n.contains(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "temp files left behind: {leftover:?}");
    }

    /// Atomicity: simulates "interrupted before the rename" by leaving the
    /// live file untouched and demonstrating that no live rows are lost.
    /// Since a real crash cannot be triggered deterministically, this test
    /// verifies the invariant: the live file is always intact under a
    /// rename-based swap — before the rename its contents are the old whole,
    /// never a half-written mix.
    #[test]
    fn rewrite_is_atomic_live_file_never_half_written() {
        let tmp = TempPath::new("rewrite-atomic");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "live", json!(i)))
                .expect("append");
        }
        let before = std::fs::read_to_string(j.path()).expect("read before");

        // Compact — atomic rename. Immediately after success, the file is
        // either FULLY old or FULLY new, never a mix.
        let kept = vec![JournalEntry::completed(
            StepId::ZERO,
            "compacted",
            json!(42),
        )];
        j.rewrite(&kept).expect("rewrite");
        let after = std::fs::read_to_string(j.path()).expect("read after");

        // Every line is intact JSON (no partial fusion from the rename).
        for line in after.lines().filter(|l| !l.trim().is_empty()) {
            serde_json::from_str::<JournalEntry>(line)
                .expect("every line after rewrite must be intact json");
        }
        assert_ne!(before, after, "rewrite must have changed contents");
        assert_eq!(j.replay_all().expect("replay").len(), 1);
    }

    // ---- compact_with (atomic compaction that closes the TOCTOU gap) ----

    #[test]
    fn compact_with_filters_and_renumbers_under_single_lock() {
        let tmp = TempPath::new("compact-with-basic");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..5 {
            j.append(JournalEntry::completed(StepId::new(i), "old", json!(i)))
                .expect("append");
        }

        // Compact: keep only even-numbered steps, renumber them 0..N.
        let dropped = j
            .compact_with(|entries| {
                assert_eq!(entries.len(), 5, "build sees all on-disk rows");
                let mut kept = Vec::new();
                let mut step = StepId::ZERO;
                for e in entries.into_iter().filter(|e| {
                    matches!(&e.kind, EntryKind::StepCompleted { output, .. } if output.as_u64().is_some_and(|n| n % 2 == 0))
                }) {
                    kept.push(JournalEntry::completed(
                        step,
                        e.step_name().unwrap_or("kept").to_string(),
                        match &e.kind {
                            EntryKind::StepCompleted { output, .. } => output.clone(),
                            _ => json!(null),
                        },
                    ));
                    step = step.next();
                }
                Ok(kept)
            })
            .expect("compact_with");

        // 5 read − 3 kept (0,2,4) = 2 dropped.
        assert_eq!(dropped, 2);
        let all = j.replay_all().expect("replay");
        assert_eq!(all.len(), 3);
        // Renumbered to a tight 0..N.
        assert_eq!(all[0].step_id, StepId::new(0));
        assert_eq!(all[1].step_id, StepId::new(1));
        assert_eq!(all[2].step_id, StepId::new(2));
    }

    /// PROOF that the TOCTOU gap is closed (regression: append-during-compact does not disappear).
    ///
    /// Atomicity follows from holding a single lock: `compact_with` locks
    /// `self.file` once and holds it for the read→filter→swap operation. This
    /// test demonstrates two observable consequences:
    /// 1. The `build` closure sees **exactly** the rows that were on disk at
    ///    lock-acquire time (no more, no less) — the read is consistent.
    /// 2. An append performed AFTER `compact_with` RETURNS lands AFTER the
    ///    compacted rows — not in the old file, which would be destroyed by
    ///    the swap.
    ///
    /// Because the lock is held throughout, a concurrent append can only
    /// complete EITHER before the lock is acquired (in which case it shows up
    /// in `build`'s rows) OR after the swap (in which case it lands after the
    /// compacted log) — never disappearing in between. Real concurrency isn't
    /// needed here: the invariant follows from the structure, and a
    /// deterministic test is more reliable than a race.
    #[test]
    fn compact_with_holds_lock_so_post_compact_append_lands_after_compacted_rows() {
        let tmp = TempPath::new("compact-with-toctou");
        let j = FileJournal::open(tmp.path()).expect("open");
        // Three rows to disk BEFORE compaction.
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "pre", json!(i)))
                .expect("append pre");
        }

        // Compact: build sees EXACTLY those three rows (a consistent read under the lock).
        let dropped = j
            .compact_with(|entries| {
                assert_eq!(
                    entries.len(),
                    3,
                    "build must observe exactly the rows on disk at lock-acquire"
                );
                for (i, e) in entries.iter().enumerate() {
                    assert_eq!(e.step_name(), Some("pre"));
                    assert_eq!(e.step_id, StepId::new(i as u64));
                }
                // Keep only one (renumbered) live row.
                Ok(vec![JournalEntry::completed(
                    StepId::ZERO,
                    "compacted",
                    json!(99),
                )])
            })
            .expect("compact_with");
        assert_eq!(dropped, 2, "3 read − 1 kept = 2 dropped");

        // An append AFTER RETURN lands AFTER the compacted rows.
        j.append(JournalEntry::completed(StepId::new(1), "post", json!(100)))
            .expect("append post must land after compacted rows");

        let all = j.replay_all().expect("replay");
        assert_eq!(all.len(), 2, "1 compacted + 1 post-append = 2");
        assert_eq!(all[0].step_name(), Some("compacted"));
        assert_eq!(all[1].step_name(), Some("post"));

        // No physical line fuses two entries (the swap left clean boundaries).
        let contents = std::fs::read_to_string(j.path()).expect("read");
        assert!(
            !contents
                .lines()
                .any(|l| l.matches("\"step_id\"").count() >= 2),
            "no physical line may fuse two entries:\n{contents}"
        );
    }

    /// NO-DEADLOCK: `compact_with` locks `self.file` only once (no reentrant
    /// re-locking), so the call completes, and an append + replay performed
    /// immediately after succeed on the same thread. If any internal path
    /// were to lock `self.file` again, this test would hang (timeout) or panic.
    #[test]
    fn compact_with_does_not_deadlock_then_append_then_replay() {
        let tmp = TempPath::new("compact-with-no-deadlock");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..4 {
            j.append(JournalEntry::completed(StepId::new(i), "x", json!(i)))
                .expect("append");
        }

        // compact_with completes (no deadlock on the same thread).
        j.compact_with(|entries| {
            // Filter out odd outputs → keep 0 and 2.
            let mut kept = Vec::new();
            let mut step = StepId::ZERO;
            for e in entries {
                let keep = matches!(&e.kind, EntryKind::StepCompleted { output, .. } if output.as_u64().is_some_and(|n| n % 2 == 0));
                if keep {
                    let out = match &e.kind {
                        EntryKind::StepCompleted { output, .. } => output.clone(),
                        _ => json!(null),
                    };
                    kept.push(JournalEntry::completed(step, "live", out));
                    step = step.next();
                }
            }
            Ok(kept)
        })
        .expect("compact_with completes without deadlock");

        // Append IMMEDIATELY after (locks self.file again — succeeds because
        // compact_with released its lock upon returning).
        j.append(JournalEntry::completed(StepId::new(2), "after", json!(7)))
            .expect("append after compact_with");

        // And replay (locks self.file once more) — succeeds.
        let all = j.replay_all().expect("replay after compact_with");
        assert_eq!(all.len(), 3, "2 live (0,2) + 1 after-append");
        assert_eq!(all[2].step_name(), Some("after"));
    }

    #[test]
    fn compact_with_propagates_build_error_and_leaves_live_file_intact() {
        let tmp = TempPath::new("compact-with-build-err");
        let j = FileJournal::open(tmp.path()).expect("open");
        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append");

        // build returns an error → compact_with returns it as-is, and the
        // live file is not modified (no swap is performed).
        let err = j
            .compact_with(|_entries| {
                Err(DurableError::step_failed(
                    "build",
                    "intentional build failure",
                ))
            })
            .expect_err("build error must propagate");
        match err {
            DurableError::StepFailed { step, .. } => assert_eq!(step, "build"),
            other => panic!("unexpected error: {other:?}"),
        }

        // Live file untouched: both original rows are still readable.
        let all = j.replay_all().expect("replay after failed compact");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
    }

    #[test]
    fn compact_with_on_empty_log_is_noop() {
        let tmp = TempPath::new("compact-with-empty");
        let j = FileJournal::open(tmp.path()).expect("open");
        let dropped = j
            .compact_with(|entries| {
                assert!(entries.is_empty(), "empty log yields no rows");
                Ok(entries)
            })
            .expect("compact_with");
        assert_eq!(dropped, 0);
        assert!(j.replay_all().expect("replay").is_empty());
        assert_eq!(std::fs::read_to_string(j.path()).expect("read").len(), 0);
    }

    #[test]
    fn append_and_replay_recover_from_poisoned_mutex() {
        use std::sync::Arc;

        let tmp = TempPath::new("poison-recovery");
        let j = Arc::new(FileJournal::open(tmp.path()).expect("open"));
        // One intact step before poisoning.
        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append a");

        // Poison the mutex: panic in another thread while holding the lock.
        let poisoner = Arc::clone(&j);
        let handle = std::thread::spawn(move || {
            let _guard = poisoner.file.lock().expect("acquire lock to poison");
            panic!("intentional panic to poison the file mutex");
        });
        assert!(
            handle.join().is_err(),
            "poisoning thread must have panicked"
        );

        // append RECOVERS from the poison — no panic, returns Ok.
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append must recover from poisoned mutex");

        // replay_all (→ read_all_entries) also RECOVERS and sees both steps.
        let all = j
            .replay_all()
            .expect("replay must recover from poisoned mutex");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
    }
}
