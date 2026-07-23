//! # familyclaw-gateway
//!
//! **Gateway binary** — the long-lived process of the `FamilyClaw` platform
//! (Layer A, OSS): it binds an HTTP port, provides liveness and readiness
//! checks (`/healthz`, `/readyz`) and Prometheus metrics (`/metrics`), starts
//! [`FamilyRuntime`] (bus + agent + channel + reply pump) with a single
//! [`build_family`] call, and stays up until the user requests a clean
//! shutdown (`Ctrl-C`).
//!
//! ## Observability: `GET /metrics` (Prometheus exposition text format)
//! The gateway shares a [`MetricsRegistry`] (built with
//! [`MetricsRegistry::with_fleet_defaults`]) in its `GatewayState`, and
//! serves it on `GET /metrics` as a `text/plain` response
//! ([`MetricsRegistry::prometheus_export`], deterministic name ordering).
//! The fleet's pre-named series (created/completed tasks, contracts,
//! agent turns, LLM calls, the `agents_online` gauge, …) are present in the
//! export from the start with the value `0`. **Event-driven population is
//! WIRED UP:** [`serve`] subscribes to the bridge-layer event bus
//! ([`FamilyBridge`]) with an [`EventRecorder`]
//! and gives the recorder and `GatewayState` the SAME [`MetricsRegistry`].
//! Runtime events therefore increment the series the recorder maps:
//! agent registration bumps the `agents_online` gauge right at startup, and
//! the bridge layer's task/contract/LLM events
//! (`task.*`, `contract.*`, `llm.*`, `agent.turn`, `workflow.*`) increment
//! the corresponding counters. Series for which no event is produced stay at zero.
//! The route is unprotected — metrics are numeric time series with no
//! secrets (see [`metrics_handler`]).
//!
//! ```bash
//! curl -s http://127.0.0.1:8787/metrics
//! # → # TYPE agents_online gauge
//! #   agents_online 1          # the agent was registered at startup
//! #   # TYPE tasks_created counter
//! #   tasks_created 0          # rises when the bridge layer creates tasks
//! #   ...
//! ```
//!
//! This is the **thin shell** around the `build_family` composer promised at
//! the C5 seam: [`build_family`] (`FamilyRuntime`) replaces the former direct
//! `ResonanceBus::start` call with a **single** call. The HTTP/shutdown shell
//! stayed unchanged — the bus handle is handed off to `GatewayState`, and
//! `Ctrl-C` triggers [`FamilyRuntime::shutdown`] (instead of the former
//! `bus.stop()`).
//!
//! ## OSS boundary (Layer A)
//! No hardcoded operator names, keys, or paths. **All** runtime configuration
//! is read from the environment (Layer B):
//! - `FAMILYCLAW_GATEWAY_ADDR` — listen address (default `127.0.0.1:8787`),
//! - `FAMILYCLAW_AGENT_NAME` — the agent's display name (default `agent_a`),
//! - `FAMILYCLAW_AGENT_MODEL` — `"provider/model"` (default `provider/model`),
//! - `FAMILYCLAW_PROFILE_DIR` — root of the soul profile directory (optional),
//! - `FAMILYCLAW_TELEGRAM_CHANNEL_ID` — Telegram channel instance identifier,
//! - `FAMILYCLAW_REPLY_TARGET` — static reply target (Telegram chat id),
//! - `FAMILYCLAW_GATEWAY_TOKEN` — optional bearer token that protects
//!   `POST /inject` (when set, the request requires `Authorization: Bearer <token>`;
//!   when empty, the endpoint stays loopback-only-open as before),
//! - `TELEGRAM_BOT_TOKEN` — Telegram bot token (required for the channel),
//! - `FAMILYCLAW_PROVIDERS` — provider table for the resolver, format
//!   `prefix=base_url=KEY_ENV` separated by semicolons (optional; without
//!   this the agent runs without an LLM).
//!
//! ## Running
//! ```bash
//! TELEGRAM_BOT_TOKEN=... \
//! FAMILYCLAW_TELEGRAM_CHANNEL_ID=tg-main \
//! FAMILYCLAW_REPLY_TARGET=123456789 \
//! cargo run -p familyclaw-gateway
//! # second terminal:
//! curl -i http://127.0.0.1:8787/healthz   # 200 OK
//! curl -i http://127.0.0.1:8787/readyz    # 200 OK (bus running)
//! ```
//!
//! ## Operator approval surface (suspend/resume bridge, roadmap §6 D2)
//! When the agent's tool loop suspends to wait for human approval
//! ([`ThinkOutcome::Suspended`](familyclaw_agent::ThinkOutcome::Suspended)),
//! NO reply is sent to the user — the suspension is the **operator's**
//! concern. The gateway provides two bearer-protected routes (same
//! [`GATEWAY_TOKEN_ENV`] token as `/inject`):
//! - `GET /approvals/pending` — lists pending approvals **redacted**
//!   (`approval_id`, `redacted_summary`, `created_at`) — never the raw
//!   payload or secrets.
//! - `POST /approvals/{approval_id}/approve` — grants the approval and runs
//!   the suspended action to completion (payload-bound, single-use).
//!
//! ```bash
//! TOKEN=...   # FAMILYCLAW_GATEWAY_TOKEN
//! curl -s -H "Authorization: Bearer $TOKEN" \
//!   http://127.0.0.1:8787/approvals/pending
//! # → [{"approval_id":"…","redacted_summary":"skill '…' awaiting …","created_at":"…"}]
//! curl -s -X POST -H "Authorization: Bearer $TOKEN" \
//!   http://127.0.0.1:8787/approvals/<approval_id>/approve
//! # → {"approval_id":"…","task_id":"…","status":"done","awaiting_further_approval":false}
//! ```

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::Router;
use clap::{Parser, Subcommand};
use familyclaw_actions::{ActionRuntime, ApprovalId, AuditCollector};
use familyclaw_agent::{resolve_profile_dir, EnvEndpointResolver, LiveTurnExecutor, Soul};
use familyclaw_bridge::{
    AgentInfo, AgentRole, FamilyBridge, HostKind, OrchestrationPlan, Orchestrator, TaskNode,
};
use familyclaw_bus::{BeingId, BusHandle, BusMessage};
use tokio::sync::Mutex;
mod config;
mod console;
mod readiness;
use config::FamilyConfig;
use familyclaw_channels::{
    verify_signature, Channel, ChannelKind, ChannelResult, DiscordChannel, DiscordInteraction,
    InboundMessage, MessageStream, OutboundMessage, SendFuture, TelegramChannel,
    RESPONSE_DEFERRED_CHANNEL_MESSAGE, RESPONSE_PONG,
};
use familyclaw_core::{AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_observability::{
    operator_acl::caps as operator_caps, EventRecorder, MetricsRegistry, OperatorAcl, OperatorRole,
};
use familyclaw_runtime::{build_family, AgentBuildSpec, FamilyRuntime};
use familyclaw_scheduler::{AgencyConfig, ScheduledTaskId, SchedulerHandle};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// Environment variable that sets the gateway's listen address.
const ADDR_ENV: &str = "FAMILYCLAW_GATEWAY_ADDR";

/// Telegram bot token (env). Required when the channel is wired up.
/// (Other env vars are served via `FamilyConfig` — see `config.rs`.)
const TELEGRAM_TOKEN_ENV: &str = "TELEGRAM_BOT_TOKEN";

/// Provider table for the resolver (env). Format: `prefix=base_url=KEY_ENV` separated by `;`.
const PROVIDERS_ENV: &str = "FAMILYCLAW_PROVIDERS";

/// Env names used in error messages (not read directly — `FamilyConfig` handles that)
const DISCORD_WEBHOOK_URL_ENV: &str = "DISCORD_WEBHOOK_URL";
const DISCORD_BOT_TOKEN_ENV: &str = "DISCORD_BOT_TOKEN";
const DISCORD_PUBLIC_KEY_ENV: &str = "DISCORD_PUBLIC_KEY";
const DISCORD_CHANNEL_ID_ENV: &str = "DISCORD_CHANNEL_ID";
const TELEGRAM_CHANNEL_ID_ENV: &str = "FAMILYCLAW_TELEGRAM_CHANNEL_ID";
const REPLY_TARGET_ENV: &str = "FAMILYCLAW_REPLY_TARGET";

/// Optional bearer token that protects `POST /inject` (env). Used only in
/// error messages/documentation — the actual value is read via
/// `FamilyConfig`. Cf. `OpenClaw`'s `OPENCLAW_GATEWAY_TOKEN`.
const GATEWAY_TOKEN_ENV: &str = "FAMILYCLAW_GATEWAY_TOKEN";

/// The `orchestrate` subcommand's plan in JSON form. Empty/unset →
/// a small built-in smoke-test plan. Format:
/// `{"id":"plan","nodes":[{"id":"n1","title":"...","description":"...","input":{...}}]}`.
const PLAN_ENV: &str = "FAMILYCLAW_PLAN";

/// Optional LLM output cap (in tokens). Without this the `LlmConfig` default is
/// [`familyclaw_agent::llm::DEFAULT_MAX_TOKENS`] (4096). Set e.g. 8192 so the
/// agent (e.g. long research reports) can respond in full. Independently of
/// this cap, a response cut off mid-generation (`finish_reason == "length"`)
/// is auto-continued — see `FAMILYCLAW_MAX_CONTINUATIONS`
/// (`familyclaw_agent::llm::max_continuations`).
const MAX_TOKENS_ENV: &str = "FAMILYCLAW_MAX_TOKENS";
const REQUEST_TIMEOUT_MS_ENV: &str = "FAMILYCLAW_REQUEST_TIMEOUT_MS";

/// Default values used by `FamilyConfig` (Layer B).
const DEFAULT_BUS_NAME: &str = "familyclaw-gateway-bus";

/// Default listen address when [`ADDR_ENV`] is not set. Binds to the
/// loopback address by default (safe default — does not expose the gateway
/// to the network without a deliberate choice).
const DEFAULT_ADDR: &str = "127.0.0.1:8787";

/// `FamilyClaw` gateway command-line interface.
///
/// Without a subcommand, the gateway behaves as it did before the CLI —
/// it starts the server (`serve`). This preserves backward compatibility
/// with `cargo run -p familyclaw-gateway` and Docker `CMD` invocations that
/// pass no arguments.
#[derive(Parser)]
#[command(name = "familyclaw-gateway", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Gateway subcommands.
#[derive(Subcommand)]
enum Command {
    /// Start the gateway server (default when no subcommand is given).
    Serve,
    /// Query the running gateway's status (`/healthz` + `/readyz`).
    ///
    /// Reads [`ADDR_ENV`] (or the default address) and makes HTTP requests.
    /// Prints the status and returns exit code `0` only when `/readyz` = 200.
    Status,
    /// Check the configuration without starting the server (offline diagnostics).
    Doctor {
        /// Automatically fix what can be fixed (data directory, stale stuck items, …).
        #[arg(long)]
        fix: bool,
    },
    /// Run a multi-step orchestration plan once and print a report.
    ///
    /// This is the **live entry point** for a multi-agent DAG run: it
    /// assembles a [`FamilyBridge`], registers workers, selects the model
    /// ([`LiveTurnExecutor`] with a real LLM chain via [`build_resolver`])
    /// and runs [`Orchestrator::run_with`]. The plan is read from the
    /// [`PLAN_ENV`] environment variable (JSON), or a small built-in default
    /// plan is used as a smoke test.
    ///
    /// **Honest scope note:** runs on the bridge's own substrate
    /// (`EventBus` + `AgentRegistry` + `TaskBoard`), NOT on [`FamilyRuntime`]'s
    /// ractor agents/`ResonanceBus`. This makes DAG orchestration runnable
    /// with real LLM calls; fusing it into live runtime agents is a
    /// separate, larger effort.
    /// DAG orchestration (bridge substrate, not `FamilyRuntime` agents).
    Orchestrate,
    /// Interactive onboarding wizard (TOML + data directory in under 5 min).
    Init,
}

/// The gateway's shared runtime state, referenced by the HTTP handlers.
///
/// Deliberately kept small. `bus` is `Some` when the Resonance Bus has been
/// started — `/readyz` reports readiness based on this.
#[derive(Clone)]
struct GatewayState {
    /// Resonance Bus handle. `Some` = bus running → readiness OK.
    bus: Option<BusHandle>,
    /// Discord channel for the inject handler. `Some` when the channel kind is "discord".
    discord_channel: Option<Arc<DiscordChannel>>,
    /// Optional `POST /inject` bearer token. `Some` = the endpoint requires
    /// `Authorization: Bearer <token>`; `None` = open loopback-only default
    /// (compatible with prior behavior). Cf. `OpenClaw`'s
    /// `OPENCLAW_GATEWAY_TOKEN`.
    inject_token: Option<Arc<str>>,
    /// Discord Interactions Ed25519 public key (hex). `Some` → `/discord/interactions` active.
    discord_public_key: Option<Arc<str>>,
    /// **Shared action runtime** for the operator approval surface
    /// (`GET /approvals/pending`, `POST /approvals/{id}/approve`).
    ///
    /// The same [`Arc<Mutex<ActionRuntime>>`] that [`FamilyRuntime`] wired
    /// into the agent's tool loop ([`FamilyRuntime::actions`]) — the operator
    /// and the agent share the SAME locked state, so the gateway sees exactly
    /// the pending approvals left behind by the agent's suspended turn, and
    /// `approve` runs the suspended action to completion in that same state.
    ///
    /// `Some` in a serving gateway (always, [`build_family`] creates the
    /// action runtime); `None` only in states where the runtime is not
    /// wired up (e.g. tests that don't need the approval surface). When
    /// `None`, the approval routes respond `503 Service Unavailable`.
    actions: Option<Arc<Mutex<ActionRuntime>>>,
    /// **Shared turn-audit collector** for the observable tool-loop trace
    /// (`GET /turns/audit`, TURN-AUDIT roadmap §6 D6).
    ///
    /// The same [`Arc<AuditCollector>`] that [`build_family`] wired into the
    /// agent's tool loop ([`FamilyRuntime::turn_audit`]) — the operator sees
    /// exactly the events the agent's turns logged (turn start,
    /// tool calls **redacted**, suspend/resume, `stop_reason`).
    ///
    /// `Some` in a serving gateway; `None` in states where the runtime is not
    /// wired up (e.g. tests). When `None`, the audit route responds
    /// `503 Service Unavailable`.
    turn_audit: Option<Arc<AuditCollector>>,
    /// **Shared scheduler handle** for the family-agency operator surface
    /// (`POST /tasks/{id}/enabled`, Phase 4 kill switch).
    ///
    /// The same [`SchedulerHandle`] that [`FamilyRuntime`] exposes
    /// ([`FamilyRuntime::scheduler_handle`]) — the operator can toggle
    /// scheduled tasks on/off through the same lock the tick loop uses.
    /// `Some` in a serving gateway when the scheduler is running; `None`
    /// when the scheduler is disabled (`FAMILYCLAW_DREAM_DISABLED`) or the
    /// runtime is not wired up. When `None`, the kill-switch route responds `503`.
    scheduler: Option<SchedulerHandle>,
    /// Path to the family-agency config (`<data_dir>/agency.json`) to which
    /// kill-switch changes are persisted (Phase 4). `Some` when the
    /// scheduler runs on a persistent path; `None` in in-memory mode →
    /// the change stays in memory only (lost on restart, which is correct
    /// for in-memory mode).
    agency_config_path: Option<std::path::PathBuf>,
    /// **Shared metrics registry** for Prometheus export (`GET /metrics`).
    ///
    /// [`MetricsRegistry`] is `Clone` and shares its state via `Arc`, so
    /// this handle sees exactly the same metrics as the instance that
    /// increments them. [`serve`] gives the exact same registry to the
    /// [`EventRecorder`] as well (which
    /// subscribes to the bridge layer's event bus), so runtime events
    /// increment these series live. Built with
    /// [`MetricsRegistry::with_fleet_defaults`], so the fleet's series
    /// (e.g. created tasks, agents online) are present in the export from
    /// the start at value `0` — dashboards don't "disappear" before the first event.
    ///
    /// Export is always safe: [`MetricsRegistry::prometheus_export`]
    /// returns a plain `String` and never leaks secrets (metrics carry only
    /// numeric values, no payload). `None` only in states where the
    /// registry is not wired up (e.g. some tests).
    metrics: Option<MetricsRegistry>,
    /// Deep readyz / canary: LLM model, Discord, journal path.
    readiness: readiness::ReadinessProbe,
}

/// Empty gateway state for tests.
//
// Shared test fixture: kept as a test helper even though current tests
// build `GatewayState` inline for now. `#[cfg(test)]` keeps it out of the
// production binary; the `dead_code` allow suppresses the warning as long
// as no test calls it yet.
#[cfg(test)]
#[allow(dead_code)]
fn test_gateway_state() -> GatewayState {
    GatewayState {
        bus: None,
        discord_channel: None,
        inject_token: None,
        discord_public_key: None,
        actions: None,
        turn_audit: None,
        scheduler: None,
        agency_config_path: None,
        metrics: None,
        readiness: readiness::ReadinessProbe::default(),
    }
}

/// **Operator-safe, redacted** representation of a single pending approval
/// for the `GET /approvals/pending` JSON response.
///
/// This is deliberately its own type rather than `familyclaw-actions`'s
/// internal structure: it carries **only** the three secret-free fields the
/// operator needs to decide on the approval — **never the raw
/// payload, tool arguments, or secrets**. `redacted_summary` comes
/// directly from [`ActionRuntime::pending_summary_for`] (derived only from
/// the skill name + identifiers), and `created_at` is the audit timestamp.
#[derive(serde::Serialize)]
struct PendingApprovalView {
    /// Approval identifier (`POST /approvals/{approval_id}/approve` continues it).
    approval_id: String,
    /// Redacted human-readable summary (no payload, no secrets).
    redacted_summary: String,
    /// Moment when the pending record was created (RFC 3339 timestamp).
    created_at: String,
    /// Moment when the approval expires (RFC 3339 timestamp).
    expires_at: String,
}

/// Liveness check: always responds `200 OK` when the process can serve
/// HTTP requests. Does not check dependencies (cf. [`readyz`]).
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness check: deep check (bus + LLM + Discord + journal).
async fn readyz(
    axum::extract::State(state): axum::extract::State<Arc<GatewayState>>,
) -> (StatusCode, axum::Json<readiness::ReadyzResponse>) {
    let bus_ok = state.bus.is_some();
    readiness::deep_readyz(bus_ok, &state.readiness).await
}

/// Canary: synthetic LLM ping + infra checks.
async fn canary(
    axum::extract::State(state): axum::extract::State<Arc<GatewayState>>,
) -> std::result::Result<axum::Json<readiness::CanaryResponse>, StatusCode> {
    readiness::run_canary(&state.readiness)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Constant-time byte-string comparison (defense-in-depth for the bearer token).
///
/// Returns `true` only if the strings are the same length and byte-for-byte
/// identical. Execution time depends only on the length of the longer
/// string, not its content — we do not short-circuit on the first
/// differing byte, so the comparison does not leak a timing side channel to
/// an attacker (same idiom as the anchor-hash comparison in
/// `familyclaw-security`).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Checks the bearer-token authorization for `POST /inject`.
///
/// - If a token is **not** configured ([`GatewayState::inject_token`] =
///   `None`), the request is accepted as-is (open loopback-only default,
///   backward-compatible).
/// - If a token **is** configured, the `Authorization: Bearer <token>`
///   header must be present and match in constant time — otherwise
///   [`StatusCode::UNAUTHORIZED`].
///
/// Token values are never logged (MEMORY.md secret-leak rule).
///
/// Note: the return type is `std::result::Result` explicitly, because
/// within this crate's scope `Result` refers to the [`familyclaw_core::Result`] alias.
fn check_inject_auth(
    state: &GatewayState,
    headers: &HeaderMap,
) -> std::result::Result<(), StatusCode> {
    let Some(expected) = state.inject_token.as_deref() else {
        // No token configured → open default (loopback-only).
        return Ok(());
    };
    // Parse `Authorization: Bearer <token>` — missing/invalid header = 401.
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);
    match presented {
        Some(tok) if constant_time_eq(tok.as_bytes(), expected.as_bytes()) => Ok(()),
        _ => {
            tracing::warn!("inject: rejected 401 — missing or wrong bearer token");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

/// Optional operator RBAC (`FAMILYCLAW_OPERATOR_ACL=1`) on top of bearer auth.
///
/// When disabled, returns `Ok(())`. When enabled, requires
/// `X-FamilyClaw-Operator-Role: viewer|approver|admin` with a grant for
/// `capability` (see `docs/ENTERPRISE_AUTH.md`).
fn check_operator_capability(
    headers: &HeaderMap,
    capability: &str,
) -> std::result::Result<(), StatusCode> {
    let acl = OperatorAcl::from_env();
    if !acl.is_enabled() {
        return Ok(());
    }
    let role = headers
        .get("x-familyclaw-operator-role")
        .and_then(|v| v.to_str().ok())
        .and_then(OperatorRole::parse);
    match role {
        Some(role) if acl.allows(role, capability) => Ok(()),
        _ => {
            tracing::warn!("operator acl: forbidden for capability {capability}");
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// Injects an external message into the Discord channel.
/// `POST /inject` — JSON: `{"sender": "...", "chat_id": "...", "body": "..."}`
///
/// If [`GATEWAY_TOKEN_ENV`] is configured, the request requires the header
/// `Authorization: Bearer <token>` (constant-time match), otherwise `401`.
async fn inject_discord(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> (StatusCode, &'static str) {
    if let Err(code) = check_inject_auth(&state, &headers) {
        return (code, "unauthorized");
    }
    let Some(ch) = &state.discord_channel else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "discord channel not configured",
        );
    };
    let sender = payload
        .get("sender")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let chat_id = payload
        .get("chat_id")
        .and_then(|v| v.as_str())
        .unwrap_or("dm");
    let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "body must not be empty");
    }
    let msg = match InboundMessage::new(sender, chat_id, body) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("invalid inbound message: {e}");
            return (StatusCode::BAD_REQUEST, "invalid message");
        }
    };
    let envelope = msg.into_envelope(ChannelKind::Discord, ch.channel_id());
    match ch.inject(envelope) {
        Ok(()) => (StatusCode::OK, "injected"),
        Err(e) => {
            tracing::warn!("discord inject failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "inject failed")
        }
    }
}

/// Discord Interactions endpoint — Ed25519 verify + inject + deferred response.
async fn handle_discord_interaction(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(public_key) = state.discord_public_key.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "discord interactions not configured"})),
        );
    };
    let Some(ch) = state.discord_channel.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "discord channel not configured"})),
        );
    };

    let sig = headers
        .get("X-Signature-Ed25519")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let timestamp = headers
        .get("X-Signature-Timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if sig.is_empty() || timestamp.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "missing signature headers"})),
        );
    }

    if let Err(e) = verify_signature(public_key, sig, timestamp, &body) {
        tracing::warn!("discord interaction verify failed: {e}");
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid signature"})),
        );
    }

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("discord interaction json parse failed: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"type": 4, "data": {"content": "invalid payload", "flags": 64}}),
                ),
            );
        }
    };

    let interaction = match DiscordInteraction::from_payload(&payload) {
        Ok(ix) => ix,
        Err(e) => {
            tracing::warn!("discord interaction parse failed: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"type": 4, "data": {"content": "invalid interaction", "flags": 64}}),
                ),
            );
        }
    };

    if interaction.is_ping() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"type": RESPONSE_PONG})),
        );
    }

    if !interaction.is_application_command() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({"type": 4, "data": {"content": "unsupported interaction type", "flags": 64}}),
            ),
        );
    }

    let inbound = match interaction.into_inbound() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("discord slash empty message: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"type": 4, "data": {"content": "message required", "flags": 64}}),
                ),
            );
        }
    };

    let envelope = inbound.into_envelope(ChannelKind::Discord, ch.channel_id());
    if let Err(e) = ch.inject(envelope) {
        tracing::warn!("discord interaction inject failed: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"type": 4, "data": {"content": "inject failed", "flags": 64}})),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({"type": RESPONSE_DEFERRED_CHANNEL_MESSAGE})),
    )
}

/// `GET /approvals/pending` — lists for the operator, **redacted**, the turns
/// that are awaiting human approval (suspend/resume bridge, roadmap §6 D2).
///
/// The response is a JSON list of [`PendingApprovalView`] objects, each
/// containing **only** three secret-free fields: `approval_id`, `redacted_summary`,
/// and `created_at`. **The raw payload, tool arguments, or secrets are
/// never returned** — the source is [`ActionRuntime::try_pending_approvals`] +
/// [`ActionRuntime::pending_summary_for`]/[`ActionRuntime::pending_created_at_for`],
/// all of which derive the data only from the redacted `PendingRecord`
/// (the actions layer's secret-free storage form).
///
/// Protection is the same as `POST /inject`: if [`GATEWAY_TOKEN_ENV`] is
/// configured, the request requires the header `Authorization: Bearer <token>`
/// (constant-time match), otherwise `401`.
///
/// Status codes:
/// - `200 OK` + JSON list (also an empty list if nothing is pending),
/// - `401 Unauthorized` if a bearer token is required and doesn't match,
/// - `503 Service Unavailable` if the action runtime is not wired up
///   ([`GatewayState::actions`] = `None`).
async fn list_pending_approvals(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    if check_operator_capability(&headers, operator_caps::APPROVALS_READ).is_err() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "forbidden" })),
        );
    }
    let Some(actions) = state.actions.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "action runtime not configured" })),
        );
    };

    // Lock only for the duration of the listing. `try_pending_approvals`
    // returns only (approval_id, task_id); we enrich it with the redacted
    // summary and creation time under the SAME lock, so the state can't
    // change in between.
    let rt = actions.lock().await;
    let pending = match rt.try_pending_approvals() {
        Ok(p) => p,
        Err(e) => {
            // Storage-surface read error (in practice only with the
            // crash-resilient surface). Do not leak details beyond the operator.
            warn!("approvals: pending-listan luku epäonnistui: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to read pending approvals" })),
            );
        }
    };
    let views: Vec<PendingApprovalView> = pending
        .iter()
        .map(|p| {
            // Redacted summary + creation time from the same storage surface.
            // `None` (race: consumed outside the lock) → neutral default,
            // never raw data.
            let redacted_summary = rt
                .pending_summary_for(p.approval_id)
                .unwrap_or_else(|| "odottaa ihmisen hyväksyntää".to_string());
            let created_at = rt
                .pending_created_at_for(p.approval_id)
                .map_or_else(String::new, |t| t.to_rfc3339());
            let expires_at = rt
                .pending_expiry_for(p.approval_id)
                .map_or_else(String::new, |t| t.to_rfc3339());
            PendingApprovalView {
                approval_id: p.approval_id.to_string(),
                redacted_summary,
                created_at,
                expires_at,
            }
        })
        .collect();
    drop(rt);

    info!(
        count = views.len(),
        "approvals: listattiin odottavat hyväksynnät (redaktoituina)"
    );
    let body = serde_json::to_value(&views).unwrap_or_else(|_| serde_json::json!([]));
    (StatusCode::OK, Json(body))
}

/// `POST /approvals/{approval_id}/approve` — **approves** the given
/// `approval_id` and **hands off the continuation to the turn's OWNING agent**
/// via the bus's [`BusMessage::ResumeApproval`] control signal (suspend/resume
/// bridge, roadmap §6 D2).
///
/// ## Single consumer for a single-use approval (Option A)
/// The approval is **single-use**: it is consumed (runs the side effect +
/// removed from pending) by exactly one party. In this model the consumer is
/// the **agent**, not the gateway. The gateway VALIDATES (auth + pre-check
/// 400/404/410) and then **publishes** the `ResumeApproval` signal; the
/// owning agent continues on the
/// [`handle_resume_signal`](familyclaw_agent::Agent::handle_resume_signal)
/// → [`resume_approved`](familyclaw_agent::Agent::resume_approved) path, runs
/// the payload-bound side effect **EXACTLY ONCE**, and routes the final
/// response to the originating channel — **without a new LLM turn**. The
/// gateway does NOT consume the approval (two consumers for one single-use
/// approval would be impossible: the later one would see `ApprovalMissing`).
///
/// ## Asynchronous semantics
/// `200 OK` means **the approval was received and handed off to the owning
/// agent** — NOT that the side effect has already run. The side effect runs
/// and the response is delivered **asynchronously** to the channel (correct
/// UX). The response body therefore cannot contain the task's outcome; it
/// contains only the identifier + the `resuming` status.
///
/// The body has no required content (optional). The pre-check is
/// **READ-ONLY** ([`ActionRuntime::pending_expiry_for`]) and does not consume
/// the approval; payload binding + single-use enforcement happen in the
/// agent's [`ActionRuntime::approve`] call, so a modified body cannot spend
/// the approval or leak secrets into execution.
///
/// Protection is the same as `POST /inject` (bearer token if configured).
///
/// Status codes (**fail-closed, no panics**):
/// - `200 OK` + `{ approval_id, status: "resuming", note }` — approval
///   received and handed off to the agent; side effect + response asynchronously
///   to the channel,
/// - `400 Bad Request` if `approval_id` does not parse as a valid identifier,
/// - `401 Unauthorized` if a bearer token is required and doesn't match,
/// - `404 Not Found` if the identifier is not (or no longer) awaiting
///   approval (unknown or already consumed),
/// - `410 Gone` if the approval has expired (TTL elapsed),
/// - `503 Service Unavailable` if (a) the action runtime is not wired up, (b)
///   the bus is not wired up (Option A requires serve mode, where the agent
///   listens on the bus — without the bus, the continuation can never
///   happen), or (c) publishing the signal failed. In all three cases the
///   approval was NOT consumed → the request can be safely retried.
async fn approve_pending(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(approval_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    if check_operator_capability(&headers, operator_caps::APPROVALS_DECIDE).is_err() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "forbidden" })),
        );
    }
    let Some(actions) = state.actions.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "action runtime not configured" })),
        );
    };

    // Parse the identifier (UUID). Invalid form = 400, not 404 — a different reason.
    let Ok(id) = ApprovalId::from_str(approval_id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid approval id" })),
        );
    };

    // Determinism (D1): the timestamp is injected at this one point and
    // drives both the expiry check and the execution of the suspended action.
    let now = familyclaw_core::time::now();

    // **Read-only pre-check** (Option A): distinguishes "unknown" (404) from
    // "expired" (410) before we hand the continuation off to the agent. This
    // does NOT consume the approval ([`ActionRuntime::pending_expiry_for`] is
    // read-only) — the consumer is the agent. Without this distinction, 404
    // and 410 would look the same on the agent's resume path, and we
    // couldn't give the operator a fail-closed, precise reason.
    let rt = actions.lock().await;
    match rt.pending_expiry_for(id) {
        None => {
            // No approval is pending for the identifier (unknown or already consumed).
            warn!(approval = %id, "approvals: approve hylätty 404 — tuntematon tai kulutettu");
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "no such pending approval" })),
            );
        }
        Some(expires_at) if now > expires_at => {
            // Expired → 410 Gone (fail-closed, the side effect is not consumed).
            warn!(approval = %id, "approvals: approve hylätty 410 — hyväksyntä vanhentunut");
            return (
                StatusCode::GONE,
                Json(serde_json::json!({ "error": "approval expired" })),
            );
        }
        Some(_) => {}
    }

    // Pre-check passed: the approval exists and is not expired. We do NOT
    // consume it in the gateway (Option A) — release the action lock and
    // hand the continuation off to the owning agent via the bus. Only the
    // agent consumes the single-use approval (runs the side effect + routes
    // the response), so we don't hold the lock while we publish.
    drop(rt);

    // **Resume bridge (Phase 1 §6 manual gate, Option A):** publish the
    // `ResumeApproval` control signal to the bus, so that the turn's OWNING
    // agent continues on the `handle_resume_signal` → `resume_approved` →
    // `route_reply` path (consumes the approval, runs the side effect
    // EXACTLY ONCE, routes the final response to the channel) WITHOUT a new LLM turn.
    //
    // `publish` is **broadcast** (not point-to-point) — and that's safe
    // here: only the owning agent consumes the resume (the ownership check
    // in `resume_approved` fails closed for everyone else), so other beings
    // no-op. The `from` identifier only affects self-echo suppression; the
    // gateway is not a registered being, so a fresh `BeingId::new()` is
    // sufficient (it cannot collide with anyone).
    let Some(bus) = state.bus.as_ref() else {
        // **No bus → 503, NOT a silent success.** In Option A the side
        // effect runs ONLY on the agent's resume path; without a bus
        // (e.g. CLI / non-serve context) no agent is listening, so the
        // continuation can never happen. The approval was NOT consumed →
        // an honest 503: operator approve requires serve mode (an agent running on the bus).
        warn!(
            approval = %id,
            "approvals: approve hylätty 503 — bussia ei kytketty (Option A vaatii serve-tilan, \
             jossa omistava agentti kuuntelee bussia); hyväksyntää ei kulutettu, voi yrittää uudelleen"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "operator approve requires serve mode (a running agent on the bus); \
                          approval was not actioned and can be retried"
            })),
        );
    };

    let signal = BusMessage::ResumeApproval {
        approval_id: approval_id.clone(),
    };
    if let Err(e) = bus.publish(BeingId::new(), signal) {
        // **Publish failed → 503, approval NOT consumed.** If we can't
        // notify the agent, no continuation happens. Don't return a false
        // 200 — return an honest 503 (still pending, can be retried).
        warn!(
            approval = %id,
            error = %e,
            "approvals: approve hylätty 503 — ResumeApproval-signaalin julkaisu epäonnistui; \
             hyväksyntää ei kulutettu, voi yrittää uudelleen"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "could not notify the owning agent (bus publish failed); \
                          approval was not actioned and can be retried"
            })),
        );
    }

    info!(
        approval = %id,
        "approvals: hyväksyntä otettu vastaan ja ResumeApproval julkaistu — omistava agentti \
         ajaa sivuvaikutuksen + vastaa kanavalle asynkronisesti"
    );
    // **200 = the approval was received and handed off to the agent.** No outcome:
    // the side effect + response run asynchronously on the agent's resume path, so
    // we don't return a task_id/status bundle (no payload, no secrets).
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "approval_id": approval_id,
            "status": "resuming",
            "note": "agent is completing the approved action; the reply will arrive on the originating channel"
        })),
    )
}

/// `POST /approvals/{approval_id}/deny` — denies the pending approval and cancels the task.
async fn deny_pending(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(approval_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    if check_operator_capability(&headers, operator_caps::APPROVALS_DECIDE).is_err() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "forbidden" })),
        );
    }
    let Some(actions) = state.actions.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "action runtime not configured" })),
        );
    };
    let Ok(id) = ApprovalId::from_str(approval_id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid approval id" })),
        );
    };
    let now = familyclaw_core::time::now();
    let mut rt = actions.lock().await;
    match rt.deny_pending(id, now).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "approval_id": approval_id,
                "status": "denied"
            })),
        ),
        Err(e) => {
            warn!(approval = %id, error = %e, "approvals: deny failed");
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

/// `POST /tasks/{task_id}/enabled` — **family-agency kill switch** (Phase 4):
/// toggles a scheduled task on or off.
///
/// Body: JSON `{"enabled": true|false}`. `enabled=false` = the scheduler
/// skips the task on subsequent ticks (kill switch); `true` = re-enables it.
/// The mutation goes through the same lock the tick loop uses, so races are
/// resolved by the lock.
///
/// Responds:
/// - `200 OK` + the new state when the task was found and toggled,
/// - `400 Bad Request` if the identifier is an invalid UUID or the body is missing `enabled`,
/// - `401` if bearer auth is required and doesn't match,
/// - `404 Not Found` if the identifier is not registered with the scheduler,
/// - `503 Service Unavailable` if the scheduler is not wired up (e.g. dream
///   disabled or the runtime is absent).
async fn set_task_enabled_route(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    if check_operator_capability(&headers, operator_caps::TASKS_CONTROL).is_err() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "forbidden" })),
        );
    }
    let Some(scheduler) = state.scheduler.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "scheduler not configured" })),
        );
    };
    let Some(enabled) = payload.get("enabled").and_then(serde_json::Value::as_bool) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "body must be {\"enabled\": bool}" })),
        );
    };
    let Ok(uuid) = uuid::Uuid::parse_str(task_id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid task id" })),
        );
    };
    let id = ScheduledTaskId::from_uuid(uuid);

    let mut sched = scheduler.lock().await;
    if sched.set_task_enabled(id, enabled) {
        drop(sched); // release the lock before file I/O
                     // Persist the change to the config file, so the kill
                     // switch survives a restart (Phase 4). Best-effort:
                     // a persistence failure does not undo the live change
                     // (already applied), but it is logged — in in-memory
                     // mode there is no path, so the change stays in memory
                     // only (correct for in-memory mode).
        if let Some(path) = state.agency_config_path.as_ref() {
            match AgencyConfig::load(path) {
                Ok(mut cfg) => {
                    cfg.set(id, enabled);
                    if let Err(e) = cfg.save(path) {
                        tracing::warn!(target: "familyclaw::scheduler", error = %e, "failed to persist agency config — live change kept, restart may revert it");
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "familyclaw::scheduler", error = %e, "failed to load agency config for persist — live change kept");
                }
            }
        }
        (
            StatusCode::OK,
            Json(serde_json::json!({ "task_id": task_id, "enabled": enabled })),
        )
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no such scheduled task" })),
        )
    }
}

/// `GET /turns/audit` — returns the **observable tool-loop trace**
/// for the operator (TURN-AUDIT, roadmap §6 D6).
///
/// The response is a JSON list of [`familyclaw_actions::ExecAuditEvent`]
/// events in insertion order: each carries the turn's correlation
/// identifier (`action_id`), the event type (`kind`: `turn_started` / `tool_dispatched`
/// / `turn_suspended` / `turn_resumed` / `turn_answered` /
/// `turn_max_iterations`), a timestamp (`at`), and a **redacted** explanation
/// (`detail`). **The raw payload, tool arguments, or secrets are
/// never returned** — `detail` was already redacted at the moment the agent logged it.
///
/// The operator can group the trace by `action_id` to get one
/// turn's entire lifecycle (start → tool calls → suspend/resume →
/// `stop_reason`). At larger volume, filtering/pagination belongs to a later
/// extension — this route returns the currently logged trace as-is.
///
/// Protection is the same as `POST /inject`: if [`GATEWAY_TOKEN_ENV`] is
/// configured, the request requires the header `Authorization: Bearer <token>`
/// (constant-time match), otherwise `401`.
///
/// Status codes:
/// - `200 OK` + JSON list (also empty if nothing has been logged yet),
/// - `401 Unauthorized` if a bearer token is required and doesn't match,
/// - `503 Service Unavailable` if turn audit is not wired up
///   ([`GatewayState::turn_audit`] = `None`).
async fn list_turn_audit(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    if check_operator_capability(&headers, operator_caps::AUDIT_READ).is_err() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "forbidden" })),
        );
    }
    let Some(audit) = state.turn_audit.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "turn audit not configured" })),
        );
    };

    // Events are already redacted (the agent redacted `detail` at the
    // moment it was logged). Serialized as-is — no further processing.
    let events = audit.list();
    info!(
        count = events.len(),
        "turns: listattiin redaktoitu tool-loop-audit-jälki"
    );
    let body = serde_json::to_value(&events).unwrap_or_else(|_| serde_json::json!([]));
    (StatusCode::OK, Json(body))
}

/// Content type for the Prometheus response (exposition text format).
///
/// We use the `version=0.0.4` exposition standard type (`text/plain`), which
/// the Prometheus scraper understands directly. Charset is `utf-8` (metric
/// names are ASCII, but an explicit charset is the exposition recommendation).
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `GET /metrics` — exports the shared [`MetricsRegistry`] in the
/// **deterministic** Prometheus exposition text format (`prometheus_export`).
///
/// The response content type is [`PROMETHEUS_CONTENT_TYPE`] (`text/plain`),
/// which the Prometheus scraper can parse. The body is ordered by metric name
/// ([`MetricsRegistry`] is backed by a `BTreeMap`), so the output is stable and
/// doesn't vary between requests — the same input produces the same output.
///
/// **Which metrics are "live" — the precise, honest state:** the registry
/// is built with [`MetricsRegistry::with_fleet_defaults`], so all of the
/// fleet's pre-named counters and the `agents_online` gauge are present in
/// the export from the start (value `0`). [`serve`] subscribes to the
/// bridge layer's event bus with an [`EventRecorder`] and gives the SAME
/// registry to both the recorder and this handler — so the **mechanism**
/// (event → counter increment → `/metrics`) is wired up and e2e-tested.
///
/// **BUT in the gateway as it currently runs in production, only ONE series
/// is actually moving right now:**
/// - checked `agents_online` (gauge) — `build_family` publishes `AgentRegistered`
///   to the served bus at startup → `1`.
/// - pending `tasks_created`, `task_handoffs`, `tasks_completed`, `contract_*`,
///   `agent_turns`, `llm_*`, `durable_replays`, `workflow_steps_completed` are
///   **wired but unfed**: the recorder maps them,
///   but no live gateway/agent/orchestration path yet publishes the
///   corresponding events (`TaskCreated` / `Custom("task.completed" |
///   "contract.*" | "llm.*" | …)`) to THIS served bus (`orchestrate`
///   uses a separate, unwired bus). They therefore stay at `0` until
///   the tool-loop/orchestration/contract/llm layers publish to the served
///   bus — that is the next wiring task, not a fault of this route.
///
/// `prometheus_export` always returns the ACTUAL numbers, never guesses — zero
/// honestly means "no events yet", not "broken".
///
/// Status codes:
/// - `200 OK` + Prometheus text (even a mostly-zero body is valid),
/// - `503 Service Unavailable` if the registry is not wired up
///   ([`GatewayState::metrics`] = `None`).
///
/// The route is **unprotected** (no bearer token): metrics are numeric
/// time series with no secrets, and scrapers (Prometheus) typically don't
/// send an `Authorization` header. Network-level restriction (loopback
/// binding / firewall) is the right protection layer for this endpoint.
async fn metrics_handler(
    State(state): State<Arc<GatewayState>>,
) -> (
    StatusCode,
    [(axum::http::header::HeaderName, &'static str); 1],
    String,
) {
    let content_type = (axum::http::header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE);
    let Some(registry) = state.metrics.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [content_type],
            "# metrics registry not configured\n".to_string(),
        );
    };
    (StatusCode::OK, [content_type], registry.prometheus_export())
}

/// Builds the gateway's HTTP router with shared state.
fn build_router(state: Arc<GatewayState>) -> Router {
    use axum::routing::post;
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/canary", post(canary))
        // Prometheus metrics (shared MetricsRegistry, with_fleet_defaults).
        // Always registered; when the registry is not wired up
        // ([`GatewayState::metrics`] = `None`), the handler responds 503. Unprotected
        // (numeric time series with no secrets) — see metrics_handler.
        .route("/metrics", get(metrics_handler))
        .route("/inject", post(inject_discord))
        // Operator approval surface (suspend/resume bridge, roadmap §6 D2).
        // Always registered; when the action runtime is not wired up
        // ([`GatewayState::actions`] = `None`), the handlers respond 503.
        // Bearer protection is the same as /inject (`check_inject_auth`).
        .route("/approvals/pending", get(list_pending_approvals))
        // axum 0.7 (matchit 0.7) uses `:param` syntax for path capture;
        // `{approval_id}` would be interpreted as a LITERAL segment → 404 over HTTP.
        .route("/approvals/:approval_id/approve", post(approve_pending))
        .route("/approvals/:approval_id/deny", post(deny_pending))
        // Family-agency kill switch (Phase 4): toggles a scheduled task
        // on/off. Always registered; when the scheduler is not wired up
        // ([`GatewayState::scheduler`] = `None`), the handler responds 503. Bearer
        // protection is the same as /inject.
        .route("/tasks/:task_id/enabled", post(set_task_enabled_route))
        // Observable tool-loop trace (TURN-AUDIT, roadmap §6 D6). Always
        // registered; when turn audit is not wired up ([`GatewayState::turn_audit`] =
        // `None`), the handler responds 503. Bearer protection is the same as /inject.
        .route("/turns/audit", get(list_turn_audit));
    // Self-contained operator reliability console. Both routes expose
    // only data already redacted by the approval and audit surfaces.
    router = router
        .route("/console", get(console::console_page))
        .route("/console/events", get(console::console_events));
    if state.discord_public_key.is_some() && state.discord_channel.is_some() {
        router = router.route("/discord/interactions", post(handle_discord_interaction));
    }
    router.with_state(state)
}

/// Resolves the listen address from an environment variable or the default.
///
/// # Errors
/// [`FamilyClawError::Config`] if the address does not parse as a `SocketAddr`.
fn resolve_addr() -> Result<SocketAddr> {
    let raw = std::env::var(ADDR_ENV).unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    raw.parse::<SocketAddr>()
        .map_err(|e| FamilyClawError::config(format!("invalid {ADDR_ENV} '{raw}': {e}")))
}

/// Builds the LLM resolver from the [`PROVIDERS_ENV`] variable (Layer B).
///
/// Format: `prefix=base_url=KEY_ENV` separated by semicolons, e.g.
/// `openai=https://api.openai.com/v1=OPENAI_API_KEY;deepseek=https://api.deepseek.com/v1=DEEPSEEK_API_KEY`.
///
/// **Key pool (failover gap #1 step 3):** the `KEY_ENV` field can be a
/// **comma-separated list** of env vars, so keys are rotated round-robin
/// on `AuthFailed` before the whole provider is cooled down,
/// e.g. `openai=https://api.openai.com/v1=OPENAI_API_KEY_1,OPENAI_API_KEY_2`.
/// The single-key syntax (`=OPENAI_API_KEY`) remains backward-compatible.
///
/// Empty/unset variable → an empty resolver (the agent runs without an LLM).
/// Invalid lines are skipped with a warning — one typo does not bring down the gateway.
fn build_resolver() -> EnvEndpointResolver {
    let mut resolver = EnvEndpointResolver::new();
    // Optional output-token cap from env. Applied to ALL resolved
    // models (apply_tunings). Without this, the LlmConfig default
    // (familyclaw_agent::llm::DEFAULT_MAX_TOKENS, 4096) applies.
    if let Ok(raw) = std::env::var(MAX_TOKENS_ENV) {
        match raw.trim().parse::<u32>() {
            Ok(max) if max > 0 => {
                resolver = resolver.with_max_tokens(max);
                info!(
                    max_tokens = max,
                    "LLM output-katto asetettu {MAX_TOKENS_ENV}:stä"
                );
            }
            _ => warn!(
                value = raw,
                "ohitetaan kelvoton {MAX_TOKENS_ENV} (odotettu positiivinen kokonaisluku)"
            ),
        }
    }
    if let Ok(raw) = std::env::var(REQUEST_TIMEOUT_MS_ENV) {
        match raw.trim().parse::<u64>() {
            Ok(ms) if ms >= 5_000 => {
                resolver = resolver.with_request_timeout_ms(ms);
                info!(
                    request_timeout_ms = ms,
                    "LLM request-timeout asetettu {REQUEST_TIMEOUT_MS_ENV}:stä"
                );
            }
            _ => warn!(
                value = raw,
                "ohitetaan kelvoton {REQUEST_TIMEOUT_MS_ENV} (odotettu >= 5000 ms)"
            ),
        }
    }
    let Ok(spec) = std::env::var(PROVIDERS_ENV) else {
        return resolver;
    };
    for entry in spec.split(';').filter(|s| !s.trim().is_empty()) {
        let parts: Vec<&str> = entry.splitn(3, '=').map(str::trim).collect();
        if let [prefix, base_url, key_field] = parts.as_slice() {
            // The key field can be a comma-separated pool (round-robin rotation).
            let key_envs: Vec<String> = key_field
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect();
            if !prefix.is_empty() && !base_url.is_empty() && !key_envs.is_empty() {
                resolver = resolver.with_provider_keys(*prefix, *base_url, key_envs);
                continue;
            }
        }
        warn!(
            entry,
            "ohitetaan kelvoton {PROVIDERS_ENV}-rivi (odotettu prefix=base_url=KEY_ENV[,KEY_ENV2])"
        );
    }
    resolver
}

/// Loads the agent's soul from the profile directory if [`FAMILYCLAW_PROFILE_DIR`]
/// is set; otherwise a bare shell (generic core, no operator soul).
///
/// [`FAMILYCLAW_PROFILE_DIR`]: familyclaw_agent::PROFILE_DIR_ENV
fn load_agent_soul(agent_name: &str) -> Soul {
    match resolve_profile_dir(None, agent_name) {
        Some(dir) => match familyclaw_agent::load_soul(&dir) {
            Ok(soul) => {
                info!(dir = %dir.display(), "sielu ladattu profiilihakemistosta");
                soul
            }
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "sielun lataus epäonnistui — paljas runko");
                Soul::from_essence(format!("I am {agent_name}, a FamilyClaw being."))
            }
        },
        None => Soul::from_essence(format!("I am {agent_name}, a FamilyClaw being.")),
    }
}

/// Shared-instance adapter: wraps an `Arc<DiscordChannel>` as a `Channel`
/// trait object by delegating all calls to the SAME instance.
///
/// **Why this exists (dual-instance bug fix):** the bus pump
/// ([`build_family`] → `channel.receive()`) and the inject paths (`/inject`,
/// `/discord/interactions` → `Arc<DiscordChannel>::inject`) were previously
/// built from TWO separate [`DiscordChannel::from_webhook`] calls.
/// Each call creates its own `mpsc` pair (`inbound_tx`/`inbound_rx`), so
/// injected messages were pushed into instance #1's `inbound_tx`, whose
/// `inbound_rx` was never consumed by anyone — webhook injection disappeared
/// into a black hole.
///
/// This adapter lets the channel be built **once** (`Arc<DiscordChannel>`) and
/// shares the SAME instance: the bus gets the adapter (`Box<dyn Channel>`),
/// inject gets an `Arc` handle. `receive()`/`send()`/`inject()` all take
/// `&self`, so they operate on one instance's same
/// `inbound_tx`/`inbound_rx` pair — exactly the single-stream model that
/// `DiscordChannel::inject`'s documentation already promises.
struct SharedDiscordChannel(Arc<DiscordChannel>);

impl Channel for SharedDiscordChannel {
    fn channel_id(&self) -> &str {
        self.0.channel_id()
    }

    fn kind(&self) -> ChannelKind {
        self.0.kind()
    }

    fn send(&self, message: OutboundMessage) -> SendFuture<'_> {
        self.0.send(message)
    }

    fn receive(&self) -> ChannelResult<MessageStream> {
        self.0.receive()
    }
}

/// Starts [`FamilyRuntime`] with configuration read from the environment
/// (Layer B). Reads the agent's name, model, soul, Telegram channel, and
/// reply target from env vars — nothing is hardcoded (Layer A).
///
/// # Errors
/// - [`FamilyClawError::InvalidInput`] if a required env var
///   ([`TELEGRAM_TOKEN_ENV`], [`TELEGRAM_CHANNEL_ID_ENV`],
///   [`REPLY_TARGET_ENV`]) is missing or building the channel fails.
///
/// `bridge` is the shared bridge-layer event bus for observability:
/// it is given to [`build_family`], which publishes the agent registration
/// to it (→ `agents_online` gauge). The caller (serve) must already have
/// subscribed to it with an [`EventRecorder`] before this call, so the event isn't lost.
///
/// Returns true when the listen address is loopback-only (`127.0.0.0/8` or `::1`).
///
/// Binding `0.0.0.0` / `::` is **not** loopback — those expose the port on
/// every interface and therefore require a gateway token (fail-closed).
fn is_loopback_bind(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Resolves the `/inject` protection token from the configuration.
///
/// - Token set → mandatory bearer match (value never logged).
/// - Token empty **and** loopback bind → open default with a warning
///   (local eval / `cargo run` convenience).
/// - Token empty **and** non-loopback bind → [`Err`] fail-closed (do not
///   start serving operator routes on a remotely reachable socket without
///   auth). Matches `OpenClaw`'s production expectation that a gateway token
///   is required once the control plane leaves loopback.
fn resolve_inject_token(cfg: &FamilyConfig, bind: SocketAddr) -> Result<Option<Arc<str>>> {
    let raw = cfg.gateway_token().trim();
    if raw.is_empty() {
        if !is_loopback_bind(bind) {
            return Err(FamilyClawError::config(format!(
                "{GATEWAY_TOKEN_ENV} is required when binding non-loopback address {bind} \
                 (refusing to expose /inject, /approvals, and /turns/audit without auth). \
                 Set a bearer token or bind 127.0.0.1 / ::1 for local eval."
            )));
        }
        warn!(
            "{GATEWAY_TOKEN_ENV} unset — POST /inject is open on loopback {bind}. \
             Set a token before any non-loopback deploy."
        );
        Ok(None)
    } else {
        info!("POST /inject protected by bearer token ({GATEWAY_TOKEN_ENV})");
        Ok(Some(Arc::from(raw)))
    }
}

fn build_extra_agent_specs(cfg: &FamilyConfig, model_cfg: &ModelConfig) -> Vec<AgentBuildSpec> {
    cfg.all_agents()
        .into_iter()
        .skip(1)
        .map(|a| AgentBuildSpec {
            config: AgentConfig::new_with_stable_id(&a.name, model_cfg.clone()),
            soul: load_agent_soul(&a.name),
            reply_target: if a.reply_target.is_empty() {
                None
            } else {
                Some(a.reply_target)
            },
        })
        .collect()
}

/// Returns the runtime, the Discord channel (inject/interactions), the inject token, and the public key.
// Three channel branches (none / discord / telegram), each assembling the
// runtime on its own path — long but linear; splitting it up would hurt readability.
#[allow(clippy::too_many_lines)]
async fn start_runtime(
    bridge: FamilyBridge,
) -> Result<(
    FamilyRuntime,
    Option<Arc<DiscordChannel>>,
    Option<Arc<str>>,
    Option<Arc<str>>,
)> {
    let cfg = FamilyConfig::load()?;
    let all_agents = cfg.all_agents();
    let primary = &all_agents[0];
    let agent_name = primary.name.clone();
    let model = cfg.model().to_string();
    let channel_kind = cfg.channel_kind().to_string();
    let mut model_cfg = ModelConfig::new(model.clone());
    for fb in cfg.fallback_models() {
        model_cfg = model_cfg.with_fallback(fb);
    }
    let extra_agents = build_extra_agent_specs(&cfg, &model_cfg);

    // Token resolution needs the eventual bind address. `start_runtime` is
    // called from `serve` after `resolve_addr`; we re-read the same env here
    // so channel-less / Discord / Telegram paths share one fail-closed gate.
    let bind = resolve_addr()?;
    let inject_token: Option<Arc<str>> = resolve_inject_token(&cfg, bind)?;

    // CHANNEL-LESS PUBLISH MODE (`FAMILYCLAW_CHANNEL_KIND=none`): start the
    // gateway WITHOUT any operator key, soul, or reply target. Assembles the
    // runtime on top of a [`MockChannel`] (in-memory, no external SDK), so
    // a fresh `cargo install` user can `serve` + `status`-verify the HTTP surface
    // (`/healthz`, `/readyz`, `/metrics`) BEFORE wiring up a real channel. This
    // is a prerequisite for publishability: the OSS boundary (Layer A) means
    // the platform works with an empty profile — Telegram/Discord are Layer B add-ons.
    if channel_kind == "none" {
        info!(
            "channel-less publish mode (FAMILYCLAW_CHANNEL_KIND=none) — MockChannel, no family keys"
        );
        let mock = familyclaw_channels::MockChannel::new("familyclaw-none")
            .map_err(FamilyClawError::from)?;
        let channel: Box<dyn Channel> = Box::new(mock);
        // A reply target is not required in channel-less mode — MockChannel
        // swallows responses into its outbox. We use a neutral placeholder
        // that does not route anywhere outbound.
        let reply_target = "none".to_string();
        let agent_cfg = AgentConfig::new_with_stable_id(&agent_name, model_cfg.clone());
        let soul = load_agent_soul(&agent_name);
        let resolver = build_resolver();
        let runtime = build_family(
            Some(DEFAULT_BUS_NAME.to_string()),
            agent_cfg,
            soul,
            extra_agents.clone(),
            channel,
            reply_target,
            &resolver,
            Some(bridge),
        )
        .await?;
        return Ok((runtime, None, inject_token, None));
    }

    let (channel, discord_ch): (Box<dyn Channel>, Option<Arc<DiscordChannel>>) = if channel_kind
        == "discord"
    {
        let bot_token = cfg.discord_bot_token();
        let ch_id = cfg.discord_channel_id();
        // TWO-WAY bot mode if DISCORD_BOT_TOKEN is set: the serenity
        // gateway listens (MESSAGE_CONTENT) AND posts. Otherwise fall back
        // to one-way webhook posting (DISCORD_WEBHOOK_URL).
        // Build the DiscordChannel EXACTLY ONCE and share the same instance: the bus pump
        // gets the `SharedDiscordChannel` adapter, the inject paths get an `Arc` handle — both
        // to the same `inbound_tx`/`inbound_rx` pair (see SharedDiscordChannel's documentation).
        let dc = if bot_token.is_empty() {
            let webhook_url = cfg.discord_webhook_url();
            if webhook_url.is_empty() {
                return Err(FamilyClawError::invalid_input(format!(
                        "discord channel requires DISCORD_BOT_TOKEN (kaksisuuntainen) tai {DISCORD_WEBHOOK_URL_ENV} (postaus)"
                    )));
            }
            info!("Discord: yksisuuntainen webhook-postaus");
            DiscordChannel::from_webhook(webhook_url.to_string(), ch_id.to_string())
                .map_err(FamilyClawError::from)?
        } else {
            let cid: u64 = ch_id.trim().parse().map_err(|_| {
                FamilyClawError::invalid_input(format!(
                    "DISCORD_CHANNEL_ID must be a numeric id for bot mode, got: {ch_id:?}"
                ))
            })?;
            // owner_id from config (TOML + env FAMILYCLAW_OWNER_ID at the config boundary); 0 = DMs off.
            let dc = DiscordChannel::new(bot_token.to_string(), cid, cfg.discord_owner_id())
                .map_err(FamilyClawError::from)?;
            // Start the gateway connection: returns only once `ready` or an error.
            dc.start().await.map_err(FamilyClawError::from)?;
            info!("Discord: kaksisuuntainen bot-moodi (kanava {cid})");
            dc
        };
        let dc_arc = Arc::new(dc);
        let ch: Box<dyn Channel> = Box::new(SharedDiscordChannel(Arc::clone(&dc_arc)));
        (ch, Some(dc_arc))
    } else {
        let token = cfg.telegram_token();
        if token.is_empty() {
            return Err(FamilyClawError::invalid_input(format!(
                "{TELEGRAM_TOKEN_ENV} must be set"
            )));
        }
        let ch_id = cfg.telegram_channel_id();
        if ch_id.is_empty() {
            return Err(FamilyClawError::invalid_input(format!(
                "{TELEGRAM_CHANNEL_ID_ENV} must be set"
            )));
        }
        let tc = TelegramChannel::new(token.to_string(), ch_id.to_string())
            .map_err(FamilyClawError::from)?;
        let ch: Box<dyn Channel> = Box::new(tc);
        (ch, None)
    };

    let reply_target = cfg.reply_target();
    if reply_target.is_empty() {
        return Err(FamilyClawError::invalid_input(format!(
            "{REPLY_TARGET_ENV} must be set"
        )));
    }
    let reply_target = reply_target.to_string();

    // STABLE being identifier: derived deterministically from the agent's
    // name, NOT randomly assigned. `AgentConfig::new` rolls a random id on
    // every process startup — in that case the agent's `being_id` would
    // change on every restart, and a resumable turn saved to the
    // crash-resilient surface before a crash would NO LONGER match the
    // waking agent's ownership check (its own suspended turn would look
    // like it "belongs to another being" and could never be resumed).
    // An id derived from the name stays stable across a restart.
    // Model configuration: primary + optional fallback models
    // (FAMILYCLAW_FALLBACK_MODELS). Without fallbacks the agent runs ONLY
    // on the primary — if it's down/out of quota, the whole being is silent.
    // LlmFailover (llm_chain.rs) moves to the next one when the primary fails.
    if cfg.fallback_models().is_empty() {
        info!(agent = %agent_name, "malli: vain primary (ei FAMILYCLAW_FALLBACK_MODELS)");
    } else {
        info!(
            agent = %agent_name,
            count = cfg.fallback_models().len(),
            "malli: primary + varamallit"
        );
    }
    let agent_cfg = AgentConfig::new_with_stable_id(&agent_name, model_cfg.clone());
    let soul = load_agent_soul(&agent_name);
    let resolver = build_resolver();

    info!(agent = %agent_name, channel = %channel_kind, "kootaan FamilyRuntime (build_family)");
    let runtime = build_family(
        Some(DEFAULT_BUS_NAME.to_string()),
        agent_cfg,
        soul,
        extra_agents,
        channel,
        reply_target,
        &resolver,
        Some(bridge),
    )
    .await?;

    let discord_public_key: Option<Arc<str>> = if channel_kind == "discord" {
        let pk = cfg.discord_public_key().trim();
        if pk.is_empty() {
            warn!("{DISCORD_PUBLIC_KEY_ENV} puuttuu — POST /discord/interactions ei ole käytössä");
            None
        } else {
            info!("Discord Interactions aktiivinen ({DISCORD_PUBLIC_KEY_ENV} set)");
            Some(Arc::from(pk))
        }
    } else {
        None
    };

    Ok((runtime, discord_ch, inject_token, discord_public_key))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing: default level info, overridable with the RUST_LOG variable.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    // No subcommand = serve (backward compatibility).
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        Command::Status => status().await,
        Command::Doctor { fix } => doctor(fix).await,
        Command::Init => init_wizard(),
        Command::Orchestrate => orchestrate().await,
    }
}

/// Starts the gateway server and stays up until `Ctrl-C`.
///
/// This is the former `main` body, unchanged: one [`build_family`] call
/// assembles the bus + agent + channel + reply pump (`FamilyRuntime`), the
/// HTTP shell binds the port, and a clean shutdown stops the runtime.
///
/// # Errors
/// [`FamilyClawError`] if configuration, binding, or serving fails.
async fn serve() -> Result<()> {
    let addr = resolve_addr()?;
    info!(%addr, "familyclaw-gateway käynnistyy");

    // Prometheus metrics (GET /metrics): built with the fleet defaults, and
    // the SAME instance is shared with both the observability recorder
    // (which increments series) and GatewayState (which serves them) —
    // `MetricsRegistry` is `Clone` + `Arc`-shared, so both see the same numbers.
    let metrics = MetricsRegistry::with_fleet_defaults();

    // Observability bridge: subscribes to the bridge layer's event bus with
    // an EventRecorder BEFORE assembling the runtime (EventBus only
    // delivers events published after the subscription). The same `bridge`
    // is given to build_family, which publishes the agent registration →
    // the recorder increments the shared registry (agents_online). The
    // background task drains events continuously (run = a blocking loop
    // until the bridge closes).
    let bridge = FamilyBridge::new();
    let recorder = EventRecorder::new(&bridge, metrics.clone());
    tokio::spawn(recorder.run());

    // C5 seam: one build_family call assembles the bus + agent + channel +
    // reply pump (FamilyRuntime). The bus handle is handed off to
    // GatewayState; the HTTP/shutdown shell stays unchanged (just
    // bus.stop() → runtime.shutdown()). The same `bridge` is passed into
    // the runtime, which publishes the agent registration to it
    // (EventRecorder already subscribed above).
    let (runtime, discord_ch, inject_token, discord_public_key) = start_runtime(bridge).await?;
    info!("FamilyRuntime käynnissä (bus + agentti + kanava)");

    // The operator approval surface shares the SAME Arc<Mutex<ActionRuntime>>
    // handle that build_family wired into the agent's tool loop — pending
    // approvals (suspend) and granting them (resume) happen in the same
    // locked state. Cf. roadmap §6 D2.
    let actions = Some(runtime.actions());
    // Observable tool-loop trace (TURN-AUDIT, roadmap §6 D6): the same
    // Arc<AuditCollector> that build_family wired into the agent's tool loop.
    let turn_audit = Some(runtime.turn_audit());
    // Scheduler handle (family agency, Phase 4): the same SchedulerHandle
    // the runtime exposes → the kill-switch route toggles tasks on/off. None if
    // the scheduler is not running.
    let scheduler = runtime.scheduler_handle();
    // Agency config path: the kill-switch change is persisted here (Phase 4).
    let agency_config_path = runtime.agency_config_path();

    // Metrics registry (GET /metrics): the SAME instance the EventRecorder
    // above got (metrics.clone()). Event-driven population is now WIRED UP —
    // the agent registration (build_family → bridge) bumped the
    // `agents_online` gauge, and the bridge layer's `task.*`/`contract.*`/`llm.*`/…
    // events increment the corresponding series via the recorder. The
    // registry is shared with GatewayState via the Arc-sharing pattern →
    // /metrics sees exactly the numbers the recorder incremented.
    let discord_probe = discord_ch.as_ref().and_then(|dc| {
        let token_set = !std::env::var("DISCORD_BOT_TOKEN")
            .unwrap_or_default()
            .is_empty();
        if token_set {
            Some(Arc::clone(dc))
        } else {
            None
        }
    });
    let readiness_probe = readiness::build_probe(
        FamilyConfig::load().ok().map(|c| c.model_config()),
        discord_probe,
    );
    if let Some(ref dir) = readiness_probe.data_dir {
        if let Err(e) = readiness::cleanup_stale_approval_tasks(dir, 7).await {
            warn!("stale action_tasks cleanup failed: {e}");
        }
    }

    let state = Arc::new(GatewayState {
        bus: Some(runtime.bus().clone()),
        discord_channel: discord_ch,
        inject_token,
        discord_public_key,
        actions,
        turn_audit,
        scheduler,
        agency_config_path,
        metrics: Some(metrics),
        readiness: readiness_probe,
    });
    info!("operaattorin hyväksyntäpinta valmis — GET /approvals/pending, POST /approvals/{{id}}/approve");
    let app = build_router(state);

    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| FamilyClawError::bus(format!("gateway failed to bind {addr}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| FamilyClawError::bus(format!("gateway local_addr failed: {e}")))?;
    info!(%bound, "gateway kuuntelee — /healthz ja /readyz valmiina");

    // Serve until Ctrl-C requests a clean shutdown.
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    // Shutdown: stop the runtime cleanly (cancels tasks + stops the bus)
    // regardless of the serve outcome.
    info!("gateway sammuu — pysäytetään FamilyRuntime");
    runtime.shutdown().await;

    serve_result.map_err(|e| FamilyClawError::bus(format!("gateway serve error: {e}")))?;
    info!("familyclaw-gateway pysähtyi siististi");
    Ok(())
}

/// Builds an `http://<addr><path>` URL from the listen address.
///
/// A running gateway binds to loopback by default, so `status`
/// assumes the `http` scheme (no TLS) — the same assumption as the server's binding.
fn health_url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

/// Durability-state summary that `status`/`doctor` shows the operator.
///
/// The fields report **what [`build_family`] wires up** with the current
/// `FAMILYCLAW_DATA_DIR` environment, without secrets or file paths:
/// whether the process is in crash-resilient (persistent) or in-memory
/// mode, and the kind tags of the wired-up [`ActionRuntime`] surfaces.
struct DurabilityReport {
    /// `true` when `FAMILYCLAW_DATA_DIR` is set (persistent, crash-resilient).
    persistent: bool,
    /// Kind tag of the dispatch outbox ([`ActionRuntime::dispatch_outbox_kind`]),
    /// `"journal"` or `"in-memory"`.
    dispatch_outbox_kind: &'static str,
    /// Kind tag of the pending-approvals surface
    /// ([`ActionRuntime::pending_store_kind`]), `"journal"` or `"in-memory"`.
    pending_store_kind: &'static str,
}

impl DurabilityReport {
    /// Formats a one-line durability summary with no status prefix.
    ///
    /// E.g. `persistent (data_dir set); dispatch_outbox=journal;
    /// pending_store=journal` or `in-memory (no FAMILYCLAW_DATA_DIR) —
    /// crash-survival OFF; dispatch_outbox=in-memory; pending_store=in-memory`.
    /// The file path is **not** exposed (only `set` presence).
    fn summary(&self) -> String {
        let mode = if self.persistent {
            "persistent (data_dir set)".to_string()
        } else {
            "in-memory (no FAMILYCLAW_DATA_DIR) — crash-survival OFF".to_string()
        };
        format!(
            "{mode}; dispatch_outbox={}; pending_store={}",
            self.dispatch_outbox_kind, self.pending_store_kind
        )
    }
}

/// Assembles a [`DurabilityReport`] by building the same [`ActionRuntime`]
/// that [`build_family`] would choose with the current `FAMILYCLAW_DATA_DIR` environment.
///
/// A thin shell around [`durability_report_for`]: reads `FAMILYCLAW_DATA_DIR`
/// from the process environment (empty = unset = in-memory) and delegates.
///
/// # Errors
/// [`FamilyClawError::config`] if opening the journal surfaces for the
/// persistent path fails (the same error startup would give).
async fn build_durability_report() -> Result<DurabilityReport> {
    let data_dir = std::env::var("FAMILYCLAW_DATA_DIR")
        .ok()
        .filter(|v| !v.is_empty());
    durability_report_for(data_dir.as_deref()).await
}

/// Assembles a [`DurabilityReport`] for the given data directory (env-free core).
///
/// `data_dir`:
/// - `Some(dir)` → persistent path: opens the same journal surfaces as
///   [`build_family`] (durable pending + task + dispatch outbox) and reads their
///   **actual** kind tags — no hardcoding.
/// - `None` → in-memory path: all surfaces at their defaults, no disk I/O.
///
/// By reading the kind tags from the wired-up surfaces
/// ([`ActionRuntime::dispatch_outbox_kind`] + [`ActionRuntime::pending_store_kind`])
/// the report matches exactly the durability path the server would get.
/// On the persistent path the journal files are opened (idempotent append
/// log, same as at startup). The branching is env-free → deterministically
/// testable with an explicit directory.
///
/// # Errors
/// [`FamilyClawError::config`] if opening the journal surfaces for the
/// persistent path fails (the same error startup would give).
async fn durability_report_for(data_dir: Option<&str>) -> Result<DurabilityReport> {
    // Same branching as in build_family: the data directory decides the
    // persistent (journal) vs. in-memory path.
    let runtime = if let Some(dir) = data_dir {
        let dir = std::path::PathBuf::from(dir);
        let pending_path = dir.join("pending_approvals.jsonl");
        let task_path = dir.join("action_tasks.jsonl");
        let dispatch_path = dir.join("dispatch_outbox.jsonl");
        // `with_durable_stores` now itself opens the crash-resilient dispatch
        // outbox from a third path — the same single-call assembly as in
        // build_family, no separate with_dispatch_outbox chaining and no
        // double-opening of the outbox.
        ActionRuntime::with_durable_stores(pending_path, task_path, dispatch_path)
            .await
            .map_err(|e| {
                FamilyClawError::config(format!("durable action stores open failed: {e}"))
            })?
    } else {
        // In-memory path: all surfaces at their defaults, no disk.
        ActionRuntime::with_default_skills()
            .map_err(|e| FamilyClawError::config(format!("action runtime build failed: {e}")))?
    };

    Ok(DurabilityReport {
        persistent: data_dir.is_some(),
        dispatch_outbox_kind: runtime.dispatch_outbox_kind(),
        pending_store_kind: runtime.pending_store_kind(),
    })
}

/// Returns the sandbox availability label.
///
/// Delegates to [`familyclaw_sandbox::sandbox_availability`], which reports
/// the **actual compiled backend**: `wasmtime (host-import denial + fuel
/// cap)` when the `wasmtime` passthrough feature is active, otherwise `none (noop)`.
/// By reading availability directly from the sandbox crate (rather than the
/// gateway's own separate flag), the report cannot lie: if the label says
/// `wasmtime`, the real backend is actually compiled in. Deterministic and
/// secret-free → suitable for both `status` and `doctor` output.
fn sandbox_label() -> &'static str {
    familyclaw_sandbox::sandbox_availability()
}

/// Returns the label of the active memory embedding provider (Phase 3, D4).
///
/// The runtime wraps memory with `EmbeddingMemoryStore` using the
/// [`DeterministicEmbedder`](familyclaw_embeddings::DeterministicEmbedder)
/// default provider (dependency-free, poverty-compatible). Reports the
/// provider's stable id + dimensionality so the operator sees which
/// embedding is actually in use. Deterministic and secret-free → suitable
/// for `status`/`doctor` output. When a feature-gated model provider is
/// added, this will be updated to report the actual compiled provider (like [`sandbox_label`]).
fn embedder_label() -> String {
    use familyclaw_embeddings::DeterministicEmbedder;
    format!(
        "{} (dim={})",
        DeterministicEmbedder::ID,
        DeterministicEmbedder::DEFAULT_DIMENSIONS
    )
}

/// Queries the running gateway's status (`/healthz` + `/readyz`).
///
/// Reads the listen address via [`resolve_addr`] and makes two HTTP
/// GET requests. Prints the status of each endpoint plus the **durability state**
/// ([`build_durability_report`]) and **sandbox availability**
/// ([`sandbox_label`]), so the operator sees which backing surface is actually
/// wired up. Returns `Ok(())` only when `/readyz` responds `200 OK`; otherwise
/// [`FamilyClawError::bus`], in which case the process exits with a non-zero exit code.
///
/// # Errors
/// - [`FamilyClawError::config`] if the listen address does not parse.
/// - [`FamilyClawError::config`] if opening the journal surfaces for the
///   persistent path fails while assembling the durability report.
/// - [`FamilyClawError::bus`] if the gateway cannot be reached or `/readyz`
///   is not `200`.
async fn status() -> Result<()> {
    let addr = resolve_addr()?;
    let client = reqwest::Client::new();

    let health = client
        .get(health_url(addr, "/healthz"))
        .send()
        .await
        .map_err(|e| FamilyClawError::bus(format!("gateway not reachable at {addr}: {e}")))?;
    let health_ok = health.status().is_success();
    println!("healthz {addr} -> {}", health.status());

    let ready = client
        .get(health_url(addr, "/readyz"))
        .send()
        .await
        .map_err(|e| FamilyClawError::bus(format!("gateway not reachable at {addr}: {e}")))?;
    let ready_status = ready.status();
    println!("readyz  {addr} -> {ready_status}");

    // Durable backing surface + sandbox: the operator sees what's
    // actually wired up (not just HTTP liveness).
    let durability = build_durability_report().await?;
    println!("durability: {}", durability.summary());
    println!("sandbox: {}", sandbox_label());
    println!("embedder: {}", embedder_label());

    if health_ok && ready_status.as_u16() == 200 {
        println!("status: ready");
        Ok(())
    } else {
        Err(FamilyClawError::bus(format!(
            "gateway not ready (healthz ok={health_ok}, readyz={ready_status})"
        )))
    }
}

/// Checks the gateway's configuration offline (without starting the server).
///
/// Performs three checks and prints each result:
/// 1. **addr** — [`resolve_addr`] parses the listen address,
/// 2. **port** — the address can be temporarily bound (port free),
/// 3. **env** — the required environment variables are set.
///
/// For secrets (e.g. [`TELEGRAM_TOKEN_ENV`]) only **presence** is reported
/// (`set`/`MISSING`) — values are not printed (MEMORY.md secret-leak rule).
///
/// # Errors
/// [`FamilyClawError::invalid_input`] if any check fails, in which case the
/// process exits with a non-zero exit code.
// Sequential check blocks (addr/port/env/durability/sandbox/…), each
// printing its own line — long but a straightforward diagnostic sequence.
#[allow(clippy::too_many_lines)]
async fn doctor(fix: bool) -> Result<()> {
    let cfg = FamilyConfig::load()?;
    let mut ok = true;

    // 1. The listen address parses.
    match resolve_addr() {
        Ok(addr) => {
            println!("[OK]      addr      {addr}");
            // 2. Port free — try a temporary bind.
            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    println!("[OK]      port      {addr} bindable");
                    drop(listener);
                }
                Err(e) => {
                    println!("[FAIL]    port      {addr} not bindable: {e}");
                    ok = false;
                }
            }
        }
        Err(e) => {
            println!("[FAIL]    addr      {e}");
            ok = false;
        }
    }

    // 3. Required env vars — presence only, no values.
    //    (TELEGRAM_TOKEN is a secret → strictly set/MISSING only.)
    let channel_kind = cfg.channel_kind().to_string();
    // Channel-less publish mode (`none`): no required channel envs or reply
    // target — the gateway runs on MockChannel (HTTP surface + /metrics work).
    // This is the fresh-`cargo install` smoke-test mode: `serve` + `status`
    // without operator keys. Channel-specific env checks are skipped entirely.
    let channel_keys: &[&str] = if channel_kind == "none" {
        &[]
    } else if channel_kind == "discord" {
        &[DISCORD_CHANNEL_ID_ENV, REPLY_TARGET_ENV]
    } else {
        &[
            TELEGRAM_TOKEN_ENV,
            TELEGRAM_CHANNEL_ID_ENV,
            REPLY_TARGET_ENV,
        ]
    };
    if channel_kind == "none" {
        println!("[OK]      channel   none (channel-less serve — MockChannel, no family keys)");
    } else {
        println!("[INFO]     channel   {channel_kind}");
    }
    for key in channel_keys {
        if std::env::var_os(key).is_some_and(|v| !v.is_empty()) {
            println!("[OK]      env       {key} set");
        } else {
            println!("[MISSING] env       {key}");
            ok = false;
        }
    }

    if channel_kind == "discord" {
        // Discord requires EITHER a bot token (two-way) OR a webhook (posting).
        let has_bot = std::env::var_os(DISCORD_BOT_TOKEN_ENV).is_some_and(|v| !v.is_empty());
        let has_webhook = std::env::var_os(DISCORD_WEBHOOK_URL_ENV).is_some_and(|v| !v.is_empty());
        if has_bot {
            println!("[OK]      env       {DISCORD_BOT_TOKEN_ENV} set (kaksisuuntainen bot)");
        } else if has_webhook {
            println!("[OK]      env       {DISCORD_WEBHOOK_URL_ENV} set (webhook-postaus)");
        } else {
            println!("[MISSING] env       {DISCORD_BOT_TOKEN_ENV} tai {DISCORD_WEBHOOK_URL_ENV}");
            ok = false;
        }
    }

    if channel_kind == "discord" {
        if std::env::var_os(DISCORD_PUBLIC_KEY_ENV).is_some_and(|v| !v.is_empty()) {
            println!("[OK]      env       {DISCORD_PUBLIC_KEY_ENV} set (interactions)");
        } else {
            println!(
                "[WARN]    env       {DISCORD_PUBLIC_KEY_ENV} unset — /discord/interactions off"
            );
        }
    }

    if std::env::var_os("FAMILYCLAW_DATA_DIR").is_some_and(|v| !v.is_empty()) {
        println!("[OK]      env       FAMILYCLAW_DATA_DIR set");
    } else {
        println!("[WARN]    env       FAMILYCLAW_DATA_DIR unset — in-memory memory only");
    }

    // Durable backing surface: reports the actual kind tags that build_family
    // would wire up, and warns HONESTLY if the process would be in in-memory
    // mode — the at-most-once-under-crash guarantee needs the journal backing surface.
    // Warning != error (doesn't fail doctor), but the operator needs to know.
    let durability = build_durability_report().await?;
    println!("[INFO]     durability {}", durability.summary());
    if !durability.persistent {
        println!(
            "[WARN]    durability in-memory mode — at-most-once-under-crash guarantee needs the \
             journal backend; in-memory does NOT survive a process crash (set FAMILYCLAW_DATA_DIR)"
        );
    }
    println!("[INFO]     sandbox   {}", sandbox_label());
    println!("[INFO]     embedder  {}", embedder_label());

    if std::env::var_os("FAMILYCLAW_PROFILE_DIR").is_some_and(|v| !v.is_empty()) {
        println!("[OK]      env       FAMILYCLAW_PROFILE_DIR set");
    } else {
        println!("[WARN]    env       FAMILYCLAW_PROFILE_DIR unset — generic soul");
    }

    // /inject protection: empty token is OK only on loopback. Non-loopback
    // without a token fails doctor (same rule as `serve` fail-closed).
    let inject_bind = resolve_addr().ok().or_else(|| DEFAULT_ADDR.parse().ok());
    if cfg.gateway_token().trim().is_empty() {
        match inject_bind {
            Some(bind) if is_loopback_bind(bind) => {
                println!(
                    "[WARN]    inject    {GATEWAY_TOKEN_ENV} unset — POST /inject open (loopback-only)"
                );
            }
            Some(bind) => {
                println!(
                    "[FAIL]    inject    {GATEWAY_TOKEN_ENV} unset while binding non-loopback {bind}"
                );
                ok = false;
            }
            None => {
                println!("[FAIL]    inject    cannot resolve bind address for auth check");
                ok = false;
            }
        }
    } else {
        println!("[OK]      inject    {GATEWAY_TOKEN_ENV} set — POST /inject requires bearer");
    }

    if fix {
        println!("[FIX]     doctor --fix aktiivinen");
        let data_dir = std::env::var("FAMILYCLAW_DATA_DIR").unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".into());
            format!("{home}/.local/share/familyclaw")
        });
        if std::fs::create_dir_all(&data_dir).is_ok() {
            std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
            println!("[FIX]      data_dir  {data_dir}");
            match readiness::cleanup_stale_approval_tasks(std::path::Path::new(&data_dir), 0).await
            {
                Ok(cancelled) if cancelled > 0 => {
                    println!("[OK]      cleanup   cancelled {cancelled} needs_approval task(s)");
                }
                Ok(_) => {
                    println!("[OK]      cleanup   no pending needs_approval tasks");
                }
                Err(e) => {
                    println!("[WARN]    cleanup   pending tasks: {e}");
                }
            }
        }
        let config_path = FamilyConfig::find_path();
        if !config_path.exists() {
            if let Some(parent) = config_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let template = include_str!("../../../familyclaw.toml.example");
            if std::fs::write(&config_path, template).is_ok() {
                println!("[FIX]      config    wrote {}", config_path.display());
            }
        }
    }

    if ok {
        println!("doctor: ok");
        Ok(())
    } else {
        Err(FamilyClawError::invalid_input(
            "doctor: one or more checks failed",
        ))
    }
}

/// Interactive onboarding wizard: creates the TOML + data directory.
fn init_wizard() -> Result<()> {
    println!("FamilyClaw init — alle 5 min onboarding\n");

    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    let data_dir = format!("{home}/.local/share/familyclaw");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| FamilyClawError::config(format!("data_dir create failed: {e}")))?;
    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    println!("[OK] data_dir  {data_dir}");

    let config_path = FamilyConfig::find_path();
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if config_path.exists() {
        println!("[SKIP] config  {} exists", config_path.display());
    } else {
        let template = include_str!("../../../familyclaw.toml.example");
        std::fs::write(&config_path, template)
            .map_err(|e| FamilyClawError::config(format!("config write failed: {e}")))?;
        println!("[OK] config    {}", config_path.display());
    }

    println!("\nSeuraavat askeleet:");
    println!(
        "  1. Muokkaa {} (kanava, provider, avaimet)",
        config_path.display()
    );
    println!("  2. Aseta salaisuudet ympäristöön (DISCORD_BOT_TOKEN, OPENAI_API_KEY, …)");
    println!("  3. familyclaw-gateway doctor --fix");
    println!("  4. familyclaw-gateway serve");
    Ok(())
}

/// Parses the [`PLAN_ENV`] plan or returns the smoke-test default.
///
/// The JSON format is deliberately minimal: a list of nodes, each with
/// `id`/`title`/`description` and an optional `input` object. Dependencies,
/// roles, and capabilities are left at their defaults (a simple linear run),
/// so the entry point stays thin — more complex design belongs to the
/// library API ([`OrchestrationPlan`]).
fn load_orchestration_plan() -> OrchestrationPlan {
    let raw = std::env::var(PLAN_ENV).unwrap_or_default();
    if raw.trim().is_empty() {
        // Built-in smoke test: a single node that proves the run passes
        // through worker selection + the LiveTurnExecutor.
        return OrchestrationPlan::new(
            "smoke",
            vec![TaskNode::new(
                "n1",
                "smoke turn",
                "Produce a tiny JSON object proving the live orchestration path works.",
            )],
        );
    }
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => {
            let plan_id = v.get("id").and_then(|x| x.as_str()).unwrap_or("plan");
            let nodes: Vec<TaskNode> = v
                .get("nodes")
                .and_then(|n| n.as_array())
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .map(|(i, node)| {
                            let id = node
                                .get("id")
                                .and_then(|x| x.as_str())
                                .map_or_else(|| format!("n{i}"), ToString::to_string);
                            let title = node
                                .get("title")
                                .and_then(|x| x.as_str())
                                .unwrap_or("turn")
                                .to_string();
                            let desc = node
                                .get("description")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            TaskNode::new(id, title, desc)
                        })
                        .collect()
                })
                .unwrap_or_default();
            OrchestrationPlan::new(plan_id, nodes)
        }
        Err(e) => {
            warn!(error = %e, "kelvoton {PLAN_ENV} JSON — käytetään savutesti-oletusta");
            OrchestrationPlan::new(
                "smoke",
                vec![TaskNode::new(
                    "n1",
                    "smoke turn",
                    "fallback after invalid plan",
                )],
            )
        }
    }
}

/// Runs a multi-step orchestration plan once and prints a report.
///
/// Assembles the bridge, registers one Executor worker (online with a heartbeat),
/// builds a [`LiveTurnExecutor`] from the env resolver, and runs
/// [`Orchestrator::run_with`]. Prints the `RunReport` in JSON form.
///
/// # Errors
/// [`FamilyClawError`] if model resolution, worker registration, or the run fails.
async fn orchestrate() -> Result<()> {
    let cfg = FamilyConfig::load()?;
    let model = cfg.model().to_string();
    info!(%model, "orchestrate: kootaan bridge + LiveTurnExecutor");

    // 1. Bridge substrate (own EventBus/AgentRegistry/TaskBoard).
    let bridge = FamilyBridge::new();
    let now = familyclaw_core::time::now();

    // 2. Register one Executor worker and make it online (heartbeat),
    //    so select_worker sees it. Generic name (Layer A).
    let worker_id = familyclaw_core::AgentId::new();
    let worker = AgentInfo::new(worker_id, "worker-a", AgentRole::Executor, HostKind::Local);
    bridge.register_agent(worker).await.map_err(|e| {
        FamilyClawError::invalid_input(format!("orchestrate: register failed: {e}"))
    })?;
    bridge.heartbeat(worker_id, now).await.map_err(|e| {
        FamilyClawError::invalid_input(format!("orchestrate: heartbeat failed: {e}"))
    })?;

    // 3. LiveTurnExecutor with a real LLM chain (same resolver as serve).
    let resolver = build_resolver();
    let executor = LiveTurnExecutor::from_model(&ModelConfig::new(&model), &resolver)?;
    info!(primary = %executor.primary_model(), "LiveTurnExecutor valmis");

    // 4. Run the plan.
    let plan = load_orchestration_plan();
    let orchestrator = Orchestrator::new(bridge);
    let report = orchestrator.run_with(&plan, now, &executor).await?;

    // 5. Report to stdout. RunReport does not derive Serialize (a bridge
    //    type we don't change cross-crate), so we use Debug printing +
    //    a small JSON summary of completed nodes.
    println!("{report:#?}");
    info!(
        plan = %report.plan_id,
        "orchestrate: valmis"
    );
    Ok(())
}

/// Waits for the shutdown signal (`Ctrl-C`). Returns when the signal
/// arrives, which triggers axum's graceful shutdown.
async fn shutdown_signal() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => info!("Ctrl-C vastaanotettu — aloitetaan siisti sammutus"),
        Err(e) => error!("ctrl_c-kuuntelu epäonnistui: {e} — sammutetaan silti"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_addr_parses_to_expected_port() {
        // Make sure the default address parses as a SocketAddr on the right port.
        let parsed: SocketAddr = DEFAULT_ADDR.parse().expect("default addr parses");
        assert_eq!(parsed.port(), 8787);
        assert!(parsed.ip().is_loopback(), "oletus sitoutuu loopbackiin");
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        // Health on riippumaton busista: vastaa aina "ok".
        assert_eq!(healthz().await, "ok");
    }

    #[tokio::test]
    async fn readyz_is_unavailable_without_bus_and_ok_with_bus() {
        use axum::extract::State;
        use familyclaw_bus::ResonanceBus;

        // Ilman busia: ei valmis (503).
        let not_ready = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let (status, _) = readyz(State(not_ready)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

        // Busin kanssa: valmis (200).
        let bus = ResonanceBus::start(None).await.expect("bus");
        let ready = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let (status, _) = readyz(State(ready)).await;
        assert_eq!(status, StatusCode::OK);
        bus.stop();
    }

    #[test]
    fn build_router_constructs_without_panic() {
        // Reititin rakentuu (tyyppitason savutesti) molemmilla tiloilla.
        let _ = build_router(Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        }));
    }

    #[test]
    fn cli_definition_is_valid() {
        // The clap definition is internally consistent (surfaces derive errors).
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn cli_no_args_defaults_to_serve() {
        // No subcommand = serve (backward compatibility).
        let cli = Cli::parse_from(["familyclaw-gateway"]);
        assert!(
            matches!(cli.command.unwrap_or(Command::Serve), Command::Serve),
            "argumentiton kutsu pitää tarkoittaa serve"
        );
    }

    #[test]
    fn cli_parses_each_subcommand() {
        // serve/status/doctor parse into the expected variants.
        let serve = Cli::parse_from(["familyclaw-gateway", "serve"]);
        assert!(matches!(serve.command, Some(Command::Serve)));

        let status = Cli::parse_from(["familyclaw-gateway", "status"]);
        assert!(matches!(status.command, Some(Command::Status)));

        let doctor = Cli::parse_from(["familyclaw-gateway", "doctor"]);
        assert!(matches!(doctor.command, Some(Command::Doctor { fix: _ })));

        let orch = Cli::parse_from(["familyclaw-gateway", "orchestrate"]);
        assert!(matches!(orch.command, Some(Command::Orchestrate)));
    }

    #[test]
    fn plan_load_env_fallback_and_json_parsing() {
        // COMBINED test: [`PLAN_ENV`] is a PROCESS-WIDE environment variable,
        // so two separate test functions (one `remove_var`, another
        // `set_var`) would race when run in parallel and stomp on each
        // other's state. Both checks are done SEQUENTIALLY within the same
        // function — that way the env var isn't shared across threads and
        // the result doesn't depend on run order.

        // (a) Without PLAN_ENV -> the built-in single-node smoke test.
        std::env::remove_var(PLAN_ENV);
        let plan = load_orchestration_plan();
        assert_eq!(plan.id, "smoke");
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.nodes[0].id.as_str(), "n1");

        // (b) With PLAN_ENV set -> the JSON parses into nodes.
        let json = r#"{"id":"p","nodes":[
            {"id":"a","title":"A","description":"da"},
            {"id":"b","title":"B","description":"db"}
        ]}"#;
        std::env::set_var(PLAN_ENV, json);
        let plan = load_orchestration_plan();
        assert_eq!(plan.id, "p");
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[1].id.as_str(), "b");
        assert_eq!(plan.nodes[1].title, "B");

        // (c) Cleanup: restore the process state, so any other tests
        //     reading the same variable don't see leftover garbage.
        std::env::remove_var(PLAN_ENV);
        let plan = load_orchestration_plan();
        assert_eq!(plan.id, "smoke");
    }

    #[test]
    fn cli_rejects_unknown_subcommand() {
        // An unknown subcommand does not parse (clap returns an error).
        assert!(Cli::try_parse_from(["familyclaw-gateway", "bogus"]).is_err());
    }

    #[test]
    fn health_url_builds_http_scheme() {
        // The status helper correctly builds an http URL from the address + path.
        let addr: SocketAddr = "127.0.0.1:8787".parse().expect("addr");
        assert_eq!(
            health_url(addr, "/healthz"),
            "http://127.0.0.1:8787/healthz"
        );
        assert_eq!(health_url(addr, "/readyz"), "http://127.0.0.1:8787/readyz");
    }

    #[test]
    fn constant_time_eq_matches_only_identical_bytes() {
        // Constant-time comparison matches only same-length, byte-for-byte
        // identical strings (no short-circuit on the first difference).
        assert!(constant_time_eq(b"s3cret", b"s3cret"));
        assert!(!constant_time_eq(b"s3cret", b"s3crXt"));
        assert!(!constant_time_eq(b"s3cret", b"s3cre")); // different length
        assert!(constant_time_eq(b"", b""));
    }

    /// Helper: builds a [`HeaderMap`] containing an `Authorization` header.
    fn headers_with_auth(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            value.parse().expect("valid header value"),
        );
        h
    }

    #[test]
    fn inject_auth_no_token_configured_accepts() {
        // (c) No token configured -> the request is accepted without a header
        //     (backward-compatible open loopback default).
        let state = GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        };
        assert!(check_inject_auth(&state, &HeaderMap::new()).is_ok());
        // An extra header doesn't hurt when there's no protection.
        assert!(check_inject_auth(&state, &headers_with_auth("Bearer whatever")).is_ok());
    }

    #[test]
    fn operator_acl_disabled_allows_without_role_header() {
        std::env::remove_var("FAMILYCLAW_OPERATOR_ACL");
        assert!(
            check_operator_capability(&HeaderMap::new(), operator_caps::APPROVALS_DECIDE).is_ok()
        );
    }

    #[test]
    fn operator_acl_enabled_requires_role() {
        std::env::set_var("FAMILYCLAW_OPERATOR_ACL", "1");
        let denied = check_operator_capability(&HeaderMap::new(), operator_caps::APPROVALS_READ);
        assert_eq!(denied, Err(StatusCode::FORBIDDEN));
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-familyclaw-operator-role",
            "viewer".parse().expect("header"),
        );
        assert!(check_operator_capability(&headers, operator_caps::APPROVALS_READ).is_ok());
        assert_eq!(
            check_operator_capability(&headers, operator_caps::APPROVALS_DECIDE),
            Err(StatusCode::FORBIDDEN)
        );
        std::env::remove_var("FAMILYCLAW_OPERATOR_ACL");
    }

    #[test]
    fn resolve_inject_token_allows_empty_on_loopback() {
        let cfg = FamilyConfig::default();
        assert!(cfg.gateway_token().trim().is_empty());
        let loopback: SocketAddr = "127.0.0.1:8787".parse().expect("addr");
        let token = resolve_inject_token(&cfg, loopback).expect("loopback open ok");
        assert!(token.is_none());
    }

    #[test]
    fn resolve_inject_token_rejects_empty_on_non_loopback() {
        let cfg = FamilyConfig::default();
        let remote: SocketAddr = "0.0.0.0:8787".parse().expect("addr");
        let err = resolve_inject_token(&cfg, remote).expect_err("must fail-closed");
        let msg = err.to_string();
        assert!(
            msg.contains(GATEWAY_TOKEN_ENV) && msg.contains("non-loopback"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn is_loopback_bind_detects_wildcard_as_remote() {
        let loopback: SocketAddr = "127.0.0.1:1".parse().expect("addr");
        let wildcard: SocketAddr = "0.0.0.0:1".parse().expect("addr");
        assert!(is_loopback_bind(loopback));
        assert!(!is_loopback_bind(wildcard));
    }

    #[test]
    fn inject_auth_token_configured_correct_bearer_accepts() {
        // (a) Token configured + correct Bearer -> accepted.
        let state = GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: Some(Arc::from("s3cret-token")),
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        };
        assert!(check_inject_auth(&state, &headers_with_auth("Bearer s3cret-token")).is_ok());
    }

    #[test]
    fn inject_auth_token_configured_wrong_or_missing_rejects_401() {
        // (b) Token configured + wrong/missing Bearer -> 401.
        let state = GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: Some(Arc::from("s3cret-token")),
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        };
        // Wrong token.
        assert_eq!(
            check_inject_auth(&state, &headers_with_auth("Bearer wrong-token")),
            Err(StatusCode::UNAUTHORIZED)
        );
        // Header missing entirely.
        assert_eq!(
            check_inject_auth(&state, &HeaderMap::new()),
            Err(StatusCode::UNAUTHORIZED)
        );
        // Bearer prefix missing (bare token).
        assert_eq!(
            check_inject_auth(&state, &headers_with_auth("s3cret-token")),
            Err(StatusCode::UNAUTHORIZED)
        );
        // Correct prefix but empty token.
        assert_eq!(
            check_inject_auth(&state, &headers_with_auth("Bearer ")),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    // ---- Operator approval surface (suspend/resume bridge, roadmap §6 D2) ----

    /// Helper: gateway state with a **wired-up** action runtime (default skills)
    /// and no bearer protection. Also returns the shared handle for task submission.
    fn state_with_actions() -> (Arc<GatewayState>, Arc<Mutex<ActionRuntime>>) {
        let rt = ActionRuntime::with_default_skills().expect("default skills");
        let actions = Arc::new(Mutex::new(rt));
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        (state, actions)
    }

    /// Helper: submits a write-external task -> a pending approval is created.
    /// Returns the granted approval's identifier as a string (route form).
    async fn submit_pending(actions: &Arc<Mutex<ActionRuntime>>) -> String {
        use familyclaw_actions::GithubIssueDraftMock;
        let now = familyclaw_core::time::now();
        let mut rt = actions.lock().await;
        let submitted = rt
            .submit_task(
                GithubIssueDraftMock::skill_id(),
                serde_json::json!({ "bug_report": "Button does nothing" }),
                now,
            )
            .await
            .expect("submit");
        submitted
            .pending_approval
            .expect("write-external requires approval")
            .to_string()
    }

    /// Helper: submits a pending approval with an **injected `now` moment**,
    /// so the expiry boundary can be controlled deterministically in the test.
    ///
    /// The approval's `expires_at` is computed as `now + TTL` at submission
    /// time, so a `now` far in the past produces an approval that is already
    /// expired relative to the real current time — exactly what makes
    /// `approve` land in the `410 Gone` branch, without any clock-faking global state.
    async fn submit_pending_at(
        actions: &Arc<Mutex<ActionRuntime>>,
        now: familyclaw_core::time::Timestamp,
    ) -> String {
        use familyclaw_actions::GithubIssueDraftMock;
        let mut rt = actions.lock().await;
        let submitted = rt
            .submit_task(
                GithubIssueDraftMock::skill_id(),
                serde_json::json!({ "bug_report": "Button does nothing" }),
                now,
            )
            .await
            .expect("submit");
        submitted
            .pending_approval
            .expect("write-external requires approval")
            .to_string()
    }

    #[tokio::test]
    async fn pending_route_503_without_action_runtime() {
        // Without a wired-up action runtime -> 503 (no panic).
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let (status, _) = list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn pending_route_lists_redacted_without_payload() {
        let (state, actions) = state_with_actions();
        submit_pending(&actions).await;

        let (status, Json(body)) = list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(status, StatusCode::OK);
        let arr = body.as_array().expect("array body");
        assert_eq!(arr.len(), 1, "yksi odottava hyväksyntä");
        let item = &arr[0];
        // Only the three secret-free fields.
        assert!(item.get("approval_id").and_then(|v| v.as_str()).is_some());
        assert!(item
            .get("redacted_summary")
            .and_then(|v| v.as_str())
            .is_some());
        assert!(item.get("created_at").and_then(|v| v.as_str()).is_some());
        // NO raw payload ("bug_report"/"Button does nothing") and no payload field.
        let rendered = serde_json::to_string(&body).expect("serialize");
        assert!(!rendered.contains("bug_report"));
        assert!(!rendered.contains("Button does nothing"));
        assert!(!rendered.contains("payload"));
    }

    #[tokio::test]
    async fn pending_route_requires_bearer_when_configured() {
        // Token configured but no header -> 401, the list is not leaked.
        let (mut_state, actions) = state_with_actions();
        submit_pending(&actions).await;
        // Build a new state with the same runtime but with the token turned on.
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: Some(Arc::from("s3cret-token")),
            discord_public_key: None,
            actions: mut_state.actions.clone(),
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let (status, _) = list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn approve_route_invalid_id_is_400() {
        let (state, _actions) = state_with_actions();
        let (status, _) = approve_pending(
            State(state),
            HeaderMap::new(),
            Path("not-a-uuid".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn approve_route_unknown_id_is_404() {
        let (state, _actions) = state_with_actions();
        // A valid UUID but no pending approval -> 404 (fail-closed).
        let unknown = ApprovalId::new().to_string();
        let (status, _) = approve_pending(State(state), HeaderMap::new(), Path(unknown)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn approve_route_expired_id_is_410() {
        // An expired approval -> 410 Gone (a different reason than unknown = 404).
        // Submits a pending approval with a `now` moment far in the past
        // (epoch), so `expires_at = epoch + TTL` is already behind the real
        // current time. `approve_pending` reads the real
        // `familyclaw_core::time::now()` -> `now > expires_at` -> 410, without
        // the approval being consumed (fail-closed, no side effect).
        let (state, actions) = state_with_actions();
        let past = familyclaw_core::time::from_unix_secs(0).expect("epoch is a valid timestamp");
        let id = submit_pending_at(&actions, past).await;

        let (status, _) = approve_pending(State(state), HeaderMap::new(), Path(id)).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn approve_route_503_without_action_runtime() {
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let (status, _) = approve_pending(
            State(state),
            HeaderMap::new(),
            Path(ApprovalId::new().to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Phase 4: kill-switch route (POST /tasks/{id}/enabled) ──────────────

    fn state_without_scheduler() -> Arc<GatewayState> {
        Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        })
    }

    #[tokio::test]
    async fn killswitch_503_without_scheduler() {
        let state = state_without_scheduler();
        let (status, _) = set_task_enabled_route(
            State(state),
            HeaderMap::new(),
            Path(uuid::Uuid::from_u128(1).to_string()),
            Json(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn killswitch_400_on_bad_id_and_missing_body() {
        use familyclaw_scheduler::Scheduler;
        let sched = Arc::new(tokio::sync::Mutex::new(Scheduler::new()));
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: Some(sched),
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        // Invalid UUID -> 400.
        let (status, _) = set_task_enabled_route(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path("not-a-uuid".to_string()),
            Json(serde_json::json!({ "enabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Missing `enabled` -> 400.
        let (status, _) = set_task_enabled_route(
            State(state),
            HeaderMap::new(),
            Path(uuid::Uuid::from_u128(1).to_string()),
            Json(serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn killswitch_toggles_known_task_and_404s_unknown() {
        use familyclaw_actions::SkillId;
        use familyclaw_scheduler::{ScheduledTask, ScheduledTaskId, Scheduler};
        let mut s = Scheduler::new();
        let task_uuid = uuid::Uuid::from_u128(42);
        s.register(ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(task_uuid),
            SkillId::new(),
            serde_json::json!({}),
            chrono::Duration::seconds(60),
            "being",
        ));
        let sched = Arc::new(tokio::sync::Mutex::new(s));
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: Some(Arc::clone(&sched)),
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });

        // Known task -> 200, the state updates.
        let (status, _) = set_task_enabled_route(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(task_uuid.to_string()),
            Json(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            sched
                .lock()
                .await
                .task_enabled(ScheduledTaskId::from_uuid(task_uuid)),
            Some(false)
        );

        // Unknown task -> 404.
        let (status, _) = set_task_enabled_route(
            State(state),
            HeaderMap::new(),
            Path(uuid::Uuid::from_u128(999).to_string()),
            Json(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn killswitch_persists_to_agency_config() {
        use familyclaw_actions::SkillId;
        use familyclaw_scheduler::{AgencyConfig, ScheduledTask, ScheduledTaskId, Scheduler};
        let mut s = Scheduler::new();
        let task_uuid = uuid::Uuid::from_u128(77);
        let id = ScheduledTaskId::from_uuid(task_uuid);
        s.register(ScheduledTask::with_id(
            id,
            SkillId::new(),
            serde_json::json!({}),
            chrono::Duration::seconds(60),
            "being",
        ));
        let sched = Arc::new(tokio::sync::Mutex::new(s));

        // Isolated config path for this test.
        let dir = std::env::temp_dir().join("familyclaw-gw-agency-persist-test");
        let path = dir.join("agency.json");
        let _ = std::fs::remove_file(&path);

        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: Some(Arc::clone(&sched)),
            agency_config_path: Some(path.clone()),
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });

        // Disable via the route -> must be persisted to the file.
        let (status, _) = set_task_enabled_route(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(task_uuid.to_string()),
            Json(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // The disabled entry was written to the file.
        let cfg = AgencyConfig::load(&path).expect("load persisted");
        assert!(cfg.is_disabled(id), "kill-switch persistoitui configiin");

        // Re-enable via the route -> removed from the config.
        let (status, _) = set_task_enabled_route(
            State(state),
            HeaderMap::new(),
            Path(task_uuid.to_string()),
            Json(serde_json::json!({ "enabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let cfg = AgencyConfig::load(&path).expect("load");
        assert!(!cfg.is_disabled(id), "käyttöön otto poisti configista");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn approve_route_without_bus_is_503_and_does_not_consume() {
        // **Option A:** without a bus the gateway cannot hand the
        // continuation off to the agent (no agent is listening) -> an
        // honest 503, NOT a silent success. The pre-check is read-only ->
        // the approval is NOT consumed: it is still pending after the
        // request (can be retried).
        let (state, actions) = state_with_actions(); // bus: None
        let id = submit_pending(&actions).await;

        let (status, Json(body)) = approve_pending(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(id.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "ilman bussia operaattori-approve = 503 (Option A vaatii serve-tilan)"
        );
        assert!(
            body.get("error")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("serve mode")),
            "503-virheviesti mainitsee serve-tilan, oli: {body}"
        );

        // The approval was NOT consumed: it still shows up on the /approvals/pending list.
        let (list_status, Json(list_body)) =
            list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(list_status, StatusCode::OK);
        let arr = list_body.as_array().expect("array body");
        assert_eq!(
            arr.len(),
            1,
            "503-haaran jälkeen hyväksyntä on yhä odottavissa (ei kulutettu)"
        );
        assert_eq!(
            arr[0].get("approval_id").and_then(|v| v.as_str()),
            Some(id.as_str()),
            "sama odottava hyväksyntä yhä listalla"
        );
    }

    #[tokio::test]
    async fn approve_route_with_bus_publishes_and_does_not_consume() {
        // **Option A success path:** with a bus, the gateway VALIDATES +
        // PUBLISHES the `ResumeApproval` signal -> 200 `status: "resuming"`. The
        // gateway does NOT consume the approval (the agent consumes it);
        // without an agent in this test, the approval stays pending ->
        // proof that the gateway does not perform the side effect or consume the grant.
        use familyclaw_bus::ResonanceBus;

        let rt = ActionRuntime::with_default_skills().expect("default skills");
        let actions = Arc::new(Mutex::new(rt));
        let bus = ResonanceBus::start(None).await.expect("bus");
        let state = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let id = submit_pending(&actions).await;

        let (status, Json(body)) = approve_pending(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(id.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "bussin kanssa approve = 200");
        assert_eq!(
            body.get("status").and_then(|v| v.as_str()),
            Some("resuming"),
            "200-runko ilmoittaa asynkronisen jatkon (resuming), oli: {body}"
        );
        // NO outcome in the gateway (Option A): no task_id/awaiting fields.
        assert!(
            body.get("task_id").is_none() && body.get("awaiting_further_approval").is_none(),
            "Option A: gateway ei palauta lopputulosta (asynkroninen), oli: {body}"
        );

        // The gateway did NOT consume the approval — without an agent it is still pending.
        let (list_status, Json(list_body)) =
            list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(list_status, StatusCode::OK);
        let arr = list_body.as_array().expect("array body");
        assert_eq!(
            arr.len(),
            1,
            "gateway ei kuluta hyväksyntää (sen kuluttaa agentti) → yhä odottavissa"
        );

        bus.stop();
    }

    // ---- Prometheus metrics (GET /metrics) ----

    /// Helper: extracts the `Content-Type` header value from the header
    /// array returned by the handler, as a string (for test readability).
    fn content_type_of(headers: &[(axum::http::header::HeaderName, &'static str)]) -> &'static str {
        headers
            .iter()
            .find(|(name, _)| name == axum::http::header::CONTENT_TYPE)
            .map_or("", |(_, v)| v)
    }

    #[tokio::test]
    async fn metrics_route_503_without_registry() {
        // Without a wired-up registry -> 503 (no panic). The content type
        // stays text/plain even in the error response.
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let (status, headers, body) = metrics_handler(State(state)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(content_type_of(&headers).starts_with("text/plain"));
        assert!(body.contains("not configured"));
    }

    #[tokio::test]
    async fn metrics_route_200_text_plain_prometheus_body() {
        // Wire up the fleet default registry and increment one counter, so
        // the body contains both a TYPE line and a non-zero value. The export
        // is deterministic (name order), so the test cannot be flaky.
        let registry = MetricsRegistry::with_fleet_defaults();
        registry
            .counter(familyclaw_observability::COUNTER_TASKS_CREATED)
            .inc();
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: Some(registry),
            readiness: readiness::ReadinessProbe::default(),
        });

        let (status, headers, body) = metrics_handler(State(state)).await;

        // 200 + text/plain (the Prometheus exposition content type).
        assert_eq!(status, StatusCode::OK);
        assert!(
            content_type_of(&headers).starts_with("text/plain"),
            "Prometheus-vienti on text/plain, oli: {}",
            content_type_of(&headers)
        );

        // The body parses as a Prometheus exposition: at least one TYPE line
        // and a known metric line from the fleet defaults.
        assert!(body.contains("# TYPE tasks_created counter"));
        assert!(body.contains("tasks_created 1"));
        assert!(body.contains("# TYPE agents_online gauge"));
        assert!(body.contains("agents_online 0"));
        // Determinism: the export is in name order -> agents_online before
        // tasks_created (alphabetical order), so the output order is stable.
        let agents_at = body.find("agents_online").expect("agents_online present");
        let tasks_at = body.find("tasks_created").expect("tasks_created present");
        assert!(
            agents_at < tasks_at,
            "vienti on deterministisesti nimijärjestyksessä"
        );
    }

    /// **Real HTTP integration test:** binds the router assembled by
    /// [`build_router`] to a temporary loopback port (same pattern as
    /// [`serve`]), serves it in a background task, and fetches `GET /metrics`
    /// with a real HTTP client ([`reqwest`], already a dependency). This
    /// tests the whole chain: Router → route → handler → `Content-Type`
    /// header → Prometheus body over a real socket, not just the handler function directly.
    #[tokio::test]
    async fn metrics_route_http_integration_returns_prometheus_text() {
        let registry = MetricsRegistry::with_fleet_defaults();
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: Some(registry),
            readiness: readiness::ReadinessProbe::default(),
        });
        let app = build_router(state);

        // Bind port 0 -> the OS assigns a free port (parallel-safe, no
        // hardcoded port).
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");

        // Serve the router in the background; abort at the end of the test.
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/metrics"))
            .send()
            .await
            .expect("GET /metrics");

        // 200 OK.
        assert_eq!(resp.status().as_u16(), 200);
        // text/plain (the Prometheus exposition content type).
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.starts_with("text/plain"),
            "Content-Type pitää olla text/plain, oli: {content_type}"
        );

        // The body parses as a Prometheus exposition (TYPE line + a known metric).
        let body = resp.text().await.expect("body");
        assert!(
            body.contains("# TYPE"),
            "runko sisältää Prometheus-TYPE-rivin"
        );
        assert!(
            body.contains("agents_online"),
            "laivueen oletusmittari näkyy viennissä"
        );

        server.abort();
    }

    /// **End-to-end proof:** a live bridge event moves a counter on the SHARED
    /// registry, and `GET /metrics` reflects it (>0).
    ///
    /// This closes the gap flagged in review ("no end-to-end test that a
    /// live task moves a counter"): the same wiring as in [`serve`] —
    /// [`EventRecorder`] subscribes to [`FamilyBridge`] BEFORE the event and increments the
    /// `metrics.clone()` registry, and the EXACT same registry is given to
    /// [`GatewayState`]. An event is published (`create_task` +
    /// `Custom("task.completed")`), the recorder is drained, and it's proven that
    /// (a) the shared registry's counter increased and (b) the `GET /metrics` body shows
    /// the counter line with a value > 0.
    #[tokio::test]
    async fn live_bridge_event_moves_counter_on_shared_registry_and_metrics_reflects_it() {
        // 1. The SAME sharing pattern as in serve(): one registry, cloned
        //    for the recorder; the original goes to GatewayState.
        let metrics = MetricsRegistry::with_fleet_defaults();
        let bridge = FamilyBridge::new();
        // Subscribe BEFORE the event (EventBus only delivers post-subscription events).
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        // 2. Live bridge event: task creation (-> tasks_created) and
        //    completion (-> tasks_completed with the Custom label).
        bridge
            .create_task("live-task", None)
            .await
            .expect("create_task");
        bridge.bus().publish(familyclaw_bridge::Event::new(
            familyclaw_bridge::EventKind::Custom("task.completed".into()),
            None,
        ));

        // 3. Drain the events -> into the shared registry.
        let drained = recorder.drain_once().await;
        assert_eq!(drained, 2, "kaksi tapahtumaa käsiteltiin");

        // 4a. The shared registry's counter increased (same instance).
        assert_eq!(
            metrics
                .counter(familyclaw_observability::COUNTER_TASKS_CREATED)
                .get(),
            1
        );
        assert_eq!(
            metrics
                .counter(familyclaw_observability::COUNTER_TASKS_COMPLETED)
                .get(),
            1
        );

        // 4b. GET /metrics (the same registry in GatewayState) shows >0.
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: Some(metrics),
            readiness: readiness::ReadinessProbe::default(),
        });
        let (status, _headers, body) = metrics_handler(State(state)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("tasks_created 1"),
            "elävä tehtävä näkyy /metrics:ssä arvolla 1, runko:\n{body}"
        );
        assert!(
            body.contains("tasks_completed 1"),
            "valmistuminen näkyy /metrics:ssä arvolla 1, runko:\n{body}"
        );
    }

    /// Creates a process-unique temporary directory for journal tests.
    ///
    /// Does not depend on the `tempfile` crate (not a dev-dep here):
    /// combines the process id + a nanosecond timestamp, so parallel tests
    /// don't collide. The caller is responsible for cleanup.
    fn unique_data_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "familyclaw-durability-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("luo testihakemisto");
        dir
    }

    #[tokio::test]
    async fn durability_report_in_memory_reflects_default_kinds() {
        // Without a data directory: in-memory mode, both surfaces in-memory.
        let report = durability_report_for(None)
            .await
            .expect("in-memory report builds");
        assert!(!report.persistent, "ei data_diriä → ei persistentti");
        assert_eq!(report.dispatch_outbox_kind, "in-memory");
        assert_eq!(report.pending_store_kind, "in-memory");
    }

    #[tokio::test]
    async fn durability_report_persistent_reflects_journal_kinds() {
        // With a data directory: persistent mode, both surfaces journal.
        let dir = unique_data_dir("persistent");
        let dir_str = dir.to_str().expect("polku on UTF-8");
        let report = durability_report_for(Some(dir_str))
            .await
            .expect("persistent report builds");
        assert!(report.persistent, "data_dir set → persistentti");
        assert_eq!(report.dispatch_outbox_kind, "journal");
        assert_eq!(report.pending_store_kind, "journal");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn durability_summary_in_memory_contains_crash_survival_off() {
        // status/doctor display this line — in in-memory mode it must
        // contain "crash-survival OFF" + both surfaces' kind tags.
        let report = DurabilityReport {
            persistent: false,
            dispatch_outbox_kind: "in-memory",
            pending_store_kind: "in-memory",
        };
        let line = report.summary();
        assert!(
            line.contains("in-memory (no FAMILYCLAW_DATA_DIR)"),
            "in-memory-tila näkyy: {line}"
        );
        assert!(
            line.contains("crash-survival OFF"),
            "kaatumiskestävyyden puuttuminen näkyy: {line}"
        );
        assert!(
            line.contains("dispatch_outbox=in-memory") && line.contains("pending_store=in-memory"),
            "molemmat lajitunnisteet näkyvät: {line}"
        );
    }

    #[test]
    fn durability_summary_persistent_contains_journal_kinds() {
        // status/doctor line in persistent mode: no OFF warning, journal surfaces.
        let report = DurabilityReport {
            persistent: true,
            dispatch_outbox_kind: "journal",
            pending_store_kind: "journal",
        };
        let line = report.summary();
        assert!(line.contains("persistent (data_dir set)"), "tila: {line}");
        assert!(
            !line.contains("crash-survival OFF"),
            "persistentissä tilassa ei OFF-varoitusta: {line}"
        );
        assert!(
            line.contains("dispatch_outbox=journal") && line.contains("pending_store=journal"),
            "journal-lajitunnisteet näkyvät: {line}"
        );
    }

    /// Helper: builds the durability lines doctor shows from the report —
    /// the same formatting as in the `doctor()` function, so the warning
    /// logic is testable without a full `doctor()` run (which reads the
    /// process's global environment).
    fn doctor_durability_lines(report: &DurabilityReport) -> Vec<String> {
        let mut lines = vec![format!("[INFO]     durability {}", report.summary())];
        if !report.persistent {
            lines.push(
                "[WARN]    durability in-memory mode — at-most-once-under-crash guarantee needs the \
                 journal backend; in-memory does NOT survive a process crash (set FAMILYCLAW_DATA_DIR)"
                    .to_string(),
            );
        }
        lines
    }

    #[test]
    fn doctor_in_memory_emits_crash_survival_warning() {
        // doctor in in-memory mode: an HONEST warning (doesn't fail doctor).
        let report = DurabilityReport {
            persistent: false,
            dispatch_outbox_kind: "in-memory",
            pending_store_kind: "in-memory",
        };
        let lines = doctor_durability_lines(&report);
        let joined = lines.join("\n");
        assert!(
            joined.contains("[WARN]") && joined.contains("at-most-once-under-crash"),
            "doctor varoittaa kaatumiskestävyyden puuttumisesta: {joined}"
        );
        assert!(
            joined.contains("does NOT survive a process crash"),
            "varoitus on rehellinen kaatumisselviytymisestä: {joined}"
        );
    }

    #[test]
    fn doctor_persistent_emits_no_crash_survival_warning() {
        // doctor in persistent mode: only an INFO line, no crash warning.
        let report = DurabilityReport {
            persistent: true,
            dispatch_outbox_kind: "journal",
            pending_store_kind: "journal",
        };
        let lines = doctor_durability_lines(&report);
        let joined = lines.join("\n");
        assert!(
            joined.contains("[INFO]") && joined.contains("dispatch_outbox=journal"),
            "doctor näyttää journal-pinnat: {joined}"
        );
        assert!(
            !joined.contains("[WARN]"),
            "persistentissä tilassa ei kaatumisvaroitusta: {joined}"
        );
    }

    #[test]
    fn sandbox_label_matches_compiled_feature() {
        // The sandbox label follows the compile-time wasmtime feature.
        let label = sandbox_label();
        if cfg!(feature = "wasmtime") {
            assert_eq!(label, "wasmtime (host-import denial + fuel cap)");
        } else {
            assert_eq!(label, "none (noop)");
        }
    }

    #[test]
    fn embedder_label_reports_active_provider() {
        // Phase 3: status/doctor shows the active embedding provider's id + dim.
        let label = embedder_label();
        assert!(
            label.contains("deterministic-hash-v1"),
            "tarjoajan id: {label}"
        );
        assert!(label.contains("dim=256"), "ulottuvuus: {label}");
    }

    // ---- E2E: suspend → approve → resume → reply (Phase 1 §6, RED proof) ----

    /// **Scripted fake LLM** (raw TCP, OpenAI-compatible endpoint): returns
    /// the given JSON bodies in order, one per request. Same pattern as in
    /// `familyclaw-agent`'s tool-loop tests — no external mock library, no
    /// outbound network. Returns the base URL (`http://127.0.0.1:PORT/v1`).
    async fn spawn_scripted_llm_e2e(bodies: Vec<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scripted llm");
        let addr = listener.local_addr().expect("scripted llm addr");
        tokio::spawn(async move {
            for body in bodies {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}/v1")
    }

    /// `OpenAI` response body: **one tool call** in chat-completions wire
    /// form — `type:"function"` + a nested `function` whose `arguments`
    /// is a **JSON string**, and `content` is `null`. Mirrors a real provider.
    fn e2e_body_tool_call(id: &str, name: &str, arguments: &serde_json::Value) -> String {
        let arguments_str =
            serde_json::to_string(arguments).expect("arguments serialize to JSON string");
        serde_json::json!({
            "choices": [ {
                "message": {
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "tool_calls": [ {
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": arguments_str }
                    } ]
                },
                "finish_reason": "tool_calls"
            } ]
        })
        .to_string()
    }

    /// `OpenAI` response body: **plain text only** -> the tool loop stops (final response).
    fn e2e_body_text(text: &str) -> String {
        serde_json::json!({ "choices": [ { "message": { "content": text } } ] }).to_string()
    }

    /// An approval-gated **counting** test skill: increments a shared atomic
    /// counter on every execution -> a direct metric of how many times the
    /// side effect ran. Named `approval_skill` (the LLM tool call refers to
    /// the name).
    #[derive(Debug, Clone)]
    struct E2eCountingApprovalSkill {
        count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    /// The test skill's fixed, deterministic UUID (no `uuid!` macro).
    const E2E_APPROVAL_SKILL_UUID: u128 = 0x7e57_0000_0000_4000_8000_0000_0000_0001;

    #[async_trait::async_trait]
    impl familyclaw_actions::ActionExecutor for E2eCountingApprovalSkill {
        async fn execute(
            &self,
            request: familyclaw_actions::ActionRequest,
        ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(familyclaw_actions::ActionResult::success(
                "counting approval action executed",
                serde_json::json!({ "executed": true }),
                request.now,
            ))
        }
    }

    impl familyclaw_actions::Skill for E2eCountingApprovalSkill {
        fn manifest(&self) -> familyclaw_actions::manifest::SkillManifest {
            familyclaw_actions::manifest::SkillManifest {
                id: familyclaw_actions::SkillId::from_uuid(uuid::Uuid::from_u128(
                    E2E_APPROVAL_SKILL_UUID,
                )),
                name: "approval_skill".to_string(),
                version: "1.0.0".to_string(),
                description:
                    "Laskeva ulkoisesti kirjoittava toiminto (vaatii hyväksynnän, E2E-testi)."
                        .to_string(),
                permissions: vec![familyclaw_actions::policy::SkillPermission::WriteExternal],
                risk: familyclaw_actions::policy::ActionRisk::WriteExternal,
                approval_policy: familyclaw_actions::policy::ApprovalPolicy::RequireApproval,
                input_hint: None,
                output_hint: None,
                input_schema: familyclaw_actions::manifest::default_input_schema(),
                publisher: None,
                signature: None,
            }
        }
    }

    /// **End-to-end RED proof (Phase 1 §6 manual-gate gap):** proves that the
    /// operator's `POST /approvals/{id}/approve` **runs the action's side
    /// effect but does NOT drive the agent to a final response** — the turn
    /// does not resume (`turn_resumed`/`turn_answered` are missing) and no
    /// reply reaches the channel.
    ///
    /// The harness assembles a **real agent** in-crate (scripted LLM + a
    /// shared `ActionRuntime` with the counting approval skill + a shared
    /// `AuditCollector` + a captured reply sink). The same
    /// `Arc<Mutex<ActionRuntime>>` and the same `Arc<AuditCollector>` are given
    /// to both the agent (`with_actions` / `with_turn_audit`) and
    /// `GatewayState` — the operator and the agent share EXACTLY the same
    /// locked state, as in production's `build_family` wiring.
    ///
    /// The turn is suspended by calling `agent.think()` directly
    /// (deterministic, the same pattern as
    /// `resume_approved_completes_turn_side_effect_runs_once`); the agent is
    /// then spawned as an actor and the bus is given to `GatewayState`, so a
    /// later fix (`BusMessage::ResumeApproval` → actor handler →
    /// `resume_approved` → reply sink) can turn assertion (e) green without
    /// touching this harness.
    ///
    /// Assertions (a)-(d) PASS; (e) FAILS because the reply never arrives
    /// and `turn_resumed`/`turn_answered` never occur — this is the proof of the gap.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn e2e_suspend_approve_resume_reply() {
        use familyclaw_agent::{new_reply_channel, Agent, ErasedMemoryStore, ThinkOutcome};
        use familyclaw_bus::{BusMessage, ResonanceBus};
        use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
        use familyclaw_memory::LocalJsonStore;
        use std::sync::atomic::Ordering::SeqCst;

        // 1. Bus (same instance for GatewayState — the upcoming ResumeApproval publish).
        let bus = ResonanceBus::start(None).await.expect("bus");

        // 2. Scripted LLM: first an approval-requiring tool call (suspend),
        //    then (during resume) the final text. The second body is NOT read
        //    in this RED test, because the gateway does not run the resume — that's the point.
        // The payload contains a SENTINEL string (a fake secret, NOT a real key or
        //    operator name) whose redacted summary must STRIP it — proves in (b)
        //    that /approvals/pending does not leak the raw payload/secrets.
        let api = spawn_scripted_llm_e2e(vec![
            e2e_body_tool_call(
                "call_approve",
                "approval_skill",
                &serde_json::json!({ "q": "ship", "secret": "sk-SENTINEL-DO-NOT-LEAK" }),
            ),
            e2e_body_text("hyväksytty toiminto valmis"),
        ])
        .await;

        // 3. Shared ActionRuntime with the counting approval skill.
        let side_effect_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut rt = ActionRuntime::new();
        rt.register_skill(E2eCountingApprovalSkill {
            count: std::sync::Arc::clone(&side_effect_count),
        })
        .expect("register approval_skill");
        let actions: Arc<Mutex<ActionRuntime>> = Arc::new(Mutex::new(rt));

        // 4. Shared turn-audit collector.
        let turn_audit: Arc<AuditCollector> = Arc::new(AuditCollector::new());

        // 5. Captured reply sink: this is how we OBSERVE whether the final
        //    response reaches the channel. In production the runtime owns the
        //    recv end and pumps Channel::send.
        let (sink, mut reply_rx) = new_reply_channel();

        // 6. A real agent with the scripted LLM + shared handles (same
        //    wiring as build_family). The reply target is a static fallback.
        let config = AgentConfig::new("e2e_agent", ModelConfig::new("scripted/model"));
        let soul = Soul::from_essence("I am the E2E agent.".to_string());
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable ctx");
        let llm_cfg = familyclaw_agent::llm::LlmConfig::new(&api, "test-key", "scripted-model")
            .with_request_timeout_ms(2_000)
            .with_connect_timeout_ms(2_000);
        let agent = Agent::new(
            config,
            soul,
            memory,
            durable,
            bus.clone(),
            Some(llm_cfg),
            None,
        )
        .with_actions(Arc::clone(&actions))
        .with_turn_audit(Arc::clone(&turn_audit))
        .with_reply_sink(sink)
        .with_reply_target("e2e-channel");

        // 7. Run the turn -> the tool loop suspends on the approval-requiring tool.
        //    This produces a REAL turn_suspended audit + a pending approval
        //    on the SHARED ActionRuntime + a resumable turn on the resumable surface.
        let out = agent
            .think(&BusMessage::text("ship it"))
            .await
            .expect("think suspends");
        let approval_id = match out {
            ThinkOutcome::Suspended { approval_id, .. } => approval_id,
            other => panic!("odotettiin Suspended, sai: {other:?}"),
        };
        // The side effect has NOT run yet (approval not granted).
        assert_eq!(
            side_effect_count.load(SeqCst),
            0,
            "approval-gated side effect must NOT run before approve"
        );

        // 8. Spawn the agent as an actor (kept alive) — the upcoming
        //    ResumeApproval bus signal reaches exactly this mailbox. The RED
        //    test does not send it yet; we keep the actor alive for the harness's fidelity.
        let _actor = agent.spawn().await.expect("spawn agent actor");

        // 9. GatewayState shares the SAME actions + turn_audit handles and the same bus.
        let state = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: Some(Arc::clone(&turn_audit)),
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        // (a) The turn was suspended: no reply yet, and /turns/audit contains
        //     the turn_suspended event.
        assert!(
            reply_rx.try_recv().is_err(),
            "(a) suspendin jälkeen ei saa olla replyä reply-sinkissä"
        );
        let audit_body: String = client
            .get(format!("{base}/turns/audit"))
            .send()
            .await
            .expect("GET /turns/audit (a)")
            .text()
            .await
            .expect("audit body (a)");
        assert!(
            audit_body.contains("turn_suspended"),
            "(a) audit-jälki sisältää turn_suspended:n, oli:\n{audit_body}"
        );

        // (b) /approvals/pending returns the approval_id + a redacted
        //     summary, and does NOT leak secrets / operator names / private
        //     absolute paths (Layer B spirit).
        let pending_resp = client
            .get(format!("{base}/approvals/pending"))
            .send()
            .await
            .expect("GET /approvals/pending (b)");
        assert_eq!(pending_resp.status().as_u16(), 200, "(b) pending = 200");
        let pending_body = pending_resp.text().await.expect("pending body (b)");
        assert!(
            pending_body.contains(&approval_id.to_string()),
            "(b) pending sisältää approval_id:n, oli:\n{pending_body}"
        );
        // POSITIVE redaction assertion: the summary is EXACTLY the redacted
        // form from `ActionRuntime::pending_summary` (skill name only),
        // NOT the raw payload. This is both leak-free in the source (no
        // operator names as literals) and more meaningful than a mere
        // negative check: it ties the test to the redacted representation.
        assert!(
            pending_body.contains("taito 'approval_skill' odottaa ihmisen hyväksyntää"),
            "(b) pending sisältää redaktoidun tiivistelmän (vain taidon nimi), oli:\n{pending_body}"
        );
        // Negative leak checks: no key-shaped secret
        // (sk-/Bearer/test-key), no SENTINEL fake secret, no raw payload
        // (value `ship`, key `"q"`/`"secret"`), no private absolute path.
        // SENTINEL ACTIVELY proves that redaction strips secrets embedded in
        // the payload — not a cosmetic check.
        let lowered = pending_body.to_lowercase();
        assert!(
            !lowered.contains("sk-")
                && !lowered.contains("bearer ")
                && !lowered.contains("test-key"),
            "(b) ei avain-muotoista salaisuutta: {pending_body}"
        );
        assert!(
            !pending_body.contains("SENTINEL"),
            "(b) redaktion pitää karsia payloadiin upotettu SENTINEL-tekosalaisuus: {pending_body}"
        );
        assert!(
            !pending_body.contains("ship")
                && !pending_body.contains("\"q\"")
                && !pending_body.contains("\"secret\""),
            "(b) ei raakaa payloadia: {pending_body}"
        );
        assert!(
            !pending_body.contains("C:\\") && !pending_body.contains("/home/"),
            "(b) ei yksityistä absoluuttista polkua: {pending_body}"
        );

        // (c) POST /approvals/{id}/approve -> 200. **Option A:** 200 means
        //     "the approval was received and handed off to the agent" — NOT
        //     that the continuation is already complete. The side effect +
        //     response run asynchronously on the agent's resume path
        //     (the ResumeApproval bus signal -> handle_resume_signal).
        let approve_resp = client
            .post(format!("{base}/approvals/{approval_id}/approve"))
            .send()
            .await
            .expect("POST approve (c)");
        assert_eq!(approve_resp.status().as_u16(), 200, "(c) approve = 200");
        let approve_body: serde_json::Value =
            approve_resp.json().await.expect("approve body (c) json");
        assert_eq!(
            approve_body.get("status").and_then(|v| v.as_str()),
            Some("resuming"),
            "(c) 200-runko ilmoittaa asynkronisen jatkon (resuming), oli:\n{approve_body}"
        );

        // (d)+(e) **Asynchronous, bounded poll:** because the side effect +
        //     response now run in the agent AFTER the 200 returns (Option A), we
        //     cannot assert synchronously. Poll for at most ~3s (60 × 50ms) until
        //     ALL of these are true: the final reply arrives at the reply sink,
        //     the side-effect counter == 1, and /turns/audit contains turn_resumed and turn_answered.
        //     Bounded (not infinite) -> the test stays deterministic and fast.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut reply_body: Option<String> = None;
        let mut final_audit;
        loop {
            // Drain the reply sink non-blockingly (the agent pushes here when
            // the resume completes). Keep the first response that arrives.
            if reply_body.is_none() {
                if let Ok(msg) = reply_rx.try_recv() {
                    reply_body = Some(msg.body);
                }
            }
            final_audit = client
                .get(format!("{base}/turns/audit"))
                .send()
                .await
                .expect("GET /turns/audit (e)")
                .text()
                .await
                .expect("audit body (e)");

            let done = reply_body.is_some()
                && side_effect_count.load(SeqCst) == 1
                && final_audit.contains("turn_resumed")
                && final_audit.contains("turn_answered");
            if done || std::time::Instant::now() >= deadline {
                break; // done or timeout -> the assertions below report the observation
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // These assertions are the FORMER RED line: they are now GREEN,
        // because operator-approve publishes the ResumeApproval and the agent
        // resumes the turn to completion (resume_approved -> reply sink). Do not weaken them.
        assert_eq!(
            reply_body.as_deref(),
            Some("hyväksytty toiminto valmis"),
            "(e) lopullisen vastauksen pitää tavoittaa reply-sink approven jälkeen \
             (sai: {reply_body:?}); side_effect={}, audit:\n{final_audit}",
            side_effect_count.load(SeqCst)
        );
        // (d) The side effect ran EXACTLY ONCE (eventually-exactly-once).
        assert_eq!(
            side_effect_count.load(SeqCst),
            1,
            "(d) approval-gated side effect must run exactly once (async, polled)"
        );
        assert!(
            final_audit.contains("turn_resumed"),
            "(e) audit-jäljen pitää sisältää turn_resumed approven jälkeen, oli:\n{final_audit}"
        );
        assert!(
            final_audit.contains("turn_answered"),
            "(e) audit-jäljen pitää sisältää turn_answered approven jälkeen, oli:\n{final_audit}"
        );

        // **No double-firing:** wait a few more cycles and verify that the
        // side effect stays at 1 and no second response arrives (the approval
        // is single-use -> the agent cannot run it twice).
        for _ in 0..5 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert_eq!(
                side_effect_count.load(SeqCst),
                1,
                "side-effect ei saa laueta toista kertaa (kertakäyttöinen hyväksyntä)"
            );
            assert!(
                reply_rx.try_recv().is_err(),
                "toista vastausta ei saa saapua (ei kaksoislaukaisua)"
            );
        }

        server.abort();
        bus.stop();
    }

    /// SF1 (GPT-5.5 review): **two CONCURRENT** `POST /approvals/{id}/approve`
    /// requests for the same approval may trigger the side effect **at most
    /// once**. The earlier `e2e_suspend_approve_resume_reply` proved
    /// sequential no-double-firing; this proves that a race between two
    /// simultaneous HTTP requests does not break the single-use approval.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn concurrent_double_approve_fires_side_effect_at_most_once() {
        use familyclaw_agent::{new_reply_channel, Agent, ErasedMemoryStore, ThinkOutcome};
        use familyclaw_bus::{BusMessage, ResonanceBus};
        use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
        use familyclaw_memory::LocalJsonStore;
        use std::sync::atomic::Ordering::SeqCst;

        let bus = ResonanceBus::start(None).await.expect("bus");
        let api = spawn_scripted_llm_e2e(vec![
            e2e_body_tool_call(
                "call_approve",
                "approval_skill",
                &serde_json::json!({ "q": "ship" }),
            ),
            e2e_body_text("hyväksytty toiminto valmis"),
        ])
        .await;

        let side_effect_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut rt = ActionRuntime::new();
        rt.register_skill(E2eCountingApprovalSkill {
            count: std::sync::Arc::clone(&side_effect_count),
        })
        .expect("register approval_skill");
        let actions: Arc<Mutex<ActionRuntime>> = Arc::new(Mutex::new(rt));
        let turn_audit: Arc<AuditCollector> = Arc::new(AuditCollector::new());
        let (sink, mut reply_rx) = new_reply_channel();

        let config = AgentConfig::new("e2e_agent", ModelConfig::new("scripted/model"));
        let soul = Soul::from_essence("I am the E2E agent.".to_string());
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable ctx");
        let llm_cfg = familyclaw_agent::llm::LlmConfig::new(&api, "test-key", "scripted-model")
            .with_request_timeout_ms(2_000)
            .with_connect_timeout_ms(2_000);
        let agent = Agent::new(
            config,
            soul,
            memory,
            durable,
            bus.clone(),
            Some(llm_cfg),
            None,
        )
        .with_actions(Arc::clone(&actions))
        .with_turn_audit(Arc::clone(&turn_audit))
        .with_reply_sink(sink)
        .with_reply_target("e2e-channel");

        let out = agent
            .think(&BusMessage::text("ship it"))
            .await
            .expect("think suspends");
        let approval_id = match out {
            ThinkOutcome::Suspended { approval_id, .. } => approval_id,
            other => panic!("odotettiin Suspended, sai: {other:?}"),
        };
        assert_eq!(
            side_effect_count.load(SeqCst),
            0,
            "ei sivuvaikutusta ennen approvea"
        );

        let _actor = agent.spawn().await.expect("spawn agent actor");
        let state = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: Some(Arc::clone(&turn_audit)),
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/approvals/{approval_id}/approve");

        // Two CONCURRENT approve requests for the same id.
        let (r1, r2) = tokio::join!(client.post(&url).send(), client.post(&url).send(),);
        let s1 = r1.expect("POST approve #1").status().as_u16();
        let s2 = r2.expect("POST approve #2").status().as_u16();
        // Exactly one request may consume the single-use approval (200); the
        // other sees it already consumed (404 Not Found) or also 200 if
        // serialization allows it — but the side effect below is AT MOST 1 in EVERY case.
        let oks = u8::from(s1 == 200) + u8::from(s2 == 200);
        assert!(oks >= 1, "ainakin yksi approve onnistuu (sai {s1}/{s2})");

        // Wait for the resume to complete, then verify side-effect == 1 and it doesn't rise further.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if side_effect_count.load(SeqCst) >= 1 || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(
                side_effect_count.load(SeqCst) <= 1,
                "samanaikainen kaksois-approve EI saa laukaista sivuvaikutusta kahdesti (oli {})",
                side_effect_count.load(SeqCst)
            );
        }
        assert_eq!(
            side_effect_count.load(SeqCst),
            1,
            "sivuvaikutus ajetaan tasan kerran myös samanaikaisen kaksois-approven alla"
        );
        // Only one final response (no double-firing on the reply path).
        let mut replies = 0u8;
        while reply_rx.try_recv().is_ok() {
            replies += 1;
        }
        assert!(replies <= 1, "korkeintaan yksi vastaus (sai {replies})");

        server.abort();
        bus.stop();
    }

    /// SF2 (GPT-5.5 review): a negative route regression test that GUARDS the
    /// axum 0.7 fix (`{approval_id}` → `:approval_id`). If someone reverted the
    /// route to brace syntax, the literal path segment would be interpreted
    /// as a literal and would not capture an arbitrary id -> this test would fail.
    ///
    /// Proof: POST to an arbitrary `:approval_id` value does NOT return 404
    /// "route not found" (the route matches and the handler runs -> 400/404/503
    /// per its own validation), whereas an unknown path returns 404. We use
    /// the 503 distinction: without an action runtime the handler responds
    /// 503, so a matched route produces 503 and an unmatched one 404.
    #[tokio::test]
    async fn approve_route_captures_arbitrary_id_not_literal_braces() {
        // GatewayState WITHOUT an action runtime -> approve_pending responds
        // 503 WHEN the route matches. (The bearer check is skipped since inject_token = None.)
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        // (1) An arbitrary id matches the `:approval_id` capture -> the handler
        //     runs -> 503 (no action runtime). If the route were the literal
        //     `{approval_id}`, this would NOT match -> 404. 503 proves the capture works.
        let captured = client
            .post(format!("{base}/approvals/any-arbitrary-id-123/approve"))
            .send()
            .await
            .expect("POST arbitrary id");
        assert_eq!(
            captured.status().as_u16(),
            503,
            "mielivaltainen id matchaa reitin (handler ajaa, 503 ilman runtimea); \
             404 tarkoittaisi paluuta literaaliin {{approval_id}}-bugiin"
        );

        // (2) Control: a completely unknown path returns 404 (the router
        //     works correctly, doesn't match everything).
        let unknown = client
            .post(format!("{base}/nonexistent/path"))
            .send()
            .await
            .expect("POST unknown path");
        assert_eq!(
            unknown.status().as_u16(),
            404,
            "tuntematon polku palauttaa 404 (router ei matchaa sokeasti kaikkea)"
        );

        server.abort();
    }

    /// **P0 approval regression (race):** two SIMULTANEOUS HTTP-level
    /// `POST /approvals/{id}/approve` requests for the same approval must trigger
    /// the external side effect **EXACTLY ONCE**. Uses the SAME genuine E2E harness
    /// as [`e2e_suspend_approve_resume_reply`] (a real axum router + socket +
    /// shared `ActionRuntime` with a counting approval skill + captured reply sink +
    /// shared `AuditCollector`).
    ///
    /// **Documented semantics (Option A, same as production):** the approval is
    /// single-use; the first request consumes it and returns `200 resuming`
    /// (the side effect + response run asynchronously on the agent's resume path). The
    /// second concurrent request either (a) sees the approval already consumed and
    /// returns a safe non-success (404), OR (b) also returns 200 if it arrives
    /// before consumption — but in either case the external side effect
    /// is dispatched AT MOST ONCE (the single-use approval is serialized behind
    /// the shared `Mutex<ActionRuntime>` lock). The test confirms: exactly one 200
    /// is NOT required (concurrency can produce 1 or 2 × 200), but the side effect
    /// == 1, exactly one `turn_resumed`/`turn_answered`, at most one final reply,
    /// and the actor does not crash/panic.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn approval_double_post_race_runs_side_effect_once() {
        use familyclaw_agent::{new_reply_channel, Agent, ErasedMemoryStore, ThinkOutcome};
        use familyclaw_bus::{BusMessage, ResonanceBus};
        use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
        use familyclaw_memory::LocalJsonStore;
        use std::sync::atomic::Ordering::SeqCst;

        // 1. Bus + scripted LLM (suspend tool call → final text) — the same
        //    pattern as e2e_suspend_approve_resume_reply.
        let bus = ResonanceBus::start(None).await.expect("bus");
        let api = spawn_scripted_llm_e2e(vec![
            e2e_body_tool_call(
                "call_approve",
                "approval_skill",
                &serde_json::json!({ "q": "ship" }),
            ),
            e2e_body_text("hyväksytty toiminto valmis"),
        ])
        .await;

        // 2. Shared ActionRuntime with a counting approval skill (side-effect meter).
        let side_effect_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut rt = ActionRuntime::new();
        rt.register_skill(E2eCountingApprovalSkill {
            count: std::sync::Arc::clone(&side_effect_count),
        })
        .expect("register approval_skill");
        let actions: Arc<Mutex<ActionRuntime>> = Arc::new(Mutex::new(rt));
        let turn_audit: Arc<AuditCollector> = Arc::new(AuditCollector::new());
        let (sink, mut reply_rx) = new_reply_channel();

        // 3. A real agent with shared handles (the same wiring as build_family).
        let config = AgentConfig::new("e2e_agent", ModelConfig::new("scripted/model"));
        let soul = Soul::from_essence("I am the E2E agent.".to_string());
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable ctx");
        let llm_cfg = familyclaw_agent::llm::LlmConfig::new(&api, "test-key", "scripted-model")
            .with_request_timeout_ms(2_000)
            .with_connect_timeout_ms(2_000);
        let agent = Agent::new(
            config,
            soul,
            memory,
            durable,
            bus.clone(),
            Some(llm_cfg),
            None,
        )
        .with_actions(Arc::clone(&actions))
        .with_turn_audit(Arc::clone(&turn_audit))
        .with_reply_sink(sink)
        .with_reply_target("e2e-channel");

        // 4. Run the turn → suspends on one pending approval.
        let out = agent
            .think(&BusMessage::text("ship it"))
            .await
            .expect("think suspends");
        let approval_id = match out {
            ThinkOutcome::Suspended { approval_id, .. } => approval_id,
            other => panic!("expected Suspended, got: {other:?}"),
        };
        assert_eq!(
            side_effect_count.load(SeqCst),
            0,
            "the side effect must NOT run before approval"
        );

        // 5. Spawn the agent as an actor (the ResumeApproval signal reaches its
        //    mailbox) + GatewayState shares the SAME actions/turn_audit/bus handle.
        let _actor = agent.spawn().await.expect("spawn agent actor");
        let state = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: Some(Arc::clone(&turn_audit)),
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/approvals/{approval_id}/approve");

        // 6. TWO SIMULTANEOUS approve requests for the same id (a real socket).
        let (r1, r2) = tokio::join!(client.post(&url).send(), client.post(&url).send());
        let s1 = r1.expect("POST approve #1").status().as_u16();
        let s2 = r2.expect("POST approve #2").status().as_u16();
        // Semantics (documented above): at least one 200 (resuming); the other either
        // 200 (arrived before consumption) or 404 (already consumed). Neither is 5xx.
        let oks = u8::from(s1 == 200) + u8::from(s2 == 200);
        assert!(
            oks >= 1,
            "ainakin yksi rinnakkainen approve onnistuu (sai {s1}/{s2})"
        );
        assert!(
            s1 < 500 && s2 < 500,
            "kumpikaan rinnakkainen approve ei saa tuottaa 5xx-kaatumista (sai {s1}/{s2})"
        );
        for s in [s1, s2] {
            assert!(
                s == 200 || s == 404,
                "rinnakkainen approve on joko 200 (resuming) tai 404 (jo kulutettu), oli {s}"
            );
        }

        // 7. Wait for the asynchronous resume to finish, then verify the invariants.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut reply_count = 0u8;
        loop {
            while reply_rx.try_recv().is_ok() {
                reply_count += 1;
            }
            let audit = client
                .get(format!("http://{addr}/turns/audit"))
                .send()
                .await
                .expect("GET /turns/audit")
                .text()
                .await
                .expect("audit body");
            let done = side_effect_count.load(SeqCst) >= 1
                && audit.contains("turn_resumed")
                && audit.contains("turn_answered");
            if done || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // 8. **Hard assertions.** The side effect ran EXACTLY ONCE — a race between two
        //    HTTP requests does not break the single-use approval.
        assert_eq!(
            side_effect_count.load(SeqCst),
            1,
            "ulkoinen sivuvaikutus dispatchataan TASAN KERRAN myös rinnakkaisen \
             kaksois-approven alla (sai {})",
            side_effect_count.load(SeqCst)
        );
        // The audit must show EXACTLY one effective resumed turn (one turn_resumed
        // + one turn_answered) — not two continuations.
        let final_audit = client
            .get(format!("http://{addr}/turns/audit"))
            .send()
            .await
            .expect("GET /turns/audit (final)")
            .text()
            .await
            .expect("audit body (final)");
        assert_eq!(
            final_audit.matches("turn_resumed").count(),
            1,
            "tasan yksi turn_resumed (hyväksyntä ei jatka vuoroa kahdesti), audit:\n{final_audit}"
        );
        assert_eq!(
            final_audit.matches("turn_answered").count(),
            1,
            "tasan yksi turn_answered (yksi lopullinen vastaus), audit:\n{final_audit}"
        );

        // 9. Drain the reply sink for a few more cycles: wait until EXACTLY one
        //    final reply arrives (the audit already shows one turn_answered), and
        //    confirm the side effect does not fire again and no second reply arrives.
        //    `turn_answered == 1` (above) guarantees the response was produced; here
        //    we wait for it to also reach the reply sink exactly once.
        for _ in 0..40 {
            while reply_rx.try_recv().is_ok() {
                reply_count += 1;
            }
            assert_eq!(
                side_effect_count.load(SeqCst),
                1,
                "sivuvaikutus ei saa laueta toista kertaa (kertakäyttöinen hyväksyntä)"
            );
            if reply_count >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // Drain any trailing replies and require EXACTLY one: not zero
        // (the response was produced) and not two (no double-fire).
        while reply_rx.try_recv().is_ok() {
            reply_count += 1;
        }
        assert_eq!(
            reply_count, 1,
            "tasan yksi lopullinen reply tavoittaa reply-sinkin (sai {reply_count})"
        );

        server.abort();
        bus.stop();
    }

    /// **P0 approval regression (route syntax, axum 0.7):** guards that the approval
    /// route is registered as a `:approval_id` capture and NOT as a literal
    /// brace segment. axum 0.7 / matchit 0.7 interprets a brace-form segment
    /// as a literal path segment, so a brace route does NOT match real ids.
    ///
    /// **Empirically confirmed semantics (this repo, axum 0.7.9 / matchit 0.7.3):**
    /// - The correct `:approval_id` route: an arbitrary id (including a real UUID) MATCHES →
    ///   the handler runs → 503 (no actions runtime). A literal brace
    ///   segment also matches, since it is just one captured value → 503.
    /// - The BUGGY brace route (literal): ALL requests — both a real UUID AND
    ///   a literal brace path — return 404 (the literal doesn't match a real
    ///   id; empirically verified with a probe before writing this test).
    ///
    /// The decisive discriminator for detecting the regression is therefore **a REAL UUID
    /// matches (503, not 404)**. If someone reverts the route to the brace form, a real UUID
    /// starts returning 404 → this test goes red (verified via a temp revert). The brace route's
    /// behavior is documented and it is confirmed it does not produce a successful approval.
    #[tokio::test]
    async fn approval_literal_braces_route_does_not_match_on_axum_07() {
        // GatewayState WITHOUT an actions runtime → a matched route responds 503,
        // an unmatched route responds 404. (Bearer is bypassed when inject_token = None.)
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
            readiness: readiness::ReadinessProbe::default(),
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        // (1) DECISIVE: a real UUID reaches the approval handler → 503 (no runtime).
        //     If the route were a literal brace form, this would return 404 and the test
        //     would go red. THIS line enforces the `:approval_id` syntax.
        let real_uuid = "11111111-1111-4111-8111-111111111111";
        let real = client
            .post(format!("{base}/approvals/{real_uuid}/approve"))
            .send()
            .await
            .expect("POST real uuid");
        assert_eq!(
            real.status().as_u16(),
            503,
            "oikea UUID tavoittaa approval-handlerin (503 ilman runtimea); 404 \
             tarkoittaisi paluuta literaaliin brace-reittiin (axum 0.7 -bugi)"
        );

        // (2) The literal brace path `/approvals/{{approval_id}}/approve` must NOT
        //     produce a SUCCESSFUL approval. Under the correct `:approval_id` route it
        //     matches as a captured value and ends up at 503 (no runtime) — NOT 2xx.
        //     This proves the literal brace is not a specially-handled
        //     success path.
        let braces = client
            .post(format!("{base}/approvals/{{approval_id}}/approve"))
            .send()
            .await
            .expect("POST literal braces");
        let braces_status = braces.status().as_u16();
        assert!(
            !(200..300).contains(&braces_status),
            "kirjaimellinen brace-polku ei saa tuottaa onnistunutta hyväksyntää (oli {braces_status})"
        );

        // (3) Control: a completely unknown path returns 404 (the router doesn't
        //     blindly match everything) — confirms the 503 above is a genuine route
        //     match and not a catch-all.
        let unknown = client
            .post(format!("{base}/nonexistent/path"))
            .send()
            .await
            .expect("POST unknown path");
        assert_eq!(
            unknown.status().as_u16(),
            404,
            "tuntematon polku palauttaa 404 (router ei matchaa sokeasti kaikkea)"
        );

        server.abort();
    }
}
