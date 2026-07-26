//! Shared imports for agent submodules.

#![allow(unused_imports, clippy::wildcard_imports)]

pub use std::collections::{HashMap, VecDeque};
pub use std::future::Future;
pub use std::pin::Pin;
pub use std::sync::atomic::{AtomicBool, Ordering};
pub use std::sync::Arc;
pub use std::time::{Duration, Instant};

pub use futures_util::StreamExt;

pub use familyclaw_actions::{
    ActionId, ActionRuntime, ActionTaskId, ApprovalId, AuditCollector, AuditKind, ExecAuditEvent,
    McpToolDescriptor,
};
pub use familyclaw_bus::{
    BeingId, BeingInfo, BusHandle, BusMessage, MessageOrigin, ResonanceMessage, TaskEventKind,
};
pub use familyclaw_channels::{OutboundKind, OutboundMessage};
pub use familyclaw_core::time::Timestamp;
pub use familyclaw_core::{time, AgentConfig, FamilyClawError, Result};
pub use familyclaw_durable::{DurableContext, Journal};
pub use familyclaw_emotion::{
    default_governing_profile, ActionDecision, Dimension, EmotionActionGoverning,
    EmotionActionGovernor, EmotionCalibration, EmotionState, GoverningProfile, NeutralCalibration,
};
pub use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, Memory, MemoryStore, RetrievalContext, RetrievalResult,
};
pub use ractor::{Actor, ActorProcessingErr, ActorRef};
pub use tokio::sync::Mutex;
pub use tracing::{debug, info, warn};

pub use crate::llm::{
    LlmConfig, LlmError, LlmFailureClass, LlmImageRef, LlmMessage, ToolCall, ToolDefinition,
};
pub use crate::llm_chain::LlmFailover;
pub use crate::resumable::{InMemoryResumableStore, ResumableTurn, ResumableTurnStore};
pub use crate::soul::Soul;
pub use crate::watchdog;
pub use familyclaw_sandbox::{CodeSandbox, SandboxOutput, SandboxRequest};
