//! [`FamilyBridge`] — a composing facade over the agent registry, task board,
//! and event bus.
//!
//! The individual parts ([`AgentRegistry`], [`TaskBoard`], [`EventBus`]) are
//! also usable on their own, but in most cases you want state changes
//! (registration, heartbeat, task creation, handoff) to automatically publish
//! the corresponding [`Event`]. [`FamilyBridge`] handles this wiring while
//! preserving the same thread safety (all parts share their state via `Arc`,
//! so the facade can be cloned freely).

use serde::Serialize;

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::{self, Timestamp};
use familyclaw_core::Result;

use crate::agent::{AgentInfo, AgentRegistry, Liveness};
use crate::event::{Event, EventBus, EventKind, EventSubscriber};
use crate::task::{Task, TaskBoard, TaskId, TaskStatus};

/// A composing facade over the bridge layer.
///
/// Encapsulates the registry, board, and bus, and publishes events for state
/// changes. A clone shares the same state.
#[derive(Debug, Clone)]
pub struct FamilyBridge {
    registry: AgentRegistry,
    board: TaskBoard,
    bus: EventBus,
}

impl Default for FamilyBridge {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper struct for event payloads (serializes to a JSON object).
#[derive(Serialize)]
struct StatusChangePayload {
    task_id: String,
    from: String,
    to: String,
}

/// Helper struct for the handoff event's payload.
#[derive(Serialize)]
struct HandoffPayload {
    task_id: String,
    from_agent: String,
    to_agent: String,
}

impl FamilyBridge {
    /// Creates a new bridge with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: AgentRegistry::new(),
            board: TaskBoard::new(),
            bus: EventBus::new(),
        }
    }

    /// Builds a bridge from the given parts (e.g. a custom timeout or bus
    /// capacity).
    #[must_use]
    pub fn from_parts(registry: AgentRegistry, board: TaskBoard, bus: EventBus) -> Self {
        Self {
            registry,
            board,
            bus,
        }
    }

    /// Access to the agent registry.
    #[must_use]
    pub fn registry(&self) -> &AgentRegistry {
        &self.registry
    }

    /// Access to the task board.
    #[must_use]
    pub fn board(&self) -> &TaskBoard {
        &self.board
    }

    /// Access to the event bus.
    #[must_use]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Subscribes to the event bus.
    #[must_use]
    pub fn subscribe(&self) -> EventSubscriber {
        self.bus.subscribe()
    }

    // --- Agents --------------------------------------------------------------

    /// Registers an agent and publishes [`EventKind::AgentRegistered`].
    ///
    /// # Errors
    /// Propagates errors from [`AgentRegistry::register`].
    pub async fn register_agent(&self, info: AgentInfo) -> Result<()> {
        let id = info.id;
        self.registry.register(info).await?;
        self.bus
            .publish(Event::new(EventKind::AgentRegistered, Some(id)));
        Ok(())
    }

    /// Removes an agent and publishes [`EventKind::AgentDeregistered`] if the
    /// agent existed.
    pub async fn deregister_agent(&self, id: AgentId) -> Option<AgentInfo> {
        let removed = self.registry.deregister(id).await;
        if removed.is_some() {
            self.bus
                .publish(Event::new(EventKind::AgentDeregistered, Some(id)));
        }
        removed
    }

    /// Records a heartbeat at time `at` and publishes
    /// [`EventKind::AgentHeartbeat`].
    ///
    /// # Errors
    /// Propagates errors from [`AgentRegistry::heartbeat`].
    pub async fn heartbeat(&self, id: AgentId, at: Timestamp) -> Result<()> {
        self.registry.heartbeat(id, at).await?;
        self.bus
            .publish(Event::new(EventKind::AgentHeartbeat, Some(id)));
        Ok(())
    }

    /// Records a heartbeat at the current time.
    ///
    /// # Errors
    /// Propagates errors from [`AgentRegistry::heartbeat`].
    pub async fn heartbeat_now(&self, id: AgentId) -> Result<()> {
        self.heartbeat(id, time::now()).await
    }

    /// Returns the agent's liveness state at the current time.
    ///
    /// # Errors
    /// Propagates errors from [`AgentRegistry::liveness`].
    pub async fn liveness(&self, id: AgentId) -> Result<Liveness> {
        self.registry.liveness(id).await
    }

    /// Lists all registered agents.
    pub async fn list_agents(&self) -> Vec<AgentInfo> {
        self.registry.list().await
    }

    // --- Tasks -----------------------------------------------------------

    /// Creates a task and publishes [`EventKind::TaskCreated`].
    ///
    /// # Errors
    /// Propagates errors from [`TaskBoard::create`].
    pub async fn create_task(
        &self,
        title: impl Into<String>,
        assignee: Option<AgentId>,
    ) -> Result<Task> {
        let task = self.board.create(title, assignee).await?;
        self.bus
            .publish(Event::new(EventKind::TaskCreated, assignee));
        Ok(task)
    }

    /// Changes a task's status and publishes [`EventKind::TaskStatusChanged`]
    /// if the status actually changed.
    ///
    /// # Errors
    /// Propagates errors from [`TaskBoard::update_status`].
    pub async fn update_task_status(&self, id: TaskId, next: TaskStatus) -> Result<Task> {
        let before = self.board.get(id).await.map(|t| t.status);
        let task = self.board.update_status(id, next).await?;
        if before != Some(task.status) {
            let payload = StatusChangePayload {
                task_id: id.to_string(),
                from: before.map_or_else(|| "unknown".to_string(), |s| format!("{s:?}")),
                to: format!("{:?}", task.status),
            };
            // Payload serialization cannot fail for these fields; on error,
            // publish without a payload.
            let event = Event::with_payload(EventKind::TaskStatusChanged, task.assignee, &payload)
                .unwrap_or_else(|_| Event::new(EventKind::TaskStatusChanged, task.assignee));
            self.bus.publish(event);
        }
        Ok(task)
    }

    /// Hands off a task from agent `from` to agent `to` and publishes
    /// [`EventKind::TaskHandedOff`].
    ///
    /// # Errors
    /// Propagates errors from [`TaskBoard::handoff`].
    pub async fn handoff_task(&self, id: TaskId, from: AgentId, to: AgentId) -> Result<Task> {
        let task = self.board.handoff(id, from, to).await?;
        let payload = HandoffPayload {
            task_id: id.to_string(),
            from_agent: from.to_string(),
            to_agent: to.to_string(),
        };
        let event = Event::with_payload(EventKind::TaskHandedOff, Some(from), &payload)
            .unwrap_or_else(|_| Event::new(EventKind::TaskHandedOff, Some(from)));
        self.bus.publish(event);
        Ok(task)
    }

    /// Lists all tasks.
    pub async fn list_tasks(&self) -> Vec<Task> {
        self.board.list().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentRole, HostKind};

    fn agent(name: &str) -> AgentInfo {
        AgentInfo::new(AgentId::new(), name, AgentRole::Executor, HostKind::Local)
    }

    #[tokio::test]
    async fn register_agent_emits_event() {
        let bridge = FamilyBridge::new();
        let mut sub = bridge.subscribe();
        let info = agent("agent_a");
        let id = info.id;
        bridge.register_agent(info).await.expect("register");

        let event = sub.recv().await.expect("event");
        assert_eq!(event.kind, EventKind::AgentRegistered);
        assert_eq!(event.source, Some(id));
        assert_eq!(bridge.list_agents().await.len(), 1);
    }

    #[tokio::test]
    async fn deregister_emits_only_when_present() {
        let bridge = FamilyBridge::new();
        let info = agent("agent_a");
        let id = info.id;
        bridge.register_agent(info).await.expect("register");

        let mut sub = bridge.subscribe();
        let removed = bridge.deregister_agent(id).await;
        assert!(removed.is_some());
        let event = sub.recv().await.expect("event");
        assert_eq!(event.kind, EventKind::AgentDeregistered);

        // Toinen poisto: ei tapahtumaa.
        assert!(bridge.deregister_agent(id).await.is_none());
        assert!(sub.try_recv().expect("no error").is_none());
    }

    #[tokio::test]
    async fn heartbeat_emits_event_and_updates_liveness() {
        let bridge = FamilyBridge::new();
        let info = agent("agent_a");
        let id = info.id;
        bridge.register_agent(info).await.expect("register");

        let mut sub = bridge.subscribe();
        bridge.heartbeat_now(id).await.expect("heartbeat");
        let event = sub.recv().await.expect("event");
        assert_eq!(event.kind, EventKind::AgentHeartbeat);
        assert_eq!(
            bridge.liveness(id).await.expect("liveness"),
            Liveness::Online
        );
    }

    #[tokio::test]
    async fn create_task_emits_event() {
        let bridge = FamilyBridge::new();
        let mut sub = bridge.subscribe();
        let task = bridge.create_task("t", None).await.expect("create");
        let event = sub.recv().await.expect("event");
        assert_eq!(event.kind, EventKind::TaskCreated);
        assert_eq!(bridge.list_tasks().await.len(), 1);
        assert_eq!(task.status, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn update_status_emits_with_payload() {
        let bridge = FamilyBridge::new();
        let task = bridge.create_task("t", None).await.expect("create");
        let mut sub = bridge.subscribe();
        bridge
            .update_task_status(task.id, TaskStatus::Active)
            .await
            .expect("activate");
        let event = sub.recv().await.expect("event");
        assert_eq!(event.kind, EventKind::TaskStatusChanged);
        assert_eq!(event.payload["from"], serde_json::json!("Pending"));
        assert_eq!(event.payload["to"], serde_json::json!("Active"));
    }

    #[tokio::test]
    async fn update_status_noop_does_not_emit() {
        let bridge = FamilyBridge::new();
        let task = bridge.create_task("t", None).await.expect("create");
        let mut sub = bridge.subscribe();
        // Pending -> Pending = no-op, ei tapahtumaa.
        bridge
            .update_task_status(task.id, TaskStatus::Pending)
            .await
            .expect("noop");
        assert!(sub.try_recv().expect("no error").is_none());
    }

    #[tokio::test]
    async fn handoff_emits_event_with_payload() {
        let bridge = FamilyBridge::new();
        let from = AgentId::new();
        let to = AgentId::new();
        let task = bridge.create_task("t", Some(from)).await.expect("create");
        let mut sub = bridge.subscribe();
        let handed = bridge
            .handoff_task(task.id, from, to)
            .await
            .expect("handoff");
        assert_eq!(handed.assignee, Some(to));
        assert_eq!(handed.status, TaskStatus::Handed);

        let event = sub.recv().await.expect("event");
        assert_eq!(event.kind, EventKind::TaskHandedOff);
        assert_eq!(
            event.payload["from_agent"],
            serde_json::json!(from.to_string())
        );
        assert_eq!(event.payload["to_agent"], serde_json::json!(to.to_string()));
    }

    #[tokio::test]
    async fn handoff_failure_does_not_emit() {
        let bridge = FamilyBridge::new();
        let from = AgentId::new();
        let to = AgentId::new();
        let task = bridge.create_task("t", None).await.expect("create"); // ei assignee
        let mut sub = bridge.subscribe();
        let err = bridge.handoff_task(task.id, from, to).await;
        assert!(err.is_err());
        assert!(sub.try_recv().expect("no error").is_none());
    }

    #[tokio::test]
    async fn from_parts_and_clone_share_state() {
        let bridge = FamilyBridge::from_parts(
            AgentRegistry::new(),
            TaskBoard::new(),
            EventBus::with_capacity(8),
        );
        let clone = bridge.clone();
        bridge
            .register_agent(agent("agent_a"))
            .await
            .expect("register");
        assert_eq!(clone.list_agents().await.len(), 1);
    }
}
