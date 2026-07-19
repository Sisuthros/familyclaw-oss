//! Memory storage: the [`MemoryStore`] trait and [`LocalJsonStore`].
//!
//! [`MemoryStore`] is Eternal Thread's storage abstraction: add, get,
//! update lifecycle state, run retrieval, and run a decay pass.
//! The default implementation [`LocalJsonStore`] keeps memories in memory
//! and persists them to a JSON file with an atomic write (tmp + rename).
//!
//! ## Future: `Surreal<Any>` behind a feature flag
//! The design (§2.3, §5) selects `SurrealDB` as the production storage
//! (`Surreal<Any>`: in-mem dev / `RocksDB` prod). It will be added later as
//! its own implementation behind the `surreal` feature flag — the same
//! [`MemoryStore`] interface, a different backend. [`LocalJsonStore`] remains
//! a lightweight, dependency-free default (Layer A works without a native
//! database).
//!
//! The trait uses native `async fn` syntax (Rust >= 1.75). `Send` bounds
//! are verified in tests, so implementations work in
//! a multithreaded tokio runtime.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use familyclaw_core::{FamilyClawError, MessageId, Result, Timestamp};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::memory::{Memory, MemoryStatus};
use crate::retrieval::{retrieve, RetrievalContext, RetrievalResult};

/// Summary of a decay pass ([`MemoryStore::run_decay`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DecayReport {
    /// Number of memories moved from active to archived.
    pub archived: usize,
    /// Number of memories tombstoned from archived.
    pub tombstoned: usize,
    /// Total number of memories scanned.
    pub scanned: usize,
}

/// Thresholds for a decay pass.
///
/// Retention drops over time; when it falls below the threshold, the
/// memory moves to the next lifecycle stage. A protected core
/// ([`crate::DecayPolicy::ProtectedCore`]) is never moved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecayThresholds {
    /// Below this retention, an active memory is archived.
    pub archive_below: f32,
    /// Below this retention, an archived memory is tombstoned.
    pub tombstone_below: f32,
}

impl DecayThresholds {
    /// Builds the thresholds, clamping both to `0.0..=1.0` and
    /// ensuring `tombstone_below <= archive_below`.
    #[must_use]
    pub fn new(archive_below: f32, tombstone_below: f32) -> Self {
        let archive = clamp_unit(archive_below, 0.4);
        let tombstone = clamp_unit(tombstone_below, 0.1).min(archive);
        Self {
            archive_below: archive,
            tombstone_below: tombstone,
        }
    }
}

impl Default for DecayThresholds {
    /// Default: archive below `0.4`, tombstone below `0.1`.
    fn default() -> Self {
        Self::new(0.4, 0.1)
    }
}

/// Clamps a value to `0.0..=1.0`; invalid → `fallback`.
fn clamp_unit(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

/// Type-erased future for dyn-compatible trait.
/// Lifetime `'a` captures the borrow of `&self` so returned futures can reference `self`.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Memory storage abstraction.
///
/// Implementations are responsible for persistence and concurrency. All
/// methods are asynchronous, so database-backed backends
/// (`Surreal<Any>`) fit the same interface.
pub trait MemoryStore: Send + Sync {
    /// Adds a memory to storage and returns its identifier.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the write fails.
    fn add(&self, memory: Memory) -> BoxFuture<'_, Result<MessageId>>;

    /// Retrieves a memory by identifier, or `None` if not found.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the read fails.
    fn get(&self, id: MessageId) -> BoxFuture<'_, Result<Option<Memory>>>;

    /// Replaces an existing memory (same `id`).
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] if the identifier doesn't exist, or
    /// [`FamilyClawError::Memory`] on a storage error.
    fn update(&self, memory: Memory) -> BoxFuture<'_, Result<()>>;

    /// Reinforces a memory (raises retention + importance) at time `at`.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] if the identifier doesn't exist.
    fn reinforce(&self, id: MessageId, at: Timestamp) -> BoxFuture<'_, Result<()>>;

    /// Sets a memory's lifecycle state directly.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] if the identifier doesn't exist.
    fn set_status(&self, id: MessageId, status: MemoryStatus) -> BoxFuture<'_, Result<()>>;

    /// Returns all memories (including archived/tombstoned).
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the read fails.
    fn all(&self) -> BoxFuture<'_, Result<Vec<Memory>>>;

    /// Total number of memories.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the read fails.
    fn len(&self) -> BoxFuture<'_, Result<usize>>;

    /// Whether storage is empty.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the read fails.
    fn is_empty(&self) -> BoxFuture<'_, Result<bool>>;

    /// Runs retrieval with the given context at time `at`.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the read fails.
    fn retrieve(
        &self,
        ctx: &RetrievalContext,
        at: Timestamp,
    ) -> BoxFuture<'_, Result<Vec<RetrievalResult>>>;

    /// Runs a decay pass at time `at`: moves memories that have fallen
    /// below the threshold to archived and tombstoned. A protected core is
    /// never moved.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the write fails.
    fn run_decay(
        &self,
        thresholds: DecayThresholds,
        at: Timestamp,
    ) -> BoxFuture<'_, Result<DecayReport>>;
}

/// Memory storage persisted to a JSON file.
///
/// Keeps memories in memory protected by an [`RwLock`] and writes them to
/// disk atomically (tmp file + `rename`) after every mutation. This is
/// Layer A's dependency-free default implementation — it requires no
/// native database or C/C++ toolchain (cf. design §5: a constrained
/// target machine may not have a `RocksDB` toolchain).
#[derive(Debug)]
pub struct LocalJsonStore {
    /// The file path, or `None` if purely in-memory.
    path: Option<PathBuf>,
    /// Memories by identifier.
    memories: RwLock<HashMap<MessageId, Memory>>,
}

/// On-disk format of the JSON file (versioned for forward compatibility).
#[derive(Debug, Serialize, Deserialize)]
struct DiskFormat {
    /// File format version.
    version: u32,
    /// Stored memories.
    memories: Vec<Memory>,
}

impl DiskFormat {
    const CURRENT_VERSION: u32 = 1;
}

impl LocalJsonStore {
    /// Creates a purely in-memory storage (no disk persistence).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            memories: RwLock::new(HashMap::new()),
        }
    }

    /// Opens (or creates) JSON storage at the given path.
    ///
    /// If the file exists, its memories are loaded. If not, storage
    /// starts empty and the file is created on the first write.
    ///
    /// # Errors
    /// [`FamilyClawError::Io`] if an existing file cannot be read,
    /// or [`FamilyClawError::Serde`] if its content is invalid JSON.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let memories = if path.exists() {
            let contents = tokio::fs::read_to_string(&path).await?;
            let disk: DiskFormat = serde_json::from_str(&contents)?;
            disk.memories.into_iter().map(|m| (m.id, m)).collect()
        } else {
            HashMap::new()
        };
        Ok(Self {
            path: Some(path),
            memories: RwLock::new(memories),
        })
    }

    /// Persists the current state to disk atomically, if a path is set.
    ///
    /// The caller holds the lock; this takes a snapshot of the given map.
    async fn persist(&self, map: &HashMap<MessageId, Memory>) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut memories: Vec<Memory> = map.values().cloned().collect();
        // Stable order for a diffable, deterministic file.
        memories.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let disk = DiskFormat {
            version: DiskFormat::CURRENT_VERSION,
            memories,
        };
        let json = serde_json::to_string_pretty(&disk)?;

        // Atomic write: write to a tmp file, then rename.
        let tmp = tmp_path(path);
        tokio::fs::write(&tmp, json.as_bytes()).await?;
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    /// Reads the entire disk state into a map. A missing file → empty map.
    ///
    /// This is the READ phase of the read-modify-write cycle: before every
    /// mutation, the latest state is read from disk so that another
    /// handle's writes are not lost (lost update). Called only while
    /// holding the lock.
    async fn read_disk(path: &Path) -> Result<HashMap<MessageId, Memory>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let contents = tokio::fs::read_to_string(path).await?;
        if contents.trim().is_empty() {
            return Ok(HashMap::new());
        }
        let disk: DiskFormat = serde_json::from_str(&contents)?;
        Ok(disk.memories.into_iter().map(|m| (m.id, m)).collect())
    }

    /// Runs a mutation under an inter-process lock with read-modify-write
    /// semantics. Resolves the `concurrent-writers` gap: two separate
    /// handles (or processes) pointed at the same path no longer clobber
    /// each other.
    ///
    /// Steps (in file-backed mode):
    /// 1. acquire the exclusive lock (`<path>.lock`),
    /// 2. load the latest state from disk into the in-memory map (see other writes),
    /// 3. run `mutate`, which modifies the map and produces a result,
    /// 4. persist the entire map atomically (tmp + rename),
    /// 5. release the lock (Drop).
    ///
    /// In in-memory mode (`path == None`), no lock/load is needed: the
    /// single [`RwLock`]-protected map is already the sole source of truth.
    ///
    /// # Errors
    /// Propagates the `mutate` error or an IO/serde error from the
    /// load/persist phase. The lock is released even on error (RAII).
    async fn with_write_lock<T, F>(&self, mutate: F) -> Result<T>
    where
        F: FnOnce(&mut HashMap<MessageId, Memory>) -> Result<T>,
    {
        let Some(path) = self.path.clone() else {
            // In-memory: no disk to coordinate.
            let mut guard = self.memories.write().await;
            return mutate(&mut guard);
        };

        // 1. exclusive lock for the duration of the mutation.
        let _lock = FileLock::acquire(&path).await?;

        // 2. load the latest disk state (including other handles' writes).
        let disk = Self::read_disk(&path).await?;
        let mut guard = self.memories.write().await;
        *guard = disk;

        // 3. run the mutation on top of the fresh state.
        let out = mutate(&mut guard)?;

        // 4. persist the entire map atomically.
        self.persist(&guard).await?;

        // 5. the lock is released here (_lock Drop).
        Ok(out)
    }

    /// Returns the storage file path (or `None` if in-memory).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// Derives the temporary file path (`<path>.tmp`).
fn tmp_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".tmp");
    PathBuf::from(os)
}

/// Derives the lock file path (`<path>.lock`).
fn lock_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".lock");
    PathBuf::from(os)
}

/// An inter-process exclusive lock implemented as a `create_new` lock file.
///
/// A classic lockfile mutex: acquiring the lock succeeds only if the lock
/// file does NOT already exist (`OpenOptions::create_new` fails with
/// [`io::ErrorKind::AlreadyExists`] if another handle/process holds the
/// lock). The lock is released in [`Drop`] by deleting the file. This
/// coordinates *separate* [`LocalJsonStore`] handles pointed at the same
/// path — including across processes — without `unsafe` FFI (workspace lint
/// `unsafe_code = forbid`).
///
/// Acquiring the lock is synchronous and fast (file creation), so it is
/// performed in a blocking manner with a short delay between retries. A
/// dead lock (e.g. a crashed process that didn't get to delete the file)
/// is broken after a staleness window: a lock file that is too old is
/// taken over. This prevents a permanent deadlock after a crash.
struct FileLock {
    lock_path: PathBuf,
}

impl FileLock {
    /// Lock staleness duration: if the lock file is older than this, its
    /// holder is assumed to have crashed and the lock is taken over
    /// (steal). Mutations take under a millisecond, so 30s is a comfortably
    /// safe upper bound.
    const STALE_AFTER: Duration = Duration::from_secs(30);

    /// Retry delay while waiting for another holder to release the lock.
    const RETRY_DELAY: Duration = Duration::from_millis(2);

    /// Acquires the exclusive lock for `<data_path>`. Blocks until the lock
    /// is free (or stale). Run in a blocking tokio task so the async
    /// executor is not stalled.
    ///
    /// # Errors
    /// [`FamilyClawError::Io`] if creating the lock file fails for a reason
    /// other than "already exists".
    async fn acquire(data_path: &Path) -> Result<Self> {
        let lock_path = lock_path(data_path);
        let probe = lock_path.clone();
        // Acquiring the lock is synchronous file I/O → spawn_blocking,
        // so it doesn't block the async executor.
        tokio::task::spawn_blocking(move || Self::acquire_blocking(&probe))
            .await
            .map_err(|e| FamilyClawError::memory(format!("lock task join failed: {e}")))?
            .map(|()| Self { lock_path })
    }

    /// Synchronous locking logic (spin + steal-on-stale).
    fn acquire_blocking(lock_path: &Path) -> Result<()> {
        use std::io::ErrorKind;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(lock_path)
            {
                Ok(_file) => return Ok(()),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    // Someone else holds the lock. Check staleness: if the
                    // lock file is too old, its holder has likely crashed →
                    // remove it and retry. Otherwise wait briefly.
                    if Self::is_stale(lock_path) {
                        // Best-effort steal: ignore the error (another
                        // thread may have taken it at the same time) and retry.
                        let _ = std::fs::remove_file(lock_path);
                    }
                    std::thread::sleep(Self::RETRY_DELAY);
                }
                Err(e) => return Err(FamilyClawError::Io(e)),
            }
        }
    }

    /// Is the lock file stale (its holder presumably crashed)?
    fn is_stale(lock_path: &Path) -> bool {
        std::fs::metadata(lock_path)
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > Self::STALE_AFTER))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Release the lock by deleting the file. Best-effort: if deletion
        // fails, the staleness window will break the lock later.
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

impl MemoryStore for LocalJsonStore {
    fn add(&self, memory: Memory) -> BoxFuture<'_, Result<MessageId>> {
        let id = memory.id;
        // The entire read-modify-write runs under the inter-process lock:
        // the latest state is loaded from disk BEFORE inserting, so a
        // concurrent write from another handle is not lost (concurrent-writers fix).
        let this = self;
        Box::pin(async move {
            this.with_write_lock(move |map| {
                // Idempotent write: if a memory with the same turn_key
                // already exists (in the freshly loaded state), skip it
                // (dual-write protection: durable.step may succeed even if
                // memory_store.add doesn't complete before a crash).
                if let Some(ref key) = memory.turn_key {
                    let exists = map.values().any(|m| m.turn_key.as_ref() == Some(key));
                    if exists {
                        return Ok(id);
                    }
                }
                map.insert(id, memory);

                // ── verification-gated: write-verify ────────────────────────────
                // Confirm the memory is actually in the map. In LocalJsonStore
                // this is a defensive check (HashMap.insert fails
                // only on out-of-memory); critical in a SurrealDB implementation.
                if !map.contains_key(&id) {
                    return Err(FamilyClawError::Memory(
                        "write-verify failed: memory not found after insert".into(),
                    ));
                }
                Ok(id)
            })
            .await
        })
    }

    fn get(&self, id: MessageId) -> BoxFuture<'_, Result<Option<Memory>>> {
        let this = self;
        Box::pin(async move {
            let guard = this.memories.read().await;
            Ok(guard.get(&id).cloned())
        })
    }

    fn update(&self, memory: Memory) -> BoxFuture<'_, Result<()>> {
        let this = self;
        Box::pin(async move {
            this.with_write_lock(move |map| {
                if !map.contains_key(&memory.id) {
                    return Err(FamilyClawError::not_found(format!(
                        "memory {} not found",
                        memory.id
                    )));
                }
                map.insert(memory.id, memory);
                Ok(())
            })
            .await
        })
    }

    fn reinforce(&self, id: MessageId, at: Timestamp) -> BoxFuture<'_, Result<()>> {
        let this = self;
        Box::pin(async move {
            this.with_write_lock(move |map| {
                let memory = map
                    .get_mut(&id)
                    .ok_or_else(|| FamilyClawError::not_found(format!("memory {id} not found")))?;
                memory.reinforce(at);
                Ok(())
            })
            .await
        })
    }

    fn set_status(&self, id: MessageId, status: MemoryStatus) -> BoxFuture<'_, Result<()>> {
        let this = self;
        Box::pin(async move {
            this.with_write_lock(move |map| {
                let memory = map
                    .get_mut(&id)
                    .ok_or_else(|| FamilyClawError::not_found(format!("memory {id} not found")))?;
                memory.status = status;
                Ok(())
            })
            .await
        })
    }

    fn all(&self) -> BoxFuture<'_, Result<Vec<Memory>>> {
        let this = self;
        Box::pin(async move {
            let guard = this.memories.read().await;
            Ok(guard.values().cloned().collect())
        })
    }

    fn len(&self) -> BoxFuture<'_, Result<usize>> {
        let this = self;
        Box::pin(async move {
            let guard = this.memories.read().await;
            Ok(guard.len())
        })
    }

    fn is_empty(&self) -> BoxFuture<'_, Result<bool>> {
        let this = self;
        Box::pin(async move {
            let guard = this.memories.read().await;
            Ok(guard.is_empty())
        })
    }

    fn retrieve(
        &self,
        ctx: &RetrievalContext,
        at: Timestamp,
    ) -> BoxFuture<'_, Result<Vec<RetrievalResult>>> {
        let this = self;
        let ctx = ctx.clone();
        Box::pin(async move {
            let guard = this.memories.read().await;
            Ok(retrieve(guard.values(), &ctx, at))
        })
    }

    fn run_decay(
        &self,
        thresholds: DecayThresholds,
        at: Timestamp,
    ) -> BoxFuture<'_, Result<DecayReport>> {
        let this = self;
        Box::pin(async move {
            this.with_write_lock(move |map| {
                let mut report = DecayReport::default();
                for memory in map.values_mut() {
                    report.scanned += 1;
                    // A protected core is skipped entirely.
                    if memory.decay_policy.is_protected() {
                        continue;
                    }
                    let retention = memory.retention(at);
                    match memory.status {
                        MemoryStatus::Active => {
                            if retention < thresholds.archive_below {
                                memory.status = MemoryStatus::Archived;
                                report.archived += 1;
                            }
                        }
                        MemoryStatus::Archived => {
                            if retention < thresholds.tombstone_below {
                                memory.status = MemoryStatus::Tombstoned;
                                report.tombstoned += 1;
                            }
                        }
                        MemoryStatus::Tombstoned => {}
                    }
                }
                Ok(report)
            })
            .await
        })
    }
}

/// Vector-tier extension for [`MemoryStore`] implementations (Phase 3b).
///
/// This trait separates vector storage and nearest-neighbor search from the
/// [`MemoryStore`] base interface, so a semantic tier can be implemented
/// (e.g. embedded LanceDB) in a separate crate without changing the
/// `MemoryStore` foundation. Used only when the `vector-store` feature is
/// enabled; the default build is byte-identical with and without this extension.
///
/// Vectors are the contents of the [`crate::memory::Memory::embedding`] field, and
/// search results feed the [`crate::retrieval::RetrievalContext`] cosine path.
#[cfg(feature = "vector-store")]
pub trait VectorStore: Send + Sync {
    /// Stores or updates the embedding vector for the given memory id.
    ///
    /// # Errors
    /// Returns an error if writing the vector fails.
    fn upsert_vector(&self, id: MessageId, embedding: Vec<f32>) -> BoxFuture<'_, Result<()>>;

    /// Returns the `k` nearest `(id, cosine-score)` pairs for the query
    /// vector, ordered from highest to lowest similarity.
    ///
    /// # Errors
    /// Returns an error if the vector search fails.
    fn nearest(&self, query: &[f32], k: usize) -> BoxFuture<'_, Result<Vec<(MessageId, f32)>>>;
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::decay::DecayPolicy;
    use crate::importance::ImportanceFactors;
    use chrono::Duration;
    use familyclaw_core::time;
    use familyclaw_emotion::Dimension;

    fn mem(content: &str) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build()
    }

    #[test]
    fn store_is_send() {
        // Verifies that LocalJsonStore can move between threads.
        fn assert_send<T: Send>() {}
        assert_send::<LocalJsonStore>();
    }

    #[tokio::test]
    async fn add_get_and_len() {
        let store = LocalJsonStore::in_memory();
        assert!(store.is_empty().await.expect("empty check"));
        let m = mem("first");
        let id = store.add(m.clone()).await.expect("add");
        assert_eq!(store.len().await.expect("len"), 1);
        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.content, "first");
        assert!(store
            .get(MessageId::new())
            .await
            .expect("get missing")
            .is_none());
    }

    #[tokio::test]
    async fn update_existing_and_missing() {
        let store = LocalJsonStore::in_memory();
        let mut m = mem("original");
        let id = store.add(m.clone()).await.expect("add");
        m.content = "edited".into();
        store.update(m.clone()).await.expect("update");
        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.content, "edited");

        // Unknown id → NotFound.
        let ghost = mem("ghost");
        let err = store.update(ghost).await.expect_err("update missing fails");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn reinforce_updates_memory() {
        let store = LocalJsonStore::in_memory();
        let m = mem("reinforce me");
        let id = store.add(m).await.expect("add");
        let before = store.get(id).await.expect("g").expect("p").importance;
        store.reinforce(id, time::now()).await.expect("reinforce");
        let after = store.get(id).await.expect("g").expect("p");
        assert_eq!(after.reinforcement_count, 1);
        assert!(after.importance > before);

        let err = store
            .reinforce(MessageId::new(), time::now())
            .await
            .expect_err("missing");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn set_status_transitions() {
        let store = LocalJsonStore::in_memory();
        let id = store.add(mem("x")).await.expect("add");
        store
            .set_status(id, MemoryStatus::Archived)
            .await
            .expect("set status");
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Archived
        );
    }

    #[tokio::test]
    async fn retrieve_through_store() {
        let store = LocalJsonStore::in_memory();
        store.add(mem("rust memory engine")).await.expect("a1");
        store.add(mem("python web framework")).await.expect("a2");
        let ctx = RetrievalContext::new("rust memory");
        let results = store.retrieve(&ctx, time::now()).await.expect("retrieve");
        assert!(!results.is_empty());
        assert!(results[0].memory.content.contains("rust"));
    }

    #[tokio::test]
    async fn run_decay_archives_then_tombstones() {
        let store = LocalJsonStore::in_memory();
        let created = time::now();
        // Fast decaying, low importance.
        let m = Memory::builder("ephemeral")
            .factors(ImportanceFactors::new(0.05, 0.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::Fast)
            .created_at(created)
            .build();
        let id = store.add(m).await.expect("add");

        // After a long time, retention is very low → archived.
        let later = created + Duration::days(30);
        let r1 = store
            .run_decay(DecayThresholds::default(), later)
            .await
            .expect("decay 1");
        assert_eq!(r1.scanned, 1);
        assert_eq!(r1.archived, 1);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Archived
        );

        // A second pass even later → tombstoned.
        let much_later = created + Duration::days(120);
        let r2 = store
            .run_decay(DecayThresholds::default(), much_later)
            .await
            .expect("decay 2");
        assert_eq!(r2.tombstoned, 1);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Tombstoned
        );
    }

    #[tokio::test]
    async fn run_decay_never_touches_protected_core() {
        let store = LocalJsonStore::in_memory();
        let created = time::now();
        let m = Memory::builder("identity anchor")
            .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::ProtectedCore)
            .created_at(created)
            .build();
        let id = store.add(m).await.expect("add");
        let far = created + Duration::days(100_000);
        let report = store
            .run_decay(DecayThresholds::default(), far)
            .await
            .expect("decay");
        assert_eq!(report.archived, 0);
        assert_eq!(report.tombstoned, 0);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[tokio::test]
    async fn json_persistence_roundtrip() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-memory-store-{}.json",
            uuid::Uuid::new_v4()
        ));

        let id = {
            let store = LocalJsonStore::open(&path).await.expect("open new");
            assert!(store.path().is_some());
            let m = Memory::builder("persisted")
                .emotions([Dimension::Gratitude])
                .factors(ImportanceFactors::new(0.7, 0.3, 0.0, 0.0))
                .source("test")
                .build();
            store.add(m).await.expect("add")
        };

        // Reopen → data was preserved.
        let reopened = LocalJsonStore::open(&path).await.expect("reopen");
        assert_eq!(reopened.len().await.expect("len"), 1);
        let got = reopened.get(id).await.expect("g").expect("p");
        assert_eq!(got.content, "persisted");
        assert_eq!(got.emotions, vec![Dimension::Gratitude]);
        assert_eq!(got.source, "test");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn open_missing_file_starts_empty() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-memory-absent-{}.json",
            uuid::Uuid::new_v4()
        ));
        // Ensure it doesn't exist.
        let _ = std::fs::remove_file(&path);
        let store = LocalJsonStore::open(&path).await.expect("open");
        assert!(store.is_empty().await.expect("empty"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn open_corrupt_file_errors() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-memory-corrupt-{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "{ not valid json").expect("write garbage");
        let err = LocalJsonStore::open(&path)
            .await
            .expect_err("corrupt errors");
        assert!(matches!(err, FamilyClawError::Serde(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decay_thresholds_clamp_and_order() {
        let t = DecayThresholds::new(2.0, -1.0);
        assert_eq!(t.archive_below, 1.0);
        assert_eq!(t.tombstone_below, 0.0);
        // tombstone cannot exceed archive.
        let t2 = DecayThresholds::new(0.3, 0.9);
        assert!(t2.tombstone_below <= t2.archive_below);
        assert_eq!(t2.tombstone_below, 0.3);
        // Invalid → fallback.
        let t3 = DecayThresholds::new(f32::NAN, f32::NAN);
        assert_eq!(t3.archive_below, 0.4);
        assert_eq!(t3.tombstone_below, 0.1);
    }

    /// REGRESSION — red-team `concurrent-writers`: two separate
    /// `LocalJsonStore` handles pointed at the same path must not clobber
    /// each other's writes. This is a deterministic "smoking gun": A writes
    /// first, then B — before the fix, the snapshot B persisted from its
    /// empty starting state wiped A's data from disk (lost update). After
    /// the fix both are preserved, because the mutation runs under the
    /// inter-process lock with read-modify-write semantics.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_handles_same_path_no_lost_update() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-memory-cw-regression-{}.json",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_file(&path);

        let store_a = LocalJsonStore::open(&path).await.expect("open a");
        let store_b = LocalJsonStore::open(&path).await.expect("open b");

        let mem_a = Memory::builder("writer A — must not be lost")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-a")
            .build();
        let mem_b = Memory::builder("writer B — must not be lost")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-b")
            .build();
        let id_a = mem_a.id;
        let id_b = mem_b.id;

        // Deterministic sequence: A first, then B (the same pattern as the
        // red-team attack's "smoking gun" variant).
        store_a.add(mem_a).await.expect("add a");
        store_b.add(mem_b).await.expect("add b");

        // Reopen from disk with a fresh handle: both must be present.
        let reopened = LocalJsonStore::open(&path).await.expect("reopen");
        let all = reopened.all().await.expect("all");
        assert_eq!(all.len(), 2, "LOST UPDATE regression: disk must hold both");
        assert!(all.iter().any(|m| m.id == id_a), "writer A vanished");
        assert!(all.iter().any(|m| m.id == id_b), "writer B vanished");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(tmp_path(&path));
        let _ = std::fs::remove_file(lock_path(&path));
    }

    /// REGRESSION — the lock is released (Drop) after a successful mutation,
    /// so consecutive mutations do not get stuck on their own lock. If the
    /// lock were not released, a subsequent `add()` would deadlock (here:
    /// the staleness window would be too long → the test would hang). The
    /// sequence succeeds quickly ⇒ the lock cycles correctly.
    #[tokio::test]
    async fn sequential_mutations_release_lock() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-memory-cw-seq-{}.json",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_file(&path);

        let store = LocalJsonStore::open(&path).await.expect("open");
        for i in 0..5 {
            let m = Memory::builder(format!("event {i}"))
                .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
                .build();
            store
                .add(m)
                .await
                .expect("add must not block on stale lock");
        }
        assert_eq!(store.len().await.expect("len"), 5);
        // The lock file must not remain after a successful sequence.
        assert!(
            !lock_path(&path).exists(),
            "lock file leaked after mutations"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(tmp_path(&path));
        let _ = std::fs::remove_file(lock_path(&path));
    }

    #[test]
    fn decay_report_serde() {
        let r = DecayReport {
            archived: 2,
            tombstoned: 1,
            scanned: 5,
        };
        let json = serde_json::to_string(&r).expect("ser");
        let back: DecayReport = serde_json::from_str(&json).expect("de");
        assert_eq!(r, back);
    }
}
