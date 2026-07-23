//! Shared helper functions and small types used across agent submodules.

use super::prelude::*;

use super::ReplySink;

/// Derives the VAD coordinate's "intensity" (`0.0..=1.0`): how charged the
/// emotion state is. Used as the memory's emotion factor.
pub(crate) fn vad_magnitude(vad: &familyclaw_emotion::Vad) -> f32 {
    // Valence is -1..=1, arousal/dominance 0..=1. Absolute values are weighted.
    let v = vad.valence.abs();
    let a = vad.arousal;
    // Distance from neutral dominance (0.5).
    let d = (vad.dominance - 0.5).abs() * 2.0;
    ((v + a + d) / 3.0).clamp(0.0, 1.0)
}

/// Extracts the message's text representation for short-term memory. Same
/// logic as [`Agent::build_think_context`]'s `query` extraction, so that the
/// user text recorded in history matches what was sent to the model.
pub(crate) fn bus_message_text(message: &BusMessage) -> String {
    match message {
        BusMessage::Text { body } => body.clone(),
        BusMessage::Latent { text_shadow, .. } => text_shadow.clone(),
        other => format!("[{}]", other.kind_label()),
    }
}

/// Session tag per-message from [`MessageOrigin`] (F4, same format as `session.rs`).
pub(crate) fn session_tag_from_origin(origin: &MessageOrigin) -> String {
    format!("session:{}:{}", origin.channel_id, origin.conversation)
}

/// Generic user-visible fallback reply when the LLM/tool loop produces no
/// text. Kept for the **max-iterations** budget cutoff (the tool loop ran
/// out of rounds without an answer — genuinely "the tool loop stopped", so
/// this wording still applies there). For a **think-error** (the LLM chain
/// itself failed), use [`recovery_fallback_reply_for_error`] instead — see
/// its doc comment for why the two must not share one message.
pub(crate) fn recovery_fallback_reply() -> String {
    "Anteeksi — en saanut vietyä pyyntöä loppuun (työkalu epäonnistui tai turvaraja täyttyi). \
     Yritä uudelleen tai kerro tarkemmin mitä tarvitset."
        .to_string()
}

/// Stable prefix marking a `FamilyClawError::Llm` message built by
/// [`tag_llm_error`] — lets [`recovery_fallback_reply_for_error`] recover the
/// LLM failure's REDACTED category (one-word class + status line) without
/// carrying the original [`LlmError`] type across the several
/// `Result<_, FamilyClawError>` call sites between the HTTP call and the
/// think-error handler. Nothing after this prefix's two `|`-delimited fields
/// is ever read by the user-facing path — only tracing/logs see the rest.
pub(crate) const LLM_CLASS_TAG_PREFIX: &str = "llmclass:";

/// Wraps an [`LlmError`] into the string carried by `FamilyClawError::Llm`:
/// `"llmclass:<class>|<status line>|<full Display, for internal logs only>"`.
/// The first two fields are built from [`LlmError::failure_class`] /
/// [`LlmError::redacted_status_line`] — both REDACTED by construction (never
/// the raw provider response body, which has been observed in production to
/// leak an account identifier). The trailing `Display` is for
/// `warn!("... failed (non-fatal): {e}")`-style internal logs, exactly as
/// before this change; it is never surfaced to the user.
pub(crate) fn tag_llm_error(e: &LlmError) -> String {
    format!(
        "{LLM_CLASS_TAG_PREFIX}{}|{}|{e}",
        e.failure_class().as_word(),
        e.redacted_status_line(),
    )
}

/// Recovers `(class_word, redacted_status_line)` from a message built by
/// [`tag_llm_error`]. `None` if `msg` wasn't tagged this way (e.g. an `Llm`
/// error string built elsewhere, or a non-LLM error).
pub(crate) fn parse_llm_class_tag(msg: &str) -> Option<(&str, &str)> {
    let rest = msg.strip_prefix(LLM_CLASS_TAG_PREFIX)?;
    let mut parts = rest.splitn(3, '|');
    let class = parts.next()?;
    let status_line = parts.next()?;
    Some((class, status_line))
}

/// **Think-error** fallback reply (Failover gap #1 fix, item 2): the LLM
/// chain itself failed this turn (every configured provider/model
/// exhausted) — as opposed to [`recovery_fallback_reply`]'s max-iterations
/// case (the tool loop ran the model successfully but hit its round budget).
/// Previously BOTH cases returned the exact same generic
/// "työkalu epäonnistui tai turvaraja täyttyi" string, which — during the
/// incident this fixes (every NIM call 404ing) — told the operator "a tool
/// failed / a safety limit was hit" when the true cause was "no LLM
/// provider is reachable". This tells them which one it actually was.
///
/// **Redaction invariant:** the message below is built ONLY from the
/// REDACTED `(class, status_line)` pair recovered via [`parse_llm_class_tag`]
/// (itself sourced from [`LlmError::redacted_status_line`], which never
/// reads the provider's response body) — never the raw error/body text. If
/// `e` isn't a tagged LLM error (e.g. a bus/durable failure), this falls
/// back to the generic [`recovery_fallback_reply`].
pub(crate) fn recovery_fallback_reply_for_error(e: &FamilyClawError) -> String {
    let FamilyClawError::Llm(msg) = e else {
        return recovery_fallback_reply();
    };
    let Some((class, status_line)) = parse_llm_class_tag(msg) else {
        return recovery_fallback_reply();
    };
    if class == LlmFailureClass::ProviderNotFound.as_word() {
        return format!(
            "LLM-palveluihin ei saatu yhteyttä (viimeisin virhe: {status_line} — malli on \
             mahdollisesti poistettu). Tarkista FAMILYCLAW_PROVIDER_MODEL / provider-konfiguraatio."
        );
    }
    format!(
        "Anteeksi — en saanut vietyä pyyntöä loppuun (LLM-palveluvirhe: {status_line}). \
         Yritä hetken kuluttua uudelleen tai tarkista provider-konfiguraatio."
    )
}

/// Generic progress report derived from the tool name (OpenClaw/Hermes style).
pub(crate) fn tool_progress_label(tool_name: &str) -> String {
    let action = match tool_name {
        n if n.contains("file_write") => "Writing files",
        n if n.contains("file_patch") => "Applying patch",
        n if n.contains("fs_read") => "Reading files",
        n if n.contains("web_search") || n.contains("research") => "Searching the web",
        n if n.contains("web_fetch") => "Fetching a page",
        n if n.contains("github") => "Working with GitHub",
        n if n.contains("email") || n.contains("discord") => "Calling an integration",
        _ => "Running a tool",
    };
    action.to_string()
}

/// Public progress messages are kept on, so the user sees the progress.
pub(crate) fn should_emit_public_progress(origin: Option<&MessageOrigin>) -> bool {
    let _ = origin;
    true
}

pub(crate) const MAX_PROGRESS_PER_TURN: u32 = 5;
pub(crate) const PROGRESS_MIN_INTERVAL: Duration = Duration::from_secs(4);
pub(crate) const TOOL_BUDGET_PER_NAME: u32 = 3;
pub(crate) const TOOL_BUDGET_FS_READ: u32 = 8;

pub(crate) struct ProgressGate {
    sent: u32,
    last_at: Option<Instant>,
}

impl ProgressGate {
    pub(crate) fn new() -> Self {
        Self {
            sent: 0,
            last_at: None,
        }
    }

    pub(crate) fn allow(&self) -> bool {
        if self.sent >= MAX_PROGRESS_PER_TURN {
            return false;
        }
        if let Some(last) = self.last_at {
            if Instant::now().duration_since(last) < PROGRESS_MIN_INTERVAL {
                return false;
            }
        }
        true
    }

    pub(crate) fn record(&mut self) {
        self.sent += 1;
        self.last_at = Some(Instant::now());
    }
}

/// User-visible notice when the turn is left waiting for a rare approval.
pub(crate) fn suspended_approval_user_reply(
    approval_id: ApprovalId,
    redacted_summary: &str,
) -> String {
    format!(
        "BLOCKED (hyväksyntä): {redacted_summary}\n\
         ID: `{approval_id}`\n\
         Operaattori Discordissa: `APPROVE {approval_id}` tai `DENY {approval_id}`\n\
         Tai gateway: POST /approvals/{approval_id}/approve"
    )
}

/// Formats a tool error into clear SYSTEM feedback for the model (anti-silence).
pub(crate) fn format_tool_failure_for_model(
    tool_name: &str,
    error: &impl std::fmt::Display,
) -> String {
    format!(
        "SYSTEM: Your previous action '{tool_name}' failed with error: {error}. \
         Acknowledge this failure to the user, explain what went wrong in plain language, \
         and suggest a corrected approach. Do not silently stop."
    )
}

/// One LLM call without tools after a stall situation.
pub(crate) async fn recover_user_visible_reply(
    llm: &LlmFailover,
    messages: &[LlmMessage],
) -> Option<String> {
    let mut recovery_messages = messages.to_vec();
    recovery_messages.push(LlmMessage::user(
        "SYSTEM: Your previous tool calls failed or the turn hit the iteration limit. \
         Reply to the user in plain language: acknowledge the failure, summarize errors \
         from the tool results above, and suggest next steps. Do not call more tools.",
    ));
    match llm.complete(&recovery_messages).await {
        Ok(text) if !text.trim().is_empty() => Some(text),
        Ok(_) => None,
        Err(e) => {
            warn!("recovery LLM call failed (non-fatal): {e}");
            None
        }
    }
}

/// Builds the LLM message stack: `[system, ...history (oldest→newest), current_user]`.
///
/// `history` is the conversation's earlier user/assistant messages from a
/// sliding window (see [`Agent::history_for`]). This is the fix that gives
/// the model conversation continuity — without it the stack was always just
/// `[system, user]` and the agent "replied only once" (it did not see the
/// previous exchange).
pub(crate) fn build_message_stack(
    system_prompt: String,
    history: &[LlmMessage],
    query: String,
) -> Vec<LlmMessage> {
    let mut messages = Vec::with_capacity(history.len() + 2);
    messages.push(LlmMessage::system(system_prompt));
    messages.extend(history.iter().cloned());
    messages.push(LlmMessage::user(query));
    messages
}

/// Truncates the text to [`history_max_chars_per_msg`] (default
/// [`HISTORY_MAX_CHARS_PER_MSG`], overridable via
/// `FAMILYCLAW_HISTORY_MAX_CHARS`) respecting the UTF-8 boundary, for storage
/// in short-term memory. Short texts are returned as-is.
pub(crate) fn truncate_for_history(text: &str) -> String {
    let max = super::history_max_chars_per_msg();
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Deterministically decides whether a message is worth remembering.
///
/// Generic baseline rule: text and latent messages are remembered (they are
/// content between beings), emotion pulses and light task signals are not
/// (they are transient nervous-system "blood", not content).
pub(crate) fn should_remember(message: &BusMessage) -> bool {
    matches!(message, BusMessage::Text { .. } | BusMessage::Latent { .. })
}

/// Builds a short, deterministic summary of the turn (into the durable log).
pub(crate) fn summarize(sender: BeingId, message: &BusMessage) -> String {
    format!("{} from {sender}", message.kind_label())
}

/// Maps the MCP tool descriptors published by the runtime into
/// [`ToolDefinition`]s to be offered to the LLM (tool loop, Phase 1 keystone).
///
/// Only **valid** definitions ([`ToolDefinition::validate`]) are offered to
/// the model — an invalid name or a non-object schema is skipped and logged
/// at debug level, so that one invalid skill does not bring down the whole
/// call. `name`, `description`, and `input_schema` (→ `function.parameters`)
/// come directly from the descriptor; the required permission / trust level
/// is the actions layer's responsibility and is not part of the form
/// offered to the LLM.
pub(crate) fn build_tool_definitions(descriptors: &[McpToolDescriptor]) -> Vec<ToolDefinition> {
    descriptors
        .iter()
        .filter_map(|d| {
            let def = ToolDefinition {
                name: d.name.clone(),
                description: d.description.clone(),
                input_schema: d.input_schema.clone(),
            };
            match def.validate() {
                Ok(()) => Some(def),
                Err(e) => {
                    debug!(tool = d.name.as_str(), error = %e, "tool loop: skipping invalid tool definition");
                    None
                }
            }
        })
        .collect()
}

/// Redacts the message stack **for a resumable turn** before persisting it
/// to disk (suspend/resume bridge, secrets invariant).
///
/// Because a resumable turn is persisted to disk, the **entire** secrets
/// surface of **every** message must be redacted — not just tool-call
/// arguments, but also the messages' text content (`content`), in which a
/// secret can hide as free text. The previous version only redacted
/// `tool_calls` arguments, and only at the "whole value / known key name"
/// level, so that (a) system/user/assistant messages' `content` and (b) a
/// secret **embedded** in the model-produced argument's free text could
/// reach disk in raw form. This function closes both gaps:
///
/// - **Messages' `content`** is run through
///   [`familyclaw_actions::redact_free_text`] (a substring pass: individual
///   secret words + `Bearer …` + `key=value`). Tool messages' content is
///   already redacted in the actions pipeline (`proof.redacted_output`), but
///   this pass is idempotent and acts as defense in depth for
///   system/user/assistant texts too.
/// - **Tool calls' `arguments`** ([`crate::llm::ToolCall::arguments`]) is raw
///   JSON produced by the model. It is run through the **deep** redactor
///   ([`familyclaw_actions::redact_value_deep`]), which redacts both the
///   whole value / known key name AND secrets embedded in free text.
///
/// This is safe with respect to resume: the approved action has already been
/// executed (the payload is bound to the pending approval in the actions
/// layer), so the replayed assistant message only needs the tool call's
/// **id and name** to bind the `tool_result` to the right call — not the raw
/// arguments.
///
/// Returns a redacted copy (the original live stack is not mutated).
pub(crate) fn redact_messages_for_resume(messages: &[LlmMessage]) -> Vec<LlmMessage> {
    messages
        .iter()
        .map(|m| {
            // 1. Text content: redact secrets embedded in free text from
            //    every message (system/user/assistant/tool).
            let (redacted_content, _) = familyclaw_actions::redact_free_text(&m.content);
            // 2. Tool calls' arguments: deep redaction (incl. embedded ones).
            let redacted_calls = m.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|c| {
                        let (redacted_args, _) =
                            familyclaw_actions::redact_value_deep(&c.arguments);
                        crate::llm::ToolCall {
                            id: c.id.clone(),
                            name: c.name.clone(),
                            arguments: redacted_args,
                        }
                    })
                    .collect()
            });
            LlmMessage {
                content: redacted_content,
                tool_calls: redacted_calls,
                ..m.clone()
            }
        })
        .collect()
}

/// Derives the text to feed back to the model from a tool call's result
/// (tool loop, Phase 1 keystone).
///
/// Uses the task's **redacted proof bundle** if one was produced
/// ([`ActionRuntime::proof`]): the proof's `redacted_output` (secrets
/// removed) is serialized to JSON text, prefixed with a short summary. If
/// there is no proof (e.g. the task did not produce one), only a status
/// description is returned. The output never contains a raw secret — the
/// proof is already redacted in the actions pipeline.
pub(crate) fn tool_result_text(
    runtime: &ActionRuntime,
    submit: &familyclaw_actions::SubmitOutcome,
) -> String {
    tool_result_text_for(runtime, submit.task_id, submit.status)
}

/// Like [`tool_result_text`], but takes the task's id and status directly
/// (the durable-journaled dispatch path, where the whole [`SubmitOutcome`]
/// is not kept alive but [`DispatchRecord`] carries the id and status).
///
/// Same redaction guarantee: the proof is fetched by id
/// ([`ActionRuntime::proof`]) and its `redacted_output` is already
/// secret-free. If no proof is found (e.g. a replay branch where the
/// executor was not run again), only a status description is returned — the
/// output never contains a raw secret.
pub(crate) fn tool_result_text_for(
    runtime: &ActionRuntime,
    task_id: ActionTaskId,
    status: familyclaw_actions::task::TaskStatus,
) -> String {
    let failed = matches!(status, familyclaw_actions::task::TaskStatus::Failed);
    if let Some(proof) = runtime.proof(task_id) {
        let body =
            serde_json::to_string(&proof.redacted_output).unwrap_or_else(|_| "{}".to_string());
        let prefix = if failed { "FAILED: " } else { "" };
        format!(
            "{prefix}status={status:?}; {}; output={body}",
            proof.output_summary
        )
    } else if failed {
        format!("FAILED: status={status:?}; action did not succeed (no proof bundle)")
    } else {
        format!("status={status:?}; no proof produced")
    }
}

/// **Durable-journaled dispatch outcome** (D1, crash-replay).
///
/// Deliberately kept small and **deterministically serializable**: exactly
/// the part of [`SubmitOutcome`] that the tool loop needs to continue
/// (`task_id`, `status`, `pending_approval`). When the dispatch is journaled
/// ([`Agent::drive_tool_loop_durable`]), replay returns exactly this value —
/// including the random [`ApprovalId`] drawn at dispatch time and the
/// clock-derived TTL, which live on in the approval granted by
/// `submit_task` — **without running the skill's executor again**. This way
/// the side effect happens exactly once and the dispatch outcome is
/// value-identical to the original run.
///
/// A `submit_task` error (e.g. an unknown skill) is stored in
/// [`DispatchRecord::error`], so that it too replays identically and does
/// not run the executor again.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct DispatchRecord {
    /// The dispatched task's id (proof lookup + diagnostics).
    pub(crate) task_id: ActionTaskId,
    /// The task's status after the pipeline ran.
    pub(crate) status: familyclaw_actions::task::TaskStatus,
    /// The approval id if the dispatch was left waiting for approval.
    pub(crate) pending_approval: Option<ApprovalId>,
    /// `submit_task`'s error message if the dispatch failed (otherwise `None`).
    pub(crate) error: Option<String>,
}

impl DispatchRecord {
    /// Builds the journaled record from `submit_task`'s outcome.
    ///
    /// On success, copies the id, status, and any approval; on failure,
    /// stores the error message (nil id + redacted status), so that replay
    /// returns the same error without running the executor again.
    pub(crate) fn from_outcome(
        outcome: &familyclaw_actions::Result<familyclaw_actions::SubmitOutcome>,
    ) -> Self {
        match outcome {
            Ok(submit) => Self {
                task_id: submit.task_id,
                status: submit.status,
                pending_approval: submit.pending_approval,
                error: None,
            },
            Err(e) => Self {
                task_id: ActionTaskId::nil(),
                status: familyclaw_actions::task::TaskStatus::Failed,
                pending_approval: None,
                error: Some(e.to_string()),
            },
        }
    }
}

/// Records a single turn-audit event via a free function (durable dispatch
/// path, which cannot call an `&self` method for disjoint-borrow reasons).
///
/// Same **secrets invariant** as [`Agent::record_turn_audit`]: `detail` is
/// run through [`familyclaw_actions::redact_free_text`] before storage
/// (defense in depth). `at` (D1) is injected — the clock is not read here.
/// No-op when no collector is wired up (`audit = None`).
pub(crate) fn record_turn_audit_into(
    audit: Option<&Arc<AuditCollector>>,
    turn_id: ActionId,
    kind: AuditKind,
    at: Timestamp,
    detail: impl Into<String>,
) {
    let Some(audit) = audit else {
        return;
    };
    let (safe_detail, _) = familyclaw_actions::redact_free_text(&detail.into());
    audit.record(ExecAuditEvent::new(kind, turn_id, at, safe_detail));
}

/// Awaits `fut` in two stages against a soft/hard watchdog deadline pair: up
/// to `soft_secs`, then — if not finished — invokes `on_soft_deadline` once
/// and keeps awaiting the **same** future for the remaining time up to
/// `hard_secs`. Only past `hard_secs` is `fut` actually abandoned
/// (`Err(())`); a completion anywhere in between is delivered as `Ok(value)`
/// (late, but not discarded).
///
/// `fut` is heap-pinned (`Pin<Box<F>>`, not the stack-pinning `tokio::pin!`)
/// specifically so it is a plain, ownable value that can be `drop`ped
/// explicitly on every exit path. Dropping it is what releases whatever it
/// borrowed for its lifetime — in the caller below, `agent`'s exclusive
/// borrow via `handle_turn_with_origin(&mut self, ...)` — mirroring the
/// implicit drop that the old single-stage `tokio::time::timeout(...).await`
/// performed when it elapsed. The difference is that here, that drop only
/// happens at the hard cap: hitting the soft deadline no longer discards any
/// in-flight work.
pub(crate) async fn watchdog_two_stage<F, T>(
    mut fut: Pin<Box<F>>,
    soft_secs: u64,
    hard_secs: u64,
    on_soft_deadline: impl FnOnce(),
) -> std::result::Result<T, ()>
where
    F: Future<Output = T>,
{
    if let Ok(value) = tokio::time::timeout(Duration::from_secs(soft_secs), &mut fut).await {
        drop(fut);
        return Ok(value);
    }
    on_soft_deadline();
    let remaining = hard_secs.saturating_sub(soft_secs);
    let result = tokio::time::timeout(Duration::from_secs(remaining), &mut fut).await;
    drop(fut);
    result.map_err(|_| ())
}

/// Sends the watchdog "still working" interim notice directly through a
/// pre-cloned reply sink + pre-resolved reply target, **without** going
/// through [`Agent::force_watchdog_reply`]. This is required because at the
/// point this fires, the turn future in [`AgentActor::handle`] still holds
/// `agent`'s exclusive borrow (`handle_turn_with_origin(&mut self, ...)`) —
/// no method on `agent` can be called until that future is dropped. Mirrors
/// `force_watchdog_reply`'s target-resolution + sink-send exactly, just
/// using values captured before the borrow started.
pub(crate) fn send_watchdog_notice(sink: Option<&ReplySink>, target: Option<&str>, body: &str) {
    let (Some(sink), Some(target)) = (sink, target) else {
        return;
    };
    match OutboundMessage::new(target, body) {
        Ok(msg) => {
            let _ = sink.send(msg);
        }
        Err(e) => {
            warn!(error = %e, "watchdog still-working notice build failed");
        }
    }
}
