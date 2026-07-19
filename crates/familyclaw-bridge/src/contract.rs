//! Contracts between agents: a typed FIPA `ContractNet` implementation.
//!
//! This module gives agents a **verifiable** way to agree on work: a provider
//! advertises a [`Capability`] (which has a typed input/output schema and
//! pre-/postconditions), a requester makes a contract proposal
//! ([`ContractBoard::propose`]) which is validated against the input schema,
//! and the contract is fulfilled ([`ContractBoard::fulfill`]) only once the
//! deliverable passes the output schema and **every** postcondition.
//!
//! ## Why typed?
//! A plain "do this" is not enough for reliable multi-agent work: a
//! machine-checkable promise is needed. [`Schema`] checks structure,
//! [`Clause`] checks assertions ("field X exists", "the list is not empty",
//! "the value is ≥ N"). Breaching a postcondition moves the contract to
//! [`ContractStatus::Failed`] with an error — never silent acceptance.
//!
//! ## OSS boundary
//! Generic: no hardcoded capabilities, souls, or keys. Payloads are
//! `serde_json::Value`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use familyclaw_core::ids::{AgentId, MessageId};
use familyclaw_core::time::Timestamp;
use familyclaw_core::FamilyClawError;

use crate::task::TaskId;

// ===========================================================================
// Schema and fields
// ===========================================================================

/// The expected type of a single field in a schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// String (`string`).
    Str,
    /// Integer or floating-point number (`number`).
    Int,
    /// Boolean (`bool`).
    Bool,
    /// List (`array`).
    Arr,
    /// Object (`object`).
    Obj,
}

impl FieldType {
    /// Whether the given JSON value matches this type.
    #[must_use]
    pub fn matches(self, value: &Value) -> bool {
        match self {
            FieldType::Str => value.is_string(),
            // `Int` accepts any JSON number (integer or floating-point).
            FieldType::Int => value.is_number(),
            FieldType::Bool => value.is_boolean(),
            FieldType::Arr => value.is_array(),
            FieldType::Obj => value.is_object(),
        }
    }

    /// The type's stable name for error messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::Str => "string",
            FieldType::Int => "number",
            FieldType::Bool => "bool",
            FieldType::Arr => "array",
            FieldType::Obj => "object",
        }
    }
}

/// A single field description in a schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// The field's name (key in the object).
    pub name: String,

    /// The field's expected type.
    pub ty: FieldType,

    /// Whether the field is required. A missing required field is a
    /// violation; a missing optional field is allowed, but if a value is
    /// present, its type must match.
    #[serde(default = "default_true")]
    pub required: bool,
}

/// Serde default for the `required` field (`true`).
const fn default_true() -> bool {
    true
}

impl Field {
    /// A required field.
    pub fn required(name: impl Into<String>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: true,
        }
    }

    /// An optional field.
    pub fn optional(name: impl Into<String>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: false,
        }
    }
}

/// A description of a single schema violation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaViolation {
    /// The name of the field the violation concerns.
    pub field: String,

    /// A human-readable reason (e.g. "missing required field", "expected number").
    pub reason: String,
}

/// A typed object schema: a set of named fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    /// The schema's fields.
    pub fields: Vec<Field>,
}

impl Schema {
    /// Builds a schema from a list of fields.
    #[must_use]
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    /// An empty schema (accepts any object).
    #[must_use]
    pub fn empty() -> Self {
        Self { fields: Vec::new() }
    }

    /// Checks a value against the schema and returns all violations.
    ///
    /// An empty return means the value passed. If the value is not an object
    /// at all, a single violation is returned for the pseudo-field `"$root"`.
    #[must_use]
    pub fn check(&self, value: &Value) -> Vec<SchemaViolation> {
        let mut out = Vec::new();
        let Some(obj) = value.as_object() else {
            out.push(SchemaViolation {
                field: "$root".to_string(),
                reason: "expected object".to_string(),
            });
            return out;
        };
        for field in &self.fields {
            match obj.get(&field.name) {
                None => {
                    if field.required {
                        out.push(SchemaViolation {
                            field: field.name.clone(),
                            reason: "missing required field".to_string(),
                        });
                    }
                }
                Some(Value::Null) if field.required => {
                    out.push(SchemaViolation {
                        field: field.name.clone(),
                        reason: "required field is null".to_string(),
                    });
                }
                Some(v) => {
                    if !field.ty.matches(v) {
                        out.push(SchemaViolation {
                            field: field.name.clone(),
                            reason: format!("expected {}", field.ty.as_str()),
                        });
                    }
                }
            }
        }
        out
    }

    /// Whether the value conforms to the schema (no violations).
    #[must_use]
    pub fn is_valid(&self, value: &Value) -> bool {
        self.check(value).is_empty()
    }
}

// ===========================================================================
// Clauses
// ===========================================================================

/// A clause's operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClauseOp {
    /// The field exists and is not `null`.
    Present,
    /// The field exists and is not empty (string/list/object).
    NonEmpty,
    /// The field's value equals the comparison value.
    Eq,
    /// The field's numeric value is ≥ the comparison value.
    Gte,
    /// The field's numeric value is ≤ the comparison value.
    Lte,
    /// The field's length (string/list/object) is ≥ the comparison value.
    MinLen,
    /// The field's length (string/list/object) is ≤ the comparison value.
    MaxLen,
}

/// A single clause: an assertion about a field in a deliverable/input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    /// The name of the field being checked.
    pub field: String,

    /// The operator.
    pub op: ClauseOp,

    /// The comparison value (a number, string, etc. depending on the
    /// operator). `Present`/`NonEmpty` ignore this.
    #[serde(default)]
    pub value: Value,
}

impl Clause {
    /// `field` exists and is not `null`.
    pub fn present(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::Present,
            value: Value::Null,
        }
    }

    /// `field` is not empty.
    pub fn non_empty(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::NonEmpty,
            value: Value::Null,
        }
    }

    /// `field == value`.
    pub fn eq(field: impl Into<String>, value: Value) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::Eq,
            value,
        }
    }

    /// `field >= value` (numeric).
    pub fn gte(field: impl Into<String>, value: Value) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::Gte,
            value,
        }
    }

    /// `field <= value` (numeric).
    pub fn lte(field: impl Into<String>, value: Value) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::Lte,
            value,
        }
    }

    /// `len(field) >= value`.
    pub fn min_len(field: impl Into<String>, value: u64) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::MinLen,
            value: Value::from(value),
        }
    }

    /// `len(field) <= value`.
    pub fn max_len(field: impl Into<String>, value: u64) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::MaxLen,
            value: Value::from(value),
        }
    }

    /// A human-readable description of the clause (for logs and error messages).
    #[must_use]
    pub fn describe(&self) -> String {
        match self.op {
            ClauseOp::Present => format!("{} present", self.field),
            ClauseOp::NonEmpty => format!("{} non-empty", self.field),
            ClauseOp::Eq => format!("{} == {}", self.field, self.value),
            ClauseOp::Gte => format!("{} >= {}", self.field, self.value),
            ClauseOp::Lte => format!("{} <= {}", self.field, self.value),
            ClauseOp::MinLen => format!("len({}) >= {}", self.field, self.value),
            ClauseOp::MaxLen => format!("len({}) <= {}", self.field, self.value),
        }
    }

    /// Evaluates the clause against the given (object) value.
    ///
    /// Returns `false` if the field is missing, the type does not fit the
    /// operator, or the assertion does not hold.
    #[must_use]
    pub fn eval(&self, value: &Value) -> bool {
        let field = value.get(&self.field);
        match self.op {
            ClauseOp::Present => matches!(field, Some(v) if !v.is_null()),
            ClauseOp::NonEmpty => match field {
                Some(Value::String(s)) => !s.is_empty(),
                Some(Value::Array(a)) => !a.is_empty(),
                Some(Value::Object(o)) => !o.is_empty(),
                _ => false,
            },
            ClauseOp::Eq => field == Some(&self.value),
            ClauseOp::Gte => match (number(field), number(Some(&self.value))) {
                (Some(a), Some(b)) => a >= b,
                _ => false,
            },
            ClauseOp::Lte => match (number(field), number(Some(&self.value))) {
                (Some(a), Some(b)) => a <= b,
                _ => false,
            },
            ClauseOp::MinLen => match (length(field), self.value.as_u64()) {
                (Some(len), Some(min)) => len >= min,
                _ => false,
            },
            ClauseOp::MaxLen => match (length(field), self.value.as_u64()) {
                (Some(len), Some(max)) => len <= max,
                _ => false,
            },
        }
    }
}

/// Extracts a numeric value (f64) from a JSON value, if it is a number.
fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

/// Returns the field's length (string/list/object), if applicable.
fn length(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::String(s)) => Some(s.chars().count() as u64),
        Some(Value::Array(a)) => Some(a.len() as u64),
        Some(Value::Object(o)) => Some(o.len() as u64),
        _ => None,
    }
}

// ===========================================================================
// Capability and its registry
// ===========================================================================

/// A typed capability that a provider can advertise.
///
/// Contains an input/output schema plus pre- and postconditions.
/// Preconditions are checked when the contract is accepted; postconditions
/// when it is fulfilled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// The capability's stable identifier.
    pub id: MessageId,

    /// The capability's name (e.g. `"render_video"`).
    pub name: String,

    /// The input schema.
    pub input: Schema,

    /// The output schema.
    pub output: Schema,

    /// Preconditions (checked on acceptance, against the input).
    #[serde(default)]
    pub preconditions: Vec<Clause>,

    /// Postconditions (checked on fulfillment, against the deliverable).
    #[serde(default)]
    pub postconditions: Vec<Clause>,
}

impl Capability {
    /// Builds a capability with a name and schemas, without conditions.
    pub fn new(name: impl Into<String>, input: Schema, output: Schema) -> Self {
        Self {
            id: MessageId::new(),
            name: name.into(),
            input,
            output,
            preconditions: Vec::new(),
            postconditions: Vec::new(),
        }
    }

    /// Sets the preconditions (builder style).
    #[must_use]
    pub fn with_preconditions(mut self, clauses: Vec<Clause>) -> Self {
        self.preconditions = clauses;
        self
    }

    /// Sets the postconditions (builder style).
    #[must_use]
    pub fn with_postconditions(mut self, clauses: Vec<Clause>) -> Self {
        self.postconditions = clauses;
        self
    }
}

/// A thread-safe registry of advertised capabilities.
#[derive(Debug, Clone, Default)]
pub struct CapabilityRegistry {
    inner: Arc<RwLock<HashMap<MessageId, Capability>>>,
}

impl CapabilityRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Advertises (adds or replaces) a capability and returns its identifier.
    pub async fn advertise(&self, capability: Capability) -> MessageId {
        let id = capability.id;
        let mut guard = self.inner.write().await;
        guard.insert(id, capability);
        id
    }

    /// Looks up a capability by identifier.
    pub async fn get(&self, id: MessageId) -> Option<Capability> {
        let guard = self.inner.read().await;
        guard.get(&id).cloned()
    }

    /// Returns all capabilities with the given name, ordered by identifier.
    pub async fn find_by_name(&self, name: &str) -> Vec<Capability> {
        let guard = self.inner.read().await;
        let mut out: Vec<Capability> = guard.values().filter(|c| c.name == name).cloned().collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Number of registered capabilities.
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.len()
    }

    /// Whether the registry is empty.
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.is_empty()
    }
}

// ===========================================================================
// Contract status and deliverable
// ===========================================================================

/// A contract's status (state machine).
///
/// Allowed transitions:
/// - `Proposed → Accepted`, `Proposed → Rejected`
/// - `Accepted → Fulfilled`, `Accepted → Failed`
///
/// `Rejected`, `Fulfilled`, and `Failed` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    /// Proposed, awaiting acceptance/rejection.
    Proposed,
    /// Accepted, work in progress.
    Accepted,
    /// Rejected at the proposal stage (terminal).
    Rejected,
    /// Fulfilled: the deliverable passed the output schema and postconditions
    /// (terminal).
    Fulfilled,
    /// Failed: the deliverable breached a postcondition, or the provider
    /// reported an error (terminal).
    Failed,
}

impl ContractStatus {
    /// Whether the status is terminal.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ContractStatus::Rejected | ContractStatus::Fulfilled | ContractStatus::Failed
        )
    }

    /// Whether the transition `self → next` is allowed.
    #[must_use]
    pub fn can_transition_to(self, next: ContractStatus) -> bool {
        use ContractStatus::{Accepted, Failed, Fulfilled, Proposed, Rejected};
        matches!(
            (self, next),
            (Proposed, Accepted | Rejected) | (Accepted, Fulfilled | Failed)
        )
    }
}

/// A contract's deliverable (the provider's output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deliverable {
    /// The agent that produced the deliverable.
    pub from: AgentId,

    /// The deliverable's payload (checked against the output schema +
    /// postconditions).
    pub payload: Value,

    /// The delivery time (UTC, injected).
    pub at: Timestamp,
}

impl Deliverable {
    /// Builds a deliverable.
    #[must_use]
    pub fn new(from: AgentId, payload: Value, at: Timestamp) -> Self {
        Self { from, payload, at }
    }
}

/// A single contract between a requester and a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contract {
    /// The contract's stable identifier.
    pub id: MessageId,

    /// The capability underlying the contract (a copy from the time it was
    /// advertised).
    pub capability: Capability,

    /// The requester of the work.
    pub requester: AgentId,

    /// The provider of the work.
    pub provider: AgentId,

    /// The contract's input (validated against the capability's input
    /// schema).
    pub input: Value,

    /// The output schema the deliverable must conform to (a copy from the
    /// capability).
    pub output_schema: Schema,

    /// The postconditions the deliverable must satisfy (a copy from the
    /// capability).
    pub postconditions: Vec<Clause>,

    /// The contract's current status.
    pub status: ContractStatus,

    /// The deliverable, once fulfilled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliverable: Option<Deliverable>,

    /// A link to the orchestration task, if the contract originated from a
    /// workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<TaskId>,

    /// Creation time (UTC, injected).
    pub created_at: Timestamp,

    /// Time of the most recent change (UTC, injected).
    pub updated_at: Timestamp,
}

impl Contract {
    /// Links the contract to an orchestration task (builder style).
    #[must_use]
    pub fn with_link(mut self, task: TaskId) -> Self {
        self.link = Some(task);
        self
    }
}

// ===========================================================================
// Errors
// ===========================================================================

/// A contract operation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// The input did not pass the capability's input schema.
    InputSchemaViolation(Vec<SchemaViolation>),

    /// A precondition was not satisfied on acceptance.
    PreconditionFailed(String),

    /// The deliverable did not pass the output schema.
    OutputSchemaViolation(Vec<SchemaViolation>),

    /// The deliverable breached a postcondition.
    PostconditionBreach(String),

    /// An illegal state transition was attempted.
    IllegalTransition {
        /// The source state.
        from: ContractStatus,
        /// The attempted target state.
        to: ContractStatus,
    },

    /// The contract/capability was not found.
    NotFound(String),

    /// The contract was rejected with the given reason.
    Rejected(String),
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContractError::InputSchemaViolation(v) => {
                write!(f, "input schema violation: {}", join_violations(v))
            }
            ContractError::PreconditionFailed(c) => write!(f, "precondition failed: {c}"),
            ContractError::OutputSchemaViolation(v) => {
                write!(f, "output schema violation: {}", join_violations(v))
            }
            ContractError::PostconditionBreach(c) => write!(f, "postcondition breach: {c}"),
            ContractError::IllegalTransition { from, to } => {
                write!(f, "illegal contract transition: {from:?} -> {to:?}")
            }
            ContractError::NotFound(what) => write!(f, "contract not found: {what}"),
            ContractError::Rejected(reason) => write!(f, "contract rejected: {reason}"),
        }
    }
}

impl std::error::Error for ContractError {}

/// Joins violations into a readable string.
fn join_violations(v: &[SchemaViolation]) -> String {
    v.iter()
        .map(|x| format!("{}: {}", x.field, x.reason))
        .collect::<Vec<_>>()
        .join("; ")
}

impl From<ContractError> for FamilyClawError {
    /// Converts a contract error into the platform's centralized error type.
    ///
    /// `NotFound` maps to [`FamilyClawError::NotFound`]; all others
    /// (validation, condition, and transition errors) map to
    /// [`FamilyClawError::InvalidInput`], since they are input/state errors.
    fn from(err: ContractError) -> Self {
        match err {
            ContractError::NotFound(what) => FamilyClawError::not_found(what),
            other => FamilyClawError::invalid_input(other.to_string()),
        }
    }
}

/// The result type for a contract operation.
pub type ContractResult<T> = std::result::Result<T, ContractError>;

// ===========================================================================
// Contract board
// ===========================================================================

/// A thread-safe contract board.
///
/// Handles the contract lifecycle: propose → accept/reject →
/// fulfill/fail. [`fulfill`](Self::fulfill) is a **verifying** method: it runs
/// the output schema and every postcondition against the deliverable, and
/// only a full pass moves the contract to [`ContractStatus::Fulfilled`].
#[derive(Debug, Clone, Default)]
pub struct ContractBoard {
    inner: Arc<RwLock<HashMap<MessageId, Contract>>>,
}

impl ContractBoard {
    /// Creates an empty board.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Proposes a contract for a capability. Validates `input` against the
    /// capability's input schema.
    ///
    /// On success, creates a contract in the [`ContractStatus::Proposed`]
    /// state.
    ///
    /// # Errors
    /// [`ContractError::InputSchemaViolation`] if the input does not pass the
    /// capability's input schema.
    pub async fn propose(
        &self,
        capability: &Capability,
        requester: AgentId,
        provider: AgentId,
        input: Value,
        now: Timestamp,
    ) -> ContractResult<Contract> {
        let violations = capability.input.check(&input);
        if !violations.is_empty() {
            return Err(ContractError::InputSchemaViolation(violations));
        }
        let contract = Contract {
            id: MessageId::new(),
            capability: capability.clone(),
            requester,
            provider,
            input,
            output_schema: capability.output.clone(),
            postconditions: capability.postconditions.clone(),
            status: ContractStatus::Proposed,
            deliverable: None,
            link: None,
            created_at: now,
            updated_at: now,
        };
        let mut guard = self.inner.write().await;
        guard.insert(contract.id, contract.clone());
        Ok(contract)
    }

    /// Inserts an already-built contract onto the board (e.g. one linked to
    /// orchestration). Skips schema validation — the caller's responsibility.
    ///
    /// # Errors
    /// Never [`ContractError::NotFound`]; but if the same identifier is
    /// already on the board, the old entry is replaced (idempotent).
    pub async fn insert(&self, contract: Contract) {
        let mut guard = self.inner.write().await;
        guard.insert(contract.id, contract);
    }

    /// Accepts a proposed contract. Rechecks preconditions against the input.
    ///
    /// # Errors
    /// - [`ContractError::NotFound`] if the contract does not exist.
    /// - [`ContractError::IllegalTransition`] if the contract is not
    ///   `Proposed`.
    /// - [`ContractError::PreconditionFailed`] if a precondition is not
    ///   satisfied.
    pub async fn accept(&self, id: MessageId, now: Timestamp) -> ContractResult<Contract> {
        let mut guard = self.inner.write().await;
        let contract = guard
            .get_mut(&id)
            .ok_or_else(|| ContractError::NotFound(id.to_string()))?;

        if !contract.status.can_transition_to(ContractStatus::Accepted) {
            return Err(ContractError::IllegalTransition {
                from: contract.status,
                to: ContractStatus::Accepted,
            });
        }
        // Preconditions against the input.
        for clause in &contract.capability.preconditions {
            if !clause.eval(&contract.input) {
                return Err(ContractError::PreconditionFailed(clause.describe()));
            }
        }
        contract.status = ContractStatus::Accepted;
        contract.updated_at = now;
        Ok(contract.clone())
    }

    /// Rejects a proposed contract with the given reason.
    ///
    /// # Errors
    /// - [`ContractError::NotFound`] if the contract does not exist.
    /// - [`ContractError::IllegalTransition`] if the contract is not
    ///   `Proposed`.
    pub async fn reject(
        &self,
        id: MessageId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> ContractResult<Contract> {
        let mut guard = self.inner.write().await;
        let contract = guard
            .get_mut(&id)
            .ok_or_else(|| ContractError::NotFound(id.to_string()))?;
        if !contract.status.can_transition_to(ContractStatus::Rejected) {
            return Err(ContractError::IllegalTransition {
                from: contract.status,
                to: ContractStatus::Rejected,
            });
        }
        contract.status = ContractStatus::Rejected;
        contract.updated_at = now;
        let _ = reason; // the reason is recorded in the event/log, not in a field
        Ok(contract.clone())
    }

    /// **Verifying fulfillment.** Runs the output schema and every
    /// postcondition against the deliverable. Any violation → `Accepted →
    /// Failed` with a descriptive error. A full pass → `Accepted →
    /// Fulfilled`.
    ///
    /// # Errors
    /// - [`ContractError::NotFound`] if the contract does not exist.
    /// - [`ContractError::IllegalTransition`] if the contract is not
    ///   `Accepted`.
    /// - [`ContractError::OutputSchemaViolation`] if the deliverable breaches
    ///   the output schema (the contract moves to the `Failed` state).
    /// - [`ContractError::PostconditionBreach`] if a postcondition is not
    ///   satisfied (the contract moves to the `Failed` state).
    pub async fn fulfill(
        &self,
        id: MessageId,
        deliverable: Deliverable,
        now: Timestamp,
    ) -> ContractResult<Contract> {
        let mut guard = self.inner.write().await;
        let contract = guard
            .get_mut(&id)
            .ok_or_else(|| ContractError::NotFound(id.to_string()))?;

        if contract.status != ContractStatus::Accepted {
            return Err(ContractError::IllegalTransition {
                from: contract.status,
                to: ContractStatus::Fulfilled,
            });
        }

        // 1) Output schema.
        let violations = contract.output_schema.check(&deliverable.payload);
        if !violations.is_empty() {
            contract.status = ContractStatus::Failed;
            contract.deliverable = Some(deliverable);
            contract.updated_at = now;
            return Err(ContractError::OutputSchemaViolation(violations));
        }

        // 2) Every postcondition.
        for clause in &contract.postconditions {
            if !clause.eval(&deliverable.payload) {
                contract.status = ContractStatus::Failed;
                contract.deliverable = Some(deliverable.clone());
                contract.updated_at = now;
                return Err(ContractError::PostconditionBreach(clause.describe()));
            }
        }

        // Full pass.
        contract.status = ContractStatus::Fulfilled;
        contract.deliverable = Some(deliverable);
        contract.updated_at = now;
        Ok(contract.clone())
    }

    /// Marks an accepted contract as failed (the provider cannot deliver)
    /// with the given reason.
    ///
    /// # Errors
    /// - [`ContractError::NotFound`] if the contract does not exist.
    /// - [`ContractError::IllegalTransition`] if the contract is not
    ///   `Accepted`.
    pub async fn fail(
        &self,
        id: MessageId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> ContractResult<Contract> {
        let mut guard = self.inner.write().await;
        let contract = guard
            .get_mut(&id)
            .ok_or_else(|| ContractError::NotFound(id.to_string()))?;
        if !contract.status.can_transition_to(ContractStatus::Failed) {
            return Err(ContractError::IllegalTransition {
                from: contract.status,
                to: ContractStatus::Failed,
            });
        }
        contract.status = ContractStatus::Failed;
        contract.updated_at = now;
        let _ = reason;
        Ok(contract.clone())
    }

    /// Looks up a contract by identifier.
    pub async fn get(&self, id: MessageId) -> Option<Contract> {
        let guard = self.inner.read().await;
        guard.get(&id).cloned()
    }

    /// Lists all contracts, ordered by identifier.
    pub async fn list(&self) -> Vec<Contract> {
        let guard = self.inner.read().await;
        let mut out: Vec<Contract> = guard.values().cloned().collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Lists a given provider's contracts.
    pub async fn list_for_provider(&self, provider: AgentId) -> Vec<Contract> {
        let guard = self.inner.read().await;
        let mut out: Vec<Contract> = guard
            .values()
            .filter(|c| c.provider == provider)
            .cloned()
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Lists contracts in a given status.
    pub async fn list_by_status(&self, status: ContractStatus) -> Vec<Contract> {
        let guard = self.inner.read().await;
        let mut out: Vec<Contract> = guard
            .values()
            .filter(|c| c.status == status)
            .cloned()
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Lists contracts linked to a given orchestration task.
    pub async fn list_for_task(&self, task: TaskId) -> Vec<Contract> {
        let guard = self.inner.read().await;
        let mut out: Vec<Contract> = guard
            .values()
            .filter(|c| c.link == Some(task))
            .cloned()
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Number of contracts on the board.
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.len()
    }

    /// Whether the board is empty.
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time;
    use serde_json::json;

    fn ts(secs: i64) -> Timestamp {
        time::from_unix_secs(secs).expect("valid unix seconds")
    }

    fn render_capability() -> Capability {
        Capability::new(
            "render_video",
            Schema::new(vec![
                Field::required("script", FieldType::Str),
                Field::required("duration", FieldType::Int),
            ]),
            Schema::new(vec![
                Field::required("url", FieldType::Str),
                Field::required("frames", FieldType::Int),
            ]),
        )
        .with_preconditions(vec![Clause::gte("duration", json!(1))])
        .with_postconditions(vec![
            Clause::non_empty("url"),
            Clause::gte("frames", json!(1)),
        ])
    }

    // --- Schema.check ------------------------------------------------------

    #[test]
    fn schema_check_passes_valid_object() {
        let schema = Schema::new(vec![Field::required("a", FieldType::Str)]);
        assert!(schema.check(&json!({ "a": "x" })).is_empty());
    }

    #[test]
    fn schema_check_reports_missing_required() {
        let schema = Schema::new(vec![Field::required("a", FieldType::Str)]);
        let v = schema.check(&json!({}));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].field, "a");
    }

    #[test]
    fn schema_check_reports_wrong_type() {
        let schema = Schema::new(vec![Field::required("n", FieldType::Int)]);
        let v = schema.check(&json!({ "n": "not a number" }));
        assert_eq!(v.len(), 1);
        assert!(v[0].reason.contains("number"));
    }

    #[test]
    fn schema_check_optional_absent_ok_but_present_typechecked() {
        let schema = Schema::new(vec![Field::optional("o", FieldType::Bool)]);
        assert!(schema.check(&json!({})).is_empty());
        assert!(!schema.check(&json!({ "o": "x" })).is_empty());
        assert!(schema.check(&json!({ "o": true })).is_empty());
    }

    #[test]
    fn schema_check_non_object_is_root_violation() {
        let schema = Schema::empty();
        let v = schema.check(&json!("a string"));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].field, "$root");
    }

    // --- Clause truth table ------------------------------------------------

    #[test]
    fn clause_present_truth_table() {
        let c = Clause::present("x");
        assert!(c.eval(&json!({ "x": 1 })));
        assert!(!c.eval(&json!({ "x": null })));
        assert!(!c.eval(&json!({})));
    }

    #[test]
    fn clause_non_empty_truth_table() {
        let c = Clause::non_empty("x");
        assert!(c.eval(&json!({ "x": "a" })));
        assert!(c.eval(&json!({ "x": [1] })));
        assert!(c.eval(&json!({ "x": { "k": 1 } })));
        assert!(!c.eval(&json!({ "x": "" })));
        assert!(!c.eval(&json!({ "x": [] })));
        assert!(!c.eval(&json!({ "x": 5 }))); // a number has no notion of "emptiness"
    }

    #[test]
    fn clause_eq_truth_table() {
        let c = Clause::eq("x", json!("ok"));
        assert!(c.eval(&json!({ "x": "ok" })));
        assert!(!c.eval(&json!({ "x": "no" })));
        assert!(!c.eval(&json!({})));
    }

    #[test]
    fn clause_gte_lte_truth_table() {
        let gte = Clause::gte("n", json!(10));
        assert!(gte.eval(&json!({ "n": 10 })));
        assert!(gte.eval(&json!({ "n": 11 })));
        assert!(!gte.eval(&json!({ "n": 9 })));
        assert!(!gte.eval(&json!({ "n": "x" })));

        let lte = Clause::lte("n", json!(10));
        assert!(lte.eval(&json!({ "n": 10 })));
        assert!(lte.eval(&json!({ "n": 9 })));
        assert!(!lte.eval(&json!({ "n": 11 })));
    }

    #[test]
    fn clause_min_max_len_truth_table() {
        let min = Clause::min_len("s", 3);
        assert!(min.eval(&json!({ "s": "abc" })));
        assert!(min.eval(&json!({ "s": [1, 2, 3, 4] })));
        assert!(!min.eval(&json!({ "s": "ab" })));

        let max = Clause::max_len("s", 3);
        assert!(max.eval(&json!({ "s": "abc" })));
        assert!(!max.eval(&json!({ "s": "abcd" })));
        assert!(!max.eval(&json!({ "s": 5 }))); // numerolla ei pituutta
    }

    #[test]
    fn clause_describe_is_readable() {
        assert_eq!(Clause::present("x").describe(), "x present");
        assert_eq!(Clause::min_len("y", 2).describe(), "len(y) >= 2");
    }

    // --- ContractStatus matrix ---------------------------------------------

    #[test]
    fn contract_status_transition_matrix() {
        use ContractStatus::{Accepted, Failed, Fulfilled, Proposed, Rejected};
        assert!(Proposed.can_transition_to(Accepted));
        assert!(Proposed.can_transition_to(Rejected));
        assert!(Accepted.can_transition_to(Fulfilled));
        assert!(Accepted.can_transition_to(Failed));

        // Laittomat.
        assert!(!Proposed.can_transition_to(Fulfilled));
        assert!(!Accepted.can_transition_to(Rejected));
        assert!(!Rejected.can_transition_to(Accepted));
        assert!(!Fulfilled.can_transition_to(Failed));
        assert!(!Failed.can_transition_to(Fulfilled));
    }

    #[test]
    fn contract_status_terminality() {
        assert!(ContractStatus::Rejected.is_terminal());
        assert!(ContractStatus::Fulfilled.is_terminal());
        assert!(ContractStatus::Failed.is_terminal());
        assert!(!ContractStatus::Proposed.is_terminal());
        assert!(!ContractStatus::Accepted.is_terminal());
    }

    // --- ContractBoard flow -------------------------------------------------

    #[tokio::test]
    async fn propose_rejects_bad_input_schema() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let err = board
            .propose(
                &cap,
                AgentId::new(),
                AgentId::new(),
                json!({ "script": "s" }),
                ts(1),
            )
            .await
            .expect_err("missing duration");
        assert!(matches!(err, ContractError::InputSchemaViolation(_)));
    }

    #[tokio::test]
    async fn propose_accept_fulfill_happy_path() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "hello", "duration": 5 }),
                ts(1),
            )
            .await
            .expect("propose");
        assert_eq!(c.status, ContractStatus::Proposed);

        let accepted = board.accept(c.id, ts(2)).await.expect("accept");
        assert_eq!(accepted.status, ContractStatus::Accepted);

        let deliverable = Deliverable::new(
            provider,
            json!({ "url": "https://x/v.mp4", "frames": 120 }),
            ts(3),
        );
        let fulfilled = board
            .fulfill(c.id, deliverable, ts(3))
            .await
            .expect("fulfill");
        assert_eq!(fulfilled.status, ContractStatus::Fulfilled);
        assert!(fulfilled.deliverable.is_some());
    }

    #[tokio::test]
    async fn fulfill_breaches_output_schema_sets_failed() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "s", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("propose");
        board.accept(c.id, ts(2)).await.expect("accept");

        // Toimite: "frames" puuttuu → tulosskeema rikkoutuu.
        let bad = Deliverable::new(provider, json!({ "url": "https://x" }), ts(3));
        let err = board
            .fulfill(c.id, bad, ts(3))
            .await
            .expect_err("schema breach");
        assert!(matches!(err, ContractError::OutputSchemaViolation(_)));

        let after = board.get(c.id).await.expect("present");
        assert_eq!(after.status, ContractStatus::Failed);
    }

    #[tokio::test]
    async fn fulfill_breaches_postcondition_sets_failed() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "s", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("propose");
        board.accept(c.id, ts(2)).await.expect("accept");

        // Schema OK (url is a string, frames is a number) but the postcondition
        // `non_empty(url)` is breached (empty) and `frames >= 1` is breached (0).
        let bad = Deliverable::new(provider, json!({ "url": "", "frames": 0 }), ts(3));
        let err = board
            .fulfill(c.id, bad, ts(3))
            .await
            .expect_err("postcondition");
        assert!(matches!(err, ContractError::PostconditionBreach(_)));
        let after = board.get(c.id).await.expect("present");
        assert_eq!(after.status, ContractStatus::Failed);
    }

    #[tokio::test]
    async fn accept_rechecks_preconditions() {
        // The precondition duration>=1 is not satisfied if the input bypassed
        // schema validation via another path. Here propose accepts duration=0
        // (the schema only requires a number), but accept rejects the precondition.
        let board = ContractBoard::new();
        let cap = render_capability();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                AgentId::new(),
                json!({ "script": "s", "duration": 0 }),
                ts(1),
            )
            .await
            .expect("propose");
        let err = board.accept(c.id, ts(2)).await.expect_err("precondition");
        assert!(matches!(err, ContractError::PreconditionFailed(_)));
    }

    #[tokio::test]
    async fn reject_only_from_proposed() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                AgentId::new(),
                json!({ "script": "s", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("propose");
        let rejected = board.reject(c.id, "too busy", ts(2)).await.expect("reject");
        assert_eq!(rejected.status, ContractStatus::Rejected);

        // A second reject → illegal transition.
        let err = board
            .reject(c.id, "again", ts(3))
            .await
            .expect_err("terminal");
        assert!(matches!(err, ContractError::IllegalTransition { .. }));
    }

    #[tokio::test]
    async fn fulfill_requires_accepted() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "s", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("propose");
        // Try to fulfill directly from the Proposed state → illegal.
        let d = Deliverable::new(provider, json!({ "url": "u", "frames": 1 }), ts(2));
        let err = board
            .fulfill(c.id, d, ts(2))
            .await
            .expect_err("not accepted");
        assert!(matches!(err, ContractError::IllegalTransition { .. }));
    }

    #[tokio::test]
    async fn queries_filter_correctly() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let other = AgentId::new();
        let task = TaskId::new();

        let c1 = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "a", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("c1");
        let mut linked = c1.clone();
        linked.link = Some(task);
        board.insert(linked).await;

        let _c2 = board
            .propose(
                &cap,
                AgentId::new(),
                other,
                json!({ "script": "b", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("c2");

        assert_eq!(board.len().await, 2);
        assert_eq!(board.list_for_provider(provider).await.len(), 1);
        assert_eq!(board.list_for_provider(other).await.len(), 1);
        assert_eq!(
            board.list_by_status(ContractStatus::Proposed).await.len(),
            2
        );
        assert_eq!(board.list_for_task(task).await.len(), 1);
    }

    #[tokio::test]
    async fn capability_registry_advertise_and_find() {
        let reg = CapabilityRegistry::new();
        assert!(reg.is_empty().await);
        let cap = render_capability();
        let id = reg.advertise(cap.clone()).await;
        assert_eq!(reg.len().await, 1);
        assert_eq!(
            reg.get(id).await.map(|c| c.name),
            Some("render_video".into())
        );
        assert_eq!(reg.find_by_name("render_video").await.len(), 1);
        assert!(reg.find_by_name("nope").await.is_empty());
    }

    #[tokio::test]
    async fn contract_error_converts_to_familyclaw_error() {
        let nf: FamilyClawError = ContractError::NotFound("x".into()).into();
        assert!(matches!(nf, FamilyClawError::NotFound(_)));
        let bad: FamilyClawError =
            ContractError::PostconditionBreach("len(url) >= 1".into()).into();
        assert!(matches!(bad, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn contract_serde_roundtrip() {
        let cap = render_capability();
        let c = Contract {
            id: MessageId::new(),
            capability: cap.clone(),
            requester: AgentId::new(),
            provider: AgentId::new(),
            input: json!({ "script": "s", "duration": 2 }),
            output_schema: cap.output.clone(),
            postconditions: cap.postconditions.clone(),
            status: ContractStatus::Proposed,
            deliverable: None,
            link: None,
            created_at: ts(1),
            updated_at: ts(1),
        };
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Contract = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }
}
