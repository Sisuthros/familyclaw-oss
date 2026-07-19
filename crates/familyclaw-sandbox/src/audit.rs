//! Audit log for the sandbox's capability checks and execution lifecycle.
//!
//! Containment requirement #5 (audit logging) requires that **every
//! capability check** (granted / denied) as well as **the start and end of
//! every execution** be recorded to an immutable, serializable log. An
//! analysis of 698 incidents (2604.23425) showed that without an audit log,
//! escapes cannot be detected after the fact.
//!
//! ## Design principle: does not break the existing interface
//! [`CapabilitySet`]'s public methods remain unchanged (they are pure,
//! side-effect-free queries). Auditing is wired in as an **optional**
//! [`AuditedCapabilities`] view: it wraps a reference to a [`CapabilitySet`]
//! and a reference to an [`AuditLog`], and offers the same check methods,
//! which **additionally** record the result. A caller that doesn't need
//! auditing can use [`CapabilitySet`] directly as before.
//!
//! The log is **append-only**: the public API offers no modification or
//! deletion, only appending and reading.
//!
//! ## Example
//! ```
//! use familyclaw_sandbox::{AuditLog, AuditedCapabilities, Capability, CapabilitySet};
//!
//! let caps = CapabilitySet::deny_all().with(Capability::network("api.example.com"));
//! let mut log = AuditLog::new();
//! {
//!     let mut audited = AuditedCapabilities::new(&caps, &mut log);
//!     assert!(audited.allows_network_host("api.example.com")); // granted
//!     assert!(!audited.allows_network_host("evil.example.com")); // denied
//! }
//!
//! assert_eq!(log.len(), 2);
//! assert_eq!(log.denied_count(), 1);
//! ```

use serde::{Deserialize, Serialize};

use crate::capability::CapabilitySet;

/// The target of a capability check — what access the code attempted to use.
///
/// Generic and serializable so the log can be persisted to a durable store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "target")]
#[non_exhaustive]
pub enum CapabilityCheck {
    /// Network access to a named host.
    Network(String),
    /// Read access to a filesystem path.
    ReadPath(String),
    /// Reading an environment variable.
    EnvVar(String),
}

impl CapabilityCheck {
    /// A network check against the given host.
    pub fn network(host: impl Into<String>) -> Self {
        Self::Network(host.into())
    }

    /// A path check against the given path.
    pub fn read_path(path: impl Into<String>) -> Self {
        Self::ReadPath(path.into())
    }

    /// An environment variable check for the given name.
    pub fn env_var(name: impl Into<String>) -> Self {
        Self::EnvVar(name.into())
    }
}

/// A single entry in the audit log.
///
/// `#[non_exhaustive]` so that new event types can be added without breaking
/// downstream code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
#[non_exhaustive]
pub enum AuditEntry {
    /// Execution started — records the backend name and the code size in bytes.
    ExecutionStart {
        /// The backend identifier (e.g. `"noop"`, `"wasmtime"`).
        backend: String,
        /// The size of the executed code in bytes.
        code_len: usize,
    },

    /// A capability check was performed.
    CapabilityCheck {
        /// What access was attempted.
        check: CapabilityCheck,
        /// Whether access was granted (`true`) or denied (`false`).
        granted: bool,
    },

    /// Execution ended — records whether it succeeded and the fuel consumed.
    ExecutionEnd {
        /// Whether the execution completed successfully.
        success: bool,
        /// Fuel consumed, if known.
        fuel_consumed: Option<u64>,
    },
}

/// An append-only audit log.
///
/// Records capability checks and the execution lifecycle. The public API
/// permits only appending and reading — no modification or deletion — so the
/// log is immutable evidence per containment requirement #5.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    /// Creates an empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an entry to the end of the log (append-only).
    pub fn record(&mut self, entry: AuditEntry) {
        self.entries.push(entry);
    }

    /// Records the start of an execution.
    pub fn record_execution_start(&mut self, backend: impl Into<String>, code_len: usize) {
        self.record(AuditEntry::ExecutionStart {
            backend: backend.into(),
            code_len,
        });
    }

    /// Records the end of an execution.
    pub fn record_execution_end(&mut self, success: bool, fuel_consumed: Option<u64>) {
        self.record(AuditEntry::ExecutionEnd {
            success,
            fuel_consumed,
        });
    }

    /// Records a capability check along with its result.
    pub fn record_capability_check(&mut self, check: CapabilityCheck, granted: bool) {
        self.record(AuditEntry::CapabilityCheck { check, granted });
    }

    /// All entries in append order.
    #[must_use]
    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of denied capability checks.
    #[must_use]
    pub fn denied_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, AuditEntry::CapabilityCheck { granted: false, .. }))
            .count()
    }

    /// The number of granted capability checks.
    #[must_use]
    pub fn granted_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, AuditEntry::CapabilityCheck { granted: true, .. }))
            .count()
    }
}

/// An auditing view over a [`CapabilitySet`].
///
/// Wraps a reference to the capability set and a mutable reference to an
/// [`AuditLog`]. Offers the same check methods as [`CapabilitySet`], but
/// **records every check** to the log. This is an optional hook: the
/// public API of the existing types does not change.
///
/// The lifetime `'a` binds both borrows to the same duration: the view
/// cannot outlive the set or the log.
#[derive(Debug)]
pub struct AuditedCapabilities<'a> {
    caps: &'a CapabilitySet,
    log: &'a mut AuditLog,
}

impl<'a> AuditedCapabilities<'a> {
    /// Builds an auditing view over a capability set and a log.
    pub fn new(caps: &'a CapabilitySet, log: &'a mut AuditLog) -> Self {
        Self { caps, log }
    }

    /// Whether network access to the host is granted — the check is recorded.
    pub fn allows_network_host(&mut self, host: &str) -> bool {
        let granted = self.caps.allows_network_host(host);
        self.log
            .record_capability_check(CapabilityCheck::network(host), granted);
        granted
    }

    /// Whether read access to the path is granted — the check is recorded.
    pub fn allows_read_path(&mut self, path: &str) -> bool {
        let granted = self.caps.allows_read_path(path);
        self.log
            .record_capability_check(CapabilityCheck::read_path(path), granted);
        granted
    }

    /// Whether reading the environment variable is allowed — the check is recorded.
    pub fn allows_env_var(&mut self, name: &str) -> bool {
        let granted = self.caps.allows_env_var(name);
        self.log
            .record_capability_check(CapabilityCheck::env_var(name), granted);
        granted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;

    #[test]
    fn new_log_is_empty() {
        let log = AuditLog::new();
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
        assert_eq!(log.denied_count(), 0);
        assert_eq!(log.granted_count(), 0);
    }

    #[test]
    fn record_is_append_only_and_ordered() {
        let mut log = AuditLog::new();
        log.record_execution_start("noop", 4);
        log.record_capability_check(CapabilityCheck::network("h"), true);
        log.record_execution_end(true, Some(7));

        let entries = log.entries();
        assert_eq!(entries.len(), 3);
        assert!(matches!(entries[0], AuditEntry::ExecutionStart { .. }));
        assert!(matches!(entries[1], AuditEntry::CapabilityCheck { .. }));
        assert!(matches!(entries[2], AuditEntry::ExecutionEnd { .. }));
    }

    #[test]
    fn audited_view_records_denied_capability() {
        // Empty set: all checks are denied.
        let caps = CapabilitySet::deny_all();
        let mut log = AuditLog::new();
        {
            let mut audited = AuditedCapabilities::new(&caps, &mut log);
            assert!(!audited.allows_network_host("evil.example.com"));
        }
        assert_eq!(log.len(), 1);
        assert_eq!(log.denied_count(), 1);
        assert_eq!(log.granted_count(), 0);
        assert_eq!(
            log.entries()[0],
            AuditEntry::CapabilityCheck {
                check: CapabilityCheck::network("evil.example.com"),
                granted: false,
            }
        );
    }

    #[test]
    fn audited_view_records_granted_capability() {
        let caps = CapabilitySet::deny_all().with(Capability::network("api.example.com"));
        let mut log = AuditLog::new();
        {
            let mut audited = AuditedCapabilities::new(&caps, &mut log);
            assert!(audited.allows_network_host("api.example.com"));
        }
        assert_eq!(log.granted_count(), 1);
        assert_eq!(log.denied_count(), 0);
    }

    #[test]
    fn audited_view_records_each_check_type() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::read_only_fs("/data"))
            .with(Capability::env_var("HOME"));
        let mut log = AuditLog::new();
        {
            let mut audited = AuditedCapabilities::new(&caps, &mut log);
            assert!(audited.allows_read_path("/data/file"));
            assert!(!audited.allows_read_path("/secret"));
            assert!(audited.allows_env_var("HOME"));
            assert!(!audited.allows_env_var("SECRET_KEY"));
            assert!(!audited.allows_network_host("h"));
        }
        assert_eq!(log.len(), 5);
        assert_eq!(log.granted_count(), 2);
        assert_eq!(log.denied_count(), 3);
    }

    #[test]
    fn audit_log_serde_roundtrip() {
        let mut log = AuditLog::new();
        log.record_execution_start("noop", 4);
        log.record_capability_check(CapabilityCheck::read_path("/data"), false);
        log.record_execution_end(false, None);

        let json = serde_json::to_string(&log).expect("serialize");
        let back: AuditLog = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(log, back);
    }

    #[test]
    fn underlying_capability_set_api_unchanged() {
        // Auditing does not change CapabilitySet's behavior: a direct query
        // gives the same result as the audited view.
        let caps = CapabilitySet::deny_all().with(Capability::network("h"));
        let direct = caps.allows_network_host("h");
        let mut log = AuditLog::new();
        let audited_result = {
            let mut audited = AuditedCapabilities::new(&caps, &mut log);
            audited.allows_network_host("h")
        };
        assert_eq!(direct, audited_result);
    }
}
