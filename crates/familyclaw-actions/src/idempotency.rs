//! Idempotency store for tool calls.
//!
//! Design source: internal research design document.
//!
//! Semantics:
//! - `idempotency_key` ::= `<scope>:<stable-id>` (scope lowercased, id case-sensitive)
//! - Keys stored under `<data_dir>/idempotency/<tool>/<2-char-hash>/<full-hash>.json`
//! - States: `in_flight` (lock), `completed` (cached), `failed` (maybe cached)
//! - Default TTL: 7 days; lazy cleanup on lookup.
//! - Replay of a completed key returns the cached response without re-execution.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ActionError, Result};

/// Default retention for idempotency records.
pub const DEFAULT_TTL_SECS: u64 = 7 * 24 * 3600;

/// How long a replay waits for an `in_flight` record to resolve.
pub const IN_FLIGHT_WAIT_SECS: u64 = 30;

/// Normalized key validation limits.
const MAX_KEY_LEN: usize = 128;

/// Lifecycle state of an idempotency record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdemState {
    /// Request is executing (short-term lock).
    InFlight,
    /// Request finished; cached response is valid until TTL.
    Completed,
    /// Request failed; cached only when the failure is idempotent-safe.
    Failed,
}

/// A persisted idempotency record on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdemRecord {
    /// Normalized idempotency key (`scope:stable-id`).
    pub key: String,
    /// Tool name this record belongs to (e.g. `shell_exec`).
    pub tool_name: String,
    /// Lifecycle state: in-flight, completed, or failed.
    pub state: IdemState,
    /// ISO timestamp when the record was created.
    pub created_at: String,
    /// ISO timestamp when the record completed (None while in-flight).
    pub completed_at: Option<String>,
    /// ISO timestamp when the record expires.
    pub ttl: String,
    /// The original tool request payload (redacted by callers as needed).
    pub request: Value,
    /// The cached tool response (for replay without re-execution).
    pub response: Value,
    /// Error message for failed records (None on success).
    pub error: Option<String>,
}

/// Disk-backed idempotency store.
#[derive(Debug, Clone)]
pub struct IdempotencyStore {
    root: PathBuf,
}

impl IdempotencyStore {
    /// Creates a store rooted at `<data_dir>/idempotency`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: data_dir.into().join("idempotency"),
        }
    }

    /// Environment-configured store: `FAMILYCLAW_DATA_DIR` or default `data/`.
    pub fn from_env() -> Self {
        let data_dir = std::env::var("FAMILYCLAW_DATA_DIR")
            .ok()
            .map_or_else(|| PathBuf::from("data"), PathBuf::from);
        Self::new(data_dir)
    }

    /// Validates + normalizes an idempotency key.
    pub fn normalize_key(raw: &str) -> Result<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ActionError::Proof(
                "idempotency_key must not be empty".into(),
            ));
        }
        if trimmed.len() > MAX_KEY_LEN {
            return Err(ActionError::Proof(format!(
                "idempotency_key too long (max {MAX_KEY_LEN})"
            )));
        }
        // Split scope (lowercased) from stable-id (case-sensitive).
        let (scope, id) = match trimmed.split_once(':') {
            Some((s, i)) => (s.trim().to_ascii_lowercase(), i.trim().to_string()),
            None => ("default".to_string(), trimmed.to_string()),
        };
        // Allowed chars for stable-id per design §4.3.
        for ch in id.chars() {
            if !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '@')) {
                return Err(ActionError::Proof(format!(
                    "invalid character in idempotency_key stable-id: {ch:?}"
                )));
            }
        }
        Ok(format!("{scope}:{id}"))
    }

    fn hash_key(key: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn record_path(&self, tool_name: &str, key: &str) -> PathBuf {
        let hash = Self::hash_key(key);
        self.root
            .join(tool_name)
            .join(&hash[..2])
            .join(format!("{hash}.json"))
    }

    fn now_iso() -> String {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        format!("{secs}Z")
    }

    fn ttl_iso(now_iso: &str, ttl_secs: u64) -> String {
        let secs: u64 = now_iso.trim_end_matches('Z').parse().unwrap_or(0);
        format!("{}Z", secs + ttl_secs)
    }

    fn is_expired(ttl: &str) -> bool {
        let ttl_secs: u64 = ttl.trim_end_matches('Z').parse().unwrap_or(0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        now > ttl_secs
    }

    /// Looks up a key. Returns None when absent or TTL-expired (lazy cleanup).
    pub fn lookup(&self, tool_name: &str, key: &str) -> Result<Option<IdemRecord>> {
        let path = self.record_path(tool_name, key);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| ActionError::ExecutionFailed(format!("idempotency read failed: {e}")))?;
        let rec: IdemRecord = serde_json::from_str(&raw)
            .map_err(|e| ActionError::ExecutionFailed(format!("idempotency parse failed: {e}")))?;
        if Self::is_expired(&rec.ttl) {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
        Ok(Some(rec))
    }

    /// Creates/updates a record atomically (tmp + rename).
    pub fn put(&self, tool_name: &str, rec: &IdemRecord) -> Result<()> {
        let path = self.record_path(tool_name, &rec.key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ActionError::ExecutionFailed(format!("idempotency dir create failed: {e}"))
            })?;
        }
        let json_str = serde_json::to_string(rec).map_err(|e| {
            ActionError::ExecutionFailed(format!("idempotency serialize failed: {e}"))
        })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json_str.as_bytes())
            .map_err(|e| ActionError::ExecutionFailed(format!("idempotency write failed: {e}")))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| ActionError::ExecutionFailed(format!("idempotency rename failed: {e}")))?;
        Ok(())
    }

    /// Records a completed execution.
    pub fn complete(
        &self,
        tool_name: &str,
        key: &str,
        request: Value,
        response: Value,
        ttl_secs: u64,
    ) -> Result<()> {
        let now = Self::now_iso();
        let rec = IdemRecord {
            key: key.to_string(),
            tool_name: tool_name.to_string(),
            state: IdemState::Completed,
            created_at: now.clone(),
            completed_at: Some(now.clone()),
            ttl: Self::ttl_iso(&now, ttl_secs),
            request,
            response,
            error: None,
        };
        self.put(tool_name, &rec)
    }

    /// Records an in-flight execution (lock).
    pub fn begin(&self, tool_name: &str, key: &str, request: Value, ttl_secs: u64) -> Result<()> {
        let now = Self::now_iso();
        let rec = IdemRecord {
            key: key.to_string(),
            tool_name: tool_name.to_string(),
            state: IdemState::InFlight,
            created_at: now.clone(),
            completed_at: None,
            ttl: Self::ttl_iso(&now, ttl_secs),
            request,
            response: Value::Null,
            error: None,
        };
        self.put(tool_name, &rec)
    }

    /// Marks a record as failed (optionally cached).
    pub fn fail(
        &self,
        tool_name: &str,
        key: &str,
        request: Value,
        error: String,
        cacheable: bool,
        ttl_secs: u64,
    ) -> Result<()> {
        let now = Self::now_iso();
        let rec = IdemRecord {
            key: key.to_string(),
            tool_name: tool_name.to_string(),
            state: if cacheable {
                IdemState::Failed
            } else {
                IdemState::Completed
            },
            created_at: now.clone(),
            completed_at: Some(now.clone()),
            ttl: Self::ttl_iso(&now, ttl_secs),
            request,
            response: Value::Null,
            error: Some(error),
        };
        self.put(tool_name, &rec)
    }

    /// Removes a record (used when a failed call must be retried as new).
    pub fn delete(&self, tool_name: &str, key: &str) -> Result<()> {
        let path = self.record_path(tool_name, key);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                ActionError::ExecutionFailed(format!("idempotency delete failed: {e}"))
            })?;
        }
        Ok(())
    }
}
