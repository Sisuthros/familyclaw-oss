//! Agent registry: agent metadata, registration, liveness, and heartbeats.
//!
//! This module provides [`AgentRegistry`] — a thread-safe registry of the
//! family's agents. Each agent is described by an [`AgentInfo`] struct, and
//! its "liveness" is derived from its most recent heartbeat relative to a
//! configured timeout ([`Liveness`]).
//!
//! The registry is intentionally decoupled from the transport layer (no MCP
//! or HTTP bindings) — adapters are wired in later. Internal state is
//! protected by a [`tokio::sync::RwLock`], so multiple tasks can read
//! concurrently while writes are serialized.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Duration;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::{self, Timestamp};
use familyclaw_core::{FamilyClawError, Result};

/// An agent's role in the family's division of labor.
///
/// Mirrors the role set of the existing family-bridge interface, but made
/// generic (not tied to individual family members).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Strategy and coordination (e.g. deep analyses).
    Strategy,
    /// Task executor (code, implementation).
    Executor,
    /// Scout (lightweight, inquisitive presence).
    Scout,
    /// Field operator (desktop/device automation).
    FieldOperator,
}

/// The kind of runtime environment (host) an agent runs on.
///
/// Generic — does not refer to actual machines or paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// Local native process.
    Local,
    /// WSL2 environment.
    Wsl,
    /// Separate hardware node.
    Hardware,
    /// "Body side" — embodied/peripheral runtime environment.
    BodySide,
}

/// An agent's liveness state derived from its most recent heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// The heartbeat is more recent than the timeout → the agent is reachable.
    Online,
    /// The most recent heartbeat is older than the timeout → unreachable.
    Offline,
    /// No heartbeat has ever been received from this agent since registration.
    Unknown,
}

/// Description of a single agent (family member) in the registry.
///
/// **OSS boundary:** fields are generic. Soul/persona/keys have no place
/// here — `preferred_model` is just a model name (e.g. `"provider/model"`),
/// never a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    /// The agent's stable identifier.
    pub id: AgentId,

    /// Display name (generic, e.g. `"agent_a"`).
    pub display_name: String,

    /// Role in the division of labor.
    pub role: AgentRole,

    /// Runtime environment type.
    pub host_kind: HostKind,

    /// Capabilities (generic identifiers, e.g. `"browser"`, `"system.run"`).
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Preferred model name, if set (not a key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_model: Option<String>,

    /// Registration time (UTC).
    pub registered_at: Timestamp,

    /// Time of the most recent heartbeat (UTC), or `None` if none received yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_heartbeat: Option<Timestamp>,
}

impl AgentInfo {
    /// Builds a new agent description with the required fields.
    ///
    /// `registered_at` is set to the current time, and `last_heartbeat` starts
    /// out as `None` (state [`Liveness::Unknown`] until the first heartbeat arrives).
    pub fn new(
        id: AgentId,
        display_name: impl Into<String>,
        role: AgentRole,
        host_kind: HostKind,
    ) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            role,
            host_kind,
            capabilities: Vec::new(),
            preferred_model: None,
            registered_at: time::now(),
            last_heartbeat: None,
        }
    }

    /// Sets the capabilities (builder style).
    #[must_use]
    pub fn with_capabilities<I, S>(mut self, capabilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.capabilities = capabilities.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the preferred model name (builder style).
    #[must_use]
    pub fn with_preferred_model(mut self, model: impl Into<String>) -> Self {
        self.preferred_model = Some(model.into());
        self
    }

    /// Validates the agent description.
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] if the display name is empty or any
    /// capability is an empty string.
    pub fn validate(&self) -> Result<()> {
        if self.display_name.trim().is_empty() {
            return Err(FamilyClawError::invalid_input(
                "agent display_name must not be empty",
            ));
        }
        if self.capabilities.iter().any(|c| c.trim().is_empty()) {
            return Err(FamilyClawError::invalid_input(
                "agent capability entries must not be empty",
            ));
        }
        Ok(())
    }

    /// Computes the agent's liveness state given the timeout and the current
    /// time `now`.
    ///
    /// `now` is passed as a parameter for determinism (makes testing and
    /// durable replay easier).
    #[must_use]
    pub fn liveness_at(&self, now: Timestamp, timeout: Duration) -> Liveness {
        match self.last_heartbeat {
            None => Liveness::Unknown,
            Some(hb) => {
                if now.signed_duration_since(hb) <= timeout {
                    Liveness::Online
                } else {
                    Liveness::Offline
                }
            }
        }
    }
}

/// A thread-safe registry of the family's agents.
///
/// Holds each agent's [`AgentInfo`] and handles registration, lookup,
/// heartbeats, and liveness computation. The timeout ([`heartbeat_timeout`])
/// determines when an agent is considered offline.
///
/// [`heartbeat_timeout`]: AgentRegistry::heartbeat_timeout
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    inner: Arc<RwLock<HashMap<AgentId, AgentInfo>>>,
    heartbeat_timeout: Duration,
}

/// Default liveness timeout (seconds): 30 s.
const DEFAULT_HEARTBEAT_TIMEOUT_SECS: i64 = 30;

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRegistry {
    /// Creates an empty registry with the default timeout (30 s).
    #[must_use]
    pub fn new() -> Self {
        Self::with_timeout(Duration::seconds(DEFAULT_HEARTBEAT_TIMEOUT_SECS))
    }

    /// Creates an empty registry with the given heartbeat timeout.
    ///
    /// A non-positive duration is normalized to zero, so agents that have not
    /// sent a heartbeat *at exactly now* show up as offline.
    #[must_use]
    pub fn with_timeout(heartbeat_timeout: Duration) -> Self {
        let heartbeat_timeout = if heartbeat_timeout < Duration::zero() {
            Duration::zero()
        } else {
            heartbeat_timeout
        };
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            heartbeat_timeout,
        }
    }

    /// Returns the registry's heartbeat timeout.
    #[must_use]
    pub fn heartbeat_timeout(&self) -> Duration {
        self.heartbeat_timeout
    }

    /// Registers an agent. If the same identifier is already in the registry,
    /// the description is replaced (idempotent re-registration).
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] if [`AgentInfo::validate`] fails.
    pub async fn register(&self, info: AgentInfo) -> Result<()> {
        info.validate()?;
        let mut guard = self.inner.write().await;
        guard.insert(info.id, info);
        Ok(())
    }

    /// Removes an agent from the registry. Returns the removed description if
    /// it existed.
    pub async fn deregister(&self, id: AgentId) -> Option<AgentInfo> {
        let mut guard = self.inner.write().await;
        guard.remove(&id)
    }

    /// Looks up an agent's description by identifier.
    pub async fn get(&self, id: AgentId) -> Option<AgentInfo> {
        let guard = self.inner.read().await;
        guard.get(&id).cloned()
    }

    /// Whether the given agent is registered.
    pub async fn contains(&self, id: AgentId) -> bool {
        let guard = self.inner.read().await;
        guard.contains_key(&id)
    }

    /// Number of registered agents.
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.len()
    }

    /// Whether the registry is empty.
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.is_empty()
    }

    /// Returns all agents' descriptions, ordered by identifier
    /// (deterministic order).
    pub async fn list(&self) -> Vec<AgentInfo> {
        let guard = self.inner.read().await;
        let mut out: Vec<AgentInfo> = guard.values().cloned().collect();
        out.sort_by_key(|a| a.id);
        out
    }

    /// Records a heartbeat for the agent at time `at`.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] if the agent is not registered.
    pub async fn heartbeat(&self, id: AgentId, at: Timestamp) -> Result<()> {
        let mut guard = self.inner.write().await;
        match guard.get_mut(&id) {
            Some(info) => {
                info.last_heartbeat = Some(at);
                Ok(())
            }
            None => Err(FamilyClawError::not_found(format!("agent {id}"))),
        }
    }

    /// Records a heartbeat at the current time.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] if the agent is not registered.
    pub async fn heartbeat_now(&self, id: AgentId) -> Result<()> {
        self.heartbeat(id, time::now()).await
    }

    /// Returns the agent's liveness state relative to time `now`.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] if the agent is not registered.
    pub async fn liveness_at(&self, id: AgentId, now: Timestamp) -> Result<Liveness> {
        let guard = self.inner.read().await;
        match guard.get(&id) {
            Some(info) => Ok(info.liveness_at(now, self.heartbeat_timeout)),
            None => Err(FamilyClawError::not_found(format!("agent {id}"))),
        }
    }

    /// Returns the agent's liveness state at the current time.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] if the agent is not registered.
    pub async fn liveness(&self, id: AgentId) -> Result<Liveness> {
        self.liveness_at(id, time::now()).await
    }

    /// Returns all agents that are online at time `now`, ordered by
    /// identifier.
    pub async fn online_at(&self, now: Timestamp) -> Vec<AgentInfo> {
        let guard = self.inner.read().await;
        let mut out: Vec<AgentInfo> = guard
            .values()
            .filter(|info| info.liveness_at(now, self.heartbeat_timeout) == Liveness::Online)
            .cloned()
            .collect();
        out.sort_by_key(|a| a.id);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> Timestamp {
        time::from_unix_secs(secs).expect("valid unix seconds")
    }

    fn sample(id: AgentId, name: &str) -> AgentInfo {
        AgentInfo::new(id, name, AgentRole::Executor, HostKind::Local)
    }

    #[test]
    fn agent_info_builder_sets_fields() {
        let id = AgentId::new();
        let info = sample(id, "agent_a")
            .with_capabilities(["browser", "system.run"])
            .with_preferred_model("provider/model");
        assert_eq!(info.id, id);
        assert_eq!(info.display_name, "agent_a");
        assert_eq!(info.role, AgentRole::Executor);
        assert_eq!(info.host_kind, HostKind::Local);
        assert_eq!(info.capabilities, vec!["browser", "system.run"]);
        assert_eq!(info.preferred_model.as_deref(), Some("provider/model"));
        assert!(info.last_heartbeat.is_none());
    }

    #[test]
    fn agent_info_validate_rejects_empty_name_and_capability() {
        let id = AgentId::new();
        let mut bad = sample(id, "   ");
        assert!(bad.validate().is_err());

        bad.display_name = "agent_a".into();
        bad.capabilities = vec!["ok".into(), "  ".into()];
        assert!(bad.validate().is_err());

        bad.capabilities = vec!["ok".into()];
        assert!(bad.validate().is_ok());
    }

    #[test]
    fn liveness_at_handles_never_online_offline() {
        let id = AgentId::new();
        let mut info = sample(id, "agent_a");
        let timeout = Duration::seconds(30);

        // No heartbeat → Unknown.
        assert_eq!(info.liveness_at(ts(100), timeout), Liveness::Unknown);

        // Heartbeat right now → Online.
        info.last_heartbeat = Some(ts(100));
        assert_eq!(info.liveness_at(ts(100), timeout), Liveness::Online);

        // 30 s later, at the boundary → Online (<=).
        assert_eq!(info.liveness_at(ts(130), timeout), Liveness::Online);

        // 31 s later → Offline.
        assert_eq!(info.liveness_at(ts(131), timeout), Liveness::Offline);
    }

    #[test]
    fn agent_info_serde_roundtrip() {
        let id = AgentId::new();
        let mut info = sample(id, "agent_a").with_capabilities(["x"]);
        info.last_heartbeat = Some(ts(42));
        let json = serde_json::to_string(&info).expect("serialize");
        let back: AgentInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, back);
    }

    #[tokio::test]
    async fn register_get_and_idempotent_replace() {
        let reg = AgentRegistry::new();
        let id = AgentId::new();
        assert!(reg.is_empty().await);

        reg.register(sample(id, "agent_a")).await.expect("register");
        assert_eq!(reg.len().await, 1);
        assert!(reg.contains(id).await);
        assert_eq!(
            reg.get(id).await.map(|i| i.display_name),
            Some("agent_a".to_string())
        );

        // Re-registering with the same id replaces the entry, without growing the count.
        reg.register(sample(id, "agent_a_renamed"))
            .await
            .expect("re-register");
        assert_eq!(reg.len().await, 1);
        assert_eq!(
            reg.get(id).await.map(|i| i.display_name),
            Some("agent_a_renamed".to_string())
        );
    }

    #[tokio::test]
    async fn register_rejects_invalid_info() {
        let reg = AgentRegistry::new();
        let id = AgentId::new();
        let err = reg
            .register(sample(id, "   "))
            .await
            .expect_err("empty name rejected");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        assert!(reg.is_empty().await);
    }

    #[tokio::test]
    async fn deregister_removes_and_returns() {
        let reg = AgentRegistry::new();
        let id = AgentId::new();
        reg.register(sample(id, "agent_a")).await.expect("register");

        let removed = reg.deregister(id).await;
        assert_eq!(removed.map(|i| i.display_name), Some("agent_a".to_string()));
        assert!(!reg.contains(id).await);
        assert!(reg.deregister(id).await.is_none());
    }

    #[tokio::test]
    async fn list_is_sorted_by_id() {
        let reg = AgentRegistry::new();
        let lo = AgentId::from_uuid(uuid::Uuid::from_u128(1));
        let hi = AgentId::from_uuid(uuid::Uuid::from_u128(2));
        // Register in reverse order.
        reg.register(sample(hi, "agent_hi")).await.expect("reg hi");
        reg.register(sample(lo, "agent_lo")).await.expect("reg lo");

        let list = reg.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, lo);
        assert_eq!(list[1].id, hi);
    }

    #[tokio::test]
    async fn heartbeat_unknown_agent_errors() {
        let reg = AgentRegistry::new();
        let err = reg
            .heartbeat(AgentId::new(), ts(1))
            .await
            .expect_err("unknown agent");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn heartbeat_updates_liveness() {
        let reg = AgentRegistry::with_timeout(Duration::seconds(30));
        let id = AgentId::new();
        reg.register(sample(id, "agent_a")).await.expect("register");

        // Before the heartbeat → Unknown.
        assert_eq!(
            reg.liveness_at(id, ts(100)).await.expect("liveness"),
            Liveness::Unknown
        );

        reg.heartbeat(id, ts(100)).await.expect("heartbeat");

        // Fresh → Online.
        assert_eq!(
            reg.liveness_at(id, ts(120)).await.expect("liveness"),
            Liveness::Online
        );
        // Stale → Offline.
        assert_eq!(
            reg.liveness_at(id, ts(200)).await.expect("liveness"),
            Liveness::Offline
        );
    }

    #[tokio::test]
    async fn liveness_unknown_agent_errors() {
        let reg = AgentRegistry::new();
        let err = reg
            .liveness_at(AgentId::new(), ts(1))
            .await
            .expect_err("unknown agent");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn online_at_filters_correctly() {
        let reg = AgentRegistry::with_timeout(Duration::seconds(30));
        let a = AgentId::from_uuid(uuid::Uuid::from_u128(1));
        let b = AgentId::from_uuid(uuid::Uuid::from_u128(2));
        let c = AgentId::from_uuid(uuid::Uuid::from_u128(3));
        reg.register(sample(a, "a")).await.expect("reg a");
        reg.register(sample(b, "b")).await.expect("reg b");
        reg.register(sample(c, "c")).await.expect("reg c");

        reg.heartbeat(a, ts(100)).await.expect("hb a"); // online @120
        reg.heartbeat(b, ts(50)).await.expect("hb b"); // offline @120 (70s old)
                                                       // c: no heartbeat → Unknown, not online.

        let online = reg.online_at(ts(120)).await;
        assert_eq!(online.len(), 1);
        assert_eq!(online[0].id, a);
    }

    #[tokio::test]
    async fn negative_timeout_is_normalized_to_zero() {
        let reg = AgentRegistry::with_timeout(Duration::seconds(-5));
        assert_eq!(reg.heartbeat_timeout(), Duration::zero());
        let id = AgentId::new();
        reg.register(sample(id, "agent_a")).await.expect("register");
        reg.heartbeat(id, ts(100)).await.expect("heartbeat");
        // Exactly now → Online (<= 0).
        assert_eq!(
            reg.liveness_at(id, ts(100)).await.expect("liveness"),
            Liveness::Online
        );
        // 1 s later → Offline.
        assert_eq!(
            reg.liveness_at(id, ts(101)).await.expect("liveness"),
            Liveness::Offline
        );
    }

    #[tokio::test]
    async fn registry_clone_shares_state() {
        let reg = AgentRegistry::new();
        let clone = reg.clone();
        let id = AgentId::new();
        reg.register(sample(id, "agent_a")).await.expect("register");
        // The clone sees the same state (shared Arc).
        assert!(clone.contains(id).await);
    }
}
