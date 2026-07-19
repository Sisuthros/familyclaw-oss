//! MCP adapter: describes the action stack's skills as MCP tools and routes
//! tool calls through a capability check (Layer A).
//!
//! This module is intentionally an **interface**, not a full MCP server: it
//! defines how skills are presented as MCP tools ([`McpToolDescriptor`]), how
//! a tool is called ([`McpToolCall`]) and what it returns
//! ([`McpToolResult`]), plus a policy gate ([`call_with_policy`]) that:
//! - rejects an unknown tool ([`ActionError::McpUnknownTool`]),
//! - rejects a call if the required permission is missing from the granted
//!   set ([`ActionError::McpDenied`]) and records the denial to the audit log,
//! - marks the output as untrusted (taint) **unless** the tool's source is
//!   explicitly trusted ([`McpToolDescriptor::trusted`]).
//!
//! ## OSS boundary (Layer A)
//! Providers are **mocks** ([`MockMcpProvider`]) — no real network calls, no
//! providers, no personas, and no keys. The output is untrusted by default,
//! just as in the execution layer ([`crate::executor`]), until the source is
//! established as trusted.
//!
//! ## Determinism
//! The policy gate takes the timestamp injected
//! ([`familyclaw_core::time::Timestamp`]) — the clock is never read inside
//! the logic, so audit events are deterministic in tests and replay.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use familyclaw_core::time::Timestamp;

use crate::audit::{AuditCollector, AuditKind, ExecAuditEvent};
use crate::error::{ActionError, Result};
use crate::ids::ActionId;
use crate::policy::SkillPermission;

/// Module readiness flag — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside the other modules.
pub(crate) const SCAFFOLDED: bool = true;

/// The description of a single MCP tool: what the provider publishes to the client.
///
/// The descriptor states the tool's name and description, its input schema
/// (a generic JSON schema as a value), the permission the tool requires, and
/// whether the tool's source is trusted. A trusted source produces trusted
/// data; otherwise the output is marked as untrusted (taint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    /// The tool's unique name (e.g. `echo`). Used for routing.
    pub name: String,
    /// A short human-readable description of what the tool does.
    pub description: String,
    /// The tool's input schema as a generic JSON value (e.g. a JSON schema).
    pub input_schema: Value,
    /// The permission the caller must have before the tool may be called.
    pub required_permission: SkillPermission,
    /// Whether the tool's source is trusted. If `true`, the output does not
    /// get the taint marker; if `false`, the output is marked as untrusted.
    pub trusted: bool,
}

impl McpToolDescriptor {
    /// Builds a new tool descriptor.
    ///
    /// The source is marked **untrusted** by default (`trusted = false`);
    /// trust must be raised explicitly via [`McpToolDescriptor::trust`].
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        required_permission: SkillPermission,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            required_permission,
            trusted: false,
        }
    }

    /// Marks the tool's source as trusted (the output does not get the taint marker).
    ///
    /// Use only when the source has been explicitly established as trusted.
    #[must_use]
    pub fn trust(mut self) -> Self {
        self.trusted = true;
        self
    }
}

/// A single MCP tool call request: which tool and with what input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolCall {
    /// The name of the tool to call (must match [`McpToolDescriptor::name`]).
    pub tool: String,
    /// The input passed to the tool as a generic JSON value.
    pub input: Value,
}

impl McpToolCall {
    /// Builds a new tool call.
    #[must_use]
    pub fn new(tool: impl Into<String>, input: Value) -> Self {
        Self {
            tool: tool.into(),
            input,
        }
    }
}

/// The result of a single MCP tool call.
///
/// `untrusted` reports whether the output originates from an untrusted
/// source (taint). A provider's own result is untrusted by default; the
/// policy gate ([`call_with_policy`]) clears the flag only if the tool
/// descriptor's source is trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolResult {
    /// The output produced by the tool as a generic JSON value.
    pub output: Value,
    /// Whether the output originates from an untrusted source (taint).
    pub untrusted: bool,
}

impl McpToolResult {
    /// Builds a new result marked as untrusted (the default taint).
    #[must_use]
    pub fn untrusted(output: Value) -> Self {
        Self {
            output,
            untrusted: true,
        }
    }

    /// Builds a new result marked as trusted (no taint marker).
    #[must_use]
    pub fn trusted(output: Value) -> Self {
        Self {
            output,
            untrusted: false,
        }
    }
}

/// A provider of MCP tools.
///
/// An implementation publishes a set of tools ([`McpToolProvider::describe`])
/// and runs a single tool call ([`McpToolProvider::call`]). Layer A
/// implementations are **mocks** — no real network calls.
#[async_trait]
pub trait McpToolProvider: Send + Sync {
    /// Returns all tool descriptors the provider publishes.
    async fn describe(&self) -> Vec<McpToolDescriptor>;

    /// Runs a single tool call and returns the result.
    ///
    /// # Errors
    /// Returns [`ActionError::McpUnknownTool`] if the tool does not exist,
    /// and other [`ActionError`] variants if execution cannot start. It is
    /// recommended to route calls through [`call_with_policy`], which
    /// performs the permission and taint checks.
    async fn call(&self, call: McpToolCall) -> Result<McpToolResult>;
}

/// A single mock tool's behavior: descriptor + a canned result.
#[derive(Debug, Clone)]
struct MockTool {
    /// The descriptor the provider publishes for this tool.
    descriptor: McpToolDescriptor,
    /// A fixed result the tool returns (mock — no network call).
    /// `None` means "echo the input back" (e.g. the `echo` tool).
    canned: Option<Value>,
}

/// A test-oriented MCP provider with an in-memory tool registry.
///
/// By default the registry contains two generic mock tools:
/// - `echo` — echoes the input back as output (untrusted source),
/// - `fetch_mock` — returns a fixed canned result (untrusted source).
///
/// Additional tools can be registered via [`MockMcpProvider::with_tool`]. No
/// mock makes network calls (Layer A).
#[derive(Debug, Clone, Default)]
pub struct MockMcpProvider {
    /// Tools keyed by name (stable, deterministic order).
    tools: BTreeMap<String, MockTool>,
}

impl MockMcpProvider {
    /// Creates an empty provider with no tools.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Creates a provider with the default tools (`echo`, `fetch_mock`).
    ///
    /// The default tools require the [`SkillPermission::NetworkRead`]
    /// permission and their source is untrusted (the output is tainted
    /// unless the caller separately marks the tool as trusted).
    #[must_use]
    pub fn with_defaults() -> Self {
        let echo = McpToolDescriptor::new(
            "echo",
            "Kaiuttaa syötteen takaisin sellaisenaan.",
            serde_json::json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        );
        let fetch = McpToolDescriptor::new(
            "fetch_mock",
            "Palauttaa kiinteän valmiin tuloksen (mock-haku).",
            serde_json::json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        );
        Self::empty().with_tool(echo, None).with_tool(
            fetch,
            Some(serde_json::json!({ "status": "ok", "items": [] })),
        )
    }

    /// Registers a tool with a descriptor and a canned result.
    ///
    /// If `canned` is `None`, the tool echoes the call's input back as
    /// output. The same name replaces a prior registration.
    #[must_use]
    pub fn with_tool(mut self, descriptor: McpToolDescriptor, canned: Option<Value>) -> Self {
        let name = descriptor.name.clone();
        self.tools.insert(name, MockTool { descriptor, canned });
        self
    }

    /// Looks up a tool's descriptor by name (if registered).
    #[must_use]
    pub fn descriptor(&self, name: &str) -> Option<&McpToolDescriptor> {
        self.tools.get(name).map(|t| &t.descriptor)
    }
}

#[async_trait]
impl McpToolProvider for MockMcpProvider {
    async fn describe(&self) -> Vec<McpToolDescriptor> {
        self.tools.values().map(|t| t.descriptor.clone()).collect()
    }

    async fn call(&self, call: McpToolCall) -> Result<McpToolResult> {
        let Some(tool) = self.tools.get(&call.tool) else {
            return Err(ActionError::McpUnknownTool(call.tool));
        };
        // Mock: either echo the input or return a canned result. Always
        // untrusted as a source; the policy gate decides the final taint
        // state based on the descriptor's `trusted` flag.
        let output = tool.canned.clone().unwrap_or(call.input);
        Ok(McpToolResult::untrusted(output))
    }
}

/// Routes a tool call through the policy gate: permission check, audit
/// logging, and taint marking.
///
/// Steps:
/// 1. **Unknown tool** → [`ActionError::McpUnknownTool`] (no audit entry: the
///    call never existed).
/// 2. **Missing permission** → [`ActionError::McpDenied`] and an
///    [`AuditKind::PolicyDenied`] event is recorded.
/// 3. **Allowed** → the provider runs the call. The output is marked
///    untrusted ([`AuditKind::TaintMarked`]) **unless** the descriptor's
///    source is trusted; with a trusted source the flag is cleared.
///
/// `action_id` binds audit events to this call, `now` is the injected
/// timestamp (never read from the clock), `audit` collects the events.
///
/// # Errors
/// Returns [`ActionError::McpUnknownTool`] for an unknown tool,
/// [`ActionError::McpDenied`] when the required permission is missing, and
/// otherwise propagates the provider's returned error if execution fails.
pub async fn call_with_policy<P: McpToolProvider + ?Sized>(
    provider: &P,
    granted_permissions: &[SkillPermission],
    call: McpToolCall,
    now: Timestamp,
    audit: &AuditCollector,
    action_id: ActionId,
) -> Result<McpToolResult> {
    // 1. Look up the tool descriptor. An unknown tool is rejected before anything else.
    let descriptors = provider.describe().await;
    let Some(descriptor) = descriptors.into_iter().find(|d| d.name == call.tool) else {
        return Err(ActionError::McpUnknownTool(call.tool));
    };

    // 2. Permission check: the required permission must be in the granted set.
    if !granted_permissions.contains(&descriptor.required_permission) {
        audit.record(ExecAuditEvent::new(
            AuditKind::PolicyDenied,
            action_id,
            now,
            format!(
                "mcp tool '{}' denied: missing required permission",
                descriptor.name
            ),
        ));
        return Err(ActionError::McpDenied(format!(
            "tool '{}' requires a permission not in the granted set",
            descriptor.name
        )));
    }

    // 3. Allowed — run the call with the provider.
    let result = provider.call(call).await?;

    // 4. Taint decision: a trusted source clears the flag, otherwise the output is tainted.
    if descriptor.trusted {
        Ok(McpToolResult::trusted(result.output))
    } else {
        audit.record(ExecAuditEvent::new(
            AuditKind::TaintMarked,
            action_id,
            now,
            format!("mcp tool '{}' output marked untrusted", descriptor.name),
        ));
        Ok(McpToolResult::untrusted(result.output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    /// Helper: an injected timestamp for tests.
    fn ts() -> Timestamp {
        from_unix_secs(1_700_000_000).expect("valid unix seconds")
    }

    #[tokio::test]
    async fn defaults_register_echo_and_fetch() {
        let provider = MockMcpProvider::with_defaults();
        let described = provider.describe().await;
        let names: Vec<&str> = described.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"fetch_mock"));
        // The default tools are untrusted as sources.
        assert!(described.iter().all(|d| !d.trusted));
    }

    #[tokio::test]
    async fn registered_tool_callable_through_policy_ok_and_untrusted_by_default() {
        let provider = MockMcpProvider::with_defaults();
        let audit = AuditCollector::new();
        let action_id = ActionId::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("echo", json!({ "user": "agent_a", "msg": "hi" }));
        let result = call_with_policy(&provider, &granted, call, ts(), &audit, action_id)
            .await
            .expect("granted permission allows call");

        // echo echoes the input back.
        assert_eq!(result.output, json!({ "user": "agent_a", "msg": "hi" }));
        // Untrusted (taint) by default, because the source is not trusted.
        assert!(result.untrusted);
        // The taint marker was recorded to the audit log.
        assert!(audit
            .list()
            .iter()
            .any(|e| e.kind == AuditKind::TaintMarked && e.action_id == action_id));
    }

    #[tokio::test]
    async fn fetch_mock_returns_canned_result() {
        let provider = MockMcpProvider::with_defaults();
        let audit = AuditCollector::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("fetch_mock", json!({ "query": "general" }));
        let result = call_with_policy(&provider, &granted, call, ts(), &audit, ActionId::new())
            .await
            .expect("fetch_mock allowed");
        assert_eq!(result.output, json!({ "status": "ok", "items": [] }));
        assert!(result.untrusted);
    }

    #[tokio::test]
    async fn unknown_tool_rejected() {
        let provider = MockMcpProvider::with_defaults();
        let audit = AuditCollector::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("does_not_exist", json!({}));
        let err = call_with_policy(&provider, &granted, call, ts(), &audit, ActionId::new())
            .await
            .expect_err("unknown tool must be rejected");
        assert!(matches!(err, ActionError::McpUnknownTool(_)));
        // An unknown tool produces no audit event.
        assert!(audit.is_empty());
    }

    #[tokio::test]
    async fn denied_permission_blocks_call_and_records_audit() {
        let provider = MockMcpProvider::with_defaults();
        let audit = AuditCollector::new();
        let action_id = ActionId::new();
        // Grant the WRONG permission (the tool requires NetworkRead).
        let granted = [SkillPermission::ReadFiles];

        let call = McpToolCall::new("echo", json!({ "user": "agent_a" }));
        let err = call_with_policy(&provider, &granted, call, ts(), &audit, action_id)
            .await
            .expect_err("missing permission must block");
        assert!(matches!(err, ActionError::McpDenied(_)));

        // The denial was recorded to the audit log.
        let events = audit.list();
        assert!(events
            .iter()
            .any(|e| e.kind == AuditKind::PolicyDenied && e.action_id == action_id));
        // And no taint event occurs when the call was blocked.
        assert!(!events.iter().any(|e| e.kind == AuditKind::TaintMarked));
    }

    #[tokio::test]
    async fn trusted_source_output_is_not_tainted() {
        // Register a trusted tool with a fixed result.
        let trusted_tool = McpToolDescriptor::new(
            "trusted_lookup",
            "Luotettu sisäinen haku.",
            json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        )
        .trust();
        let provider =
            MockMcpProvider::empty().with_tool(trusted_tool, Some(json!({ "result": "general" })));
        let audit = AuditCollector::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("trusted_lookup", json!({ "q": "x" }));
        let result = call_with_policy(&provider, &granted, call, ts(), &audit, ActionId::new())
            .await
            .expect("trusted tool allowed");

        // Trusted source → no taint marker.
        assert!(!result.untrusted);
        assert_eq!(result.output, json!({ "result": "general" }));
        // And no taint event is recorded for a trusted source.
        assert!(!audit
            .list()
            .iter()
            .any(|e| e.kind == AuditKind::TaintMarked));
    }

    #[tokio::test]
    async fn secret_looking_output_passes_through_call_result_for_proof_redaction() {
        // The provider does not redact by itself — redaction happens in the
        // proof bundle. This only verifies that a secret-looking value
        // passes through in the result without a source literal (Layer B audit).
        let fake = format!("sk-{}", "live".repeat(4));
        let tool = McpToolDescriptor::new(
            "leaky_mock",
            "Palauttaa salaisuudelta näyttävän arvon (taintataan).",
            json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        );
        let provider =
            MockMcpProvider::empty().with_tool(tool, Some(json!({ "blob": fake.clone() })));
        let audit = AuditCollector::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("leaky_mock", json!({}));
        let result = call_with_policy(&provider, &granted, call, ts(), &audit, ActionId::new())
            .await
            .expect("call allowed");
        // Untrusted source → taint set (redaction happens at the proof layer).
        assert!(result.untrusted);
        assert_eq!(result.output, json!({ "blob": fake }));
    }

    #[tokio::test]
    async fn provider_call_directly_rejects_unknown_tool() {
        let provider = MockMcpProvider::with_defaults();
        let err = provider
            .call(McpToolCall::new("nope", json!({})))
            .await
            .expect_err("unknown tool rejected at provider level");
        assert!(matches!(err, ActionError::McpUnknownTool(_)));
    }

    #[test]
    fn descriptor_and_call_serde_roundtrip() {
        let desc = McpToolDescriptor::new(
            "echo",
            "Kaiuttaa.",
            json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        );
        let json_str = serde_json::to_string(&desc).expect("serialize descriptor");
        let back: McpToolDescriptor = serde_json::from_str(&json_str).expect("deserialize");
        assert_eq!(desc, back);

        let call = McpToolCall::new("echo", json!({ "x": 1 }));
        let back_call: McpToolCall =
            serde_json::from_str(&serde_json::to_string(&call).expect("ser")).expect("de");
        assert_eq!(call, back_call);
    }
}
