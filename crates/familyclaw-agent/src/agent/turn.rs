//! Turn execution, durable replay, and tool-loop engine for [`super::Agent`].

#![allow(clippy::wildcard_imports)]

use super::prelude::*;

use super::helpers::*;

use super::{
    compact_history, Agent, ErasedJournal, MetricEvent, MetricEventSink, ReplySink, ThinkOutcome,
    ToolLoopOutcome, TurnOutcome, CHAT_ASSISTANT_TAG, CHAT_HISTORY_SOURCE, CHAT_USER_TAG,
    HISTORY_HYDRATE_LIMIT, RESUMABLE_DEFAULT_TTL_MINUTES,
};

impl Agent {
    /// The agent's thinking: fetches relevant memories from the Eternal
    /// Thread (RAG), builds the system prompt (soul + memories), and calls
    /// the LLM.
    ///
    /// Natively **async** — no `block_on`/`block_in_place` patterns, which
    /// would panic on a `current_thread` runtime or could deadlock.
    ///
    /// ## Two paths (Phase 1 keystone)
    /// - **`actions = None` (default, UNCHANGED):** a single LLM call
    ///   ([`LlmFailover::complete`]) without tools. This is the original
    ///   single-shot behavior, which is not changed — all old paths and
    ///   tests remain intact.
    /// - **`actions = Some(rt)`:** an internal `run_tool_loop` runs the loop: the LLM
    ///   is given the tools published by the runtime, and its chosen tool
    ///   calls are routed back to the runtime until the model replies
    ///   without tool calls (a stop) or the [`ToolLoopConfig`] limit is reached.
    ///
    /// Returns a [`ThinkOutcome`] (1C, roadmap amendment 3): suspend is a
    /// STATE, not a string. The earlier `Option<Result<String>>` return has
    /// been replaced — see [`ThinkOutcome`]'s documentation for the
    /// rationale behind the migration.
    ///
    /// - No LLM client → [`ThinkOutcome::NoReply`] (harmless no-op).
    /// - Single-shot path → the model's text as [`ThinkOutcome::Reply`].
    /// - Tool loop `Answer` → [`ThinkOutcome::Reply`].
    /// - Tool loop `AwaitingApproval` → [`ThinkOutcome::Suspended`]
    ///   (id + redacted summary).
    /// - Tool loop `MaxIterations` / no text → [`ThinkOutcome::NoReply`].
    ///
    /// (`Answer`/`AwaitingApproval`/`MaxIterations` are variants of the tool
    /// loop's internal `ToolLoopOutcome` type — private mechanisms that
    /// `think` converts into the public [`ThinkOutcome`] above.)
    ///
    /// ## User-boundary protection (tool loop)
    /// Only [`ThinkOutcome::Reply`] is meant for the end user.
    /// [`ThinkOutcome::Suspended`] **never** travels through the reply pipe —
    /// it is recorded to the turn's durable state for resume
    /// ([`handle_turn_with_origin`](Self::handle_turn_with_origin)) — and its
    /// internal identifiers (including the raw `approval_id`) are not routed
    /// to the user. This fixes the 1B leak, where intermediate tokens leaked
    /// verbatim.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails.
    pub async fn think(&self, current_message: &BusMessage) -> Result<ThinkOutcome> {
        self.think_with_origin(current_message, None).await
    }

    /// Like [`think`](Self::think), but knows the **turn's origin** (resume
    /// bridge, roadmap Â§6): when the tool loop suspends pending approval,
    /// the resumable turn ([`ResumableTurn`]) is saved with `conversation_origin`,
    /// so resume knows how to route the reply to the correct conversation.
    ///
    /// [`think`](Self::think) is a wrapper around this with `origin = None`
    /// (a static reply target).
    ///
    /// ## Suspend persists the resumable turn (EXACTLY ONCE)
    /// In the `AwaitingApproval` branch, this method builds a secret-free
    /// [`ResumableTurn`] (message stack + hashed arguments + identifiers) and
    /// saves it to the resumable turn store
    /// ([`resumable_store`](Self::resumable_store)) **before** returning
    /// [`ThinkOutcome::Suspended`]. The caller runs `think_with_origin` only
    /// on a FRESH turn (not during replay), so the put happens exactly once.
    ///
    /// **Determinism (D1):** the clock is read **once** (`time::now()`) at the
    /// start of this method and injected into the entire tool loop and into
    /// the resumable turn's `created_at` field â€” the loop logic does not read
    /// the clock itself.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails.
    // A turn is one unified sequence (context â†’ tool loop â†’ result â†’
    // turn audit). TURN-AUDIT records (start/answered/suspend/max-iter)
    // pushed the line count slightly over the cap; splitting it would break
    // up the outcome mapping without a clarity benefit.
    #[allow(clippy::too_many_lines)]
    pub async fn think_with_origin(
        &self,
        current_message: &BusMessage,
        origin: Option<&familyclaw_bus::MessageOrigin>,
    ) -> Result<ThinkOutcome> {
        // No LLM client â†’ no reply for this turn (harmless no-op).
        let Some(llm) = self.llm.as_ref() else {
            return Ok(ThinkOutcome::NoReply);
        };
        let (system_prompt, query) = self.build_think_context(current_message, origin).await;

        // Short-term memory: fetch this conversation's prior turns (RAM +
        // store hydration on cold start), so the model sees continuity.
        let conv_key = self.conversation_key(origin);
        let history = self.conversation_history(&conv_key, origin).await;

        match self.actions.as_ref() {
            // Single-shot path (backward-compatible): one LLM call,
            // no tools. Same behavior as before the tool loop â†’ text as Reply.
            None => {
                let messages = build_message_stack(system_prompt, &history, query);
                let text = self
                    .llm_complete_with_progress(llm, &messages, origin)
                    .await?;
                Ok(ThinkOutcome::Reply(text))
            }
            // Tool loop path: give the model tools and loop until it
            // stops requesting them (or the limit is reached). Only `Answer` â†’ `Reply`
            // crosses the user boundary; control states convert to Suspended/NoReply.
            //
            // D1: the clock is read ONCE here, injected into the tool loop.
            Some(actions) => {
                let now = time::now();
                // TURN-AUDIT (roadmap Â§6 D6): one correlation identifier for
                // this turn, with which all its events (start â†’ tool calls â†’
                // stop_reason) are marked. No-op when no audit is wired.
                let turn_id = ActionId::new();
                self.record_turn_audit(turn_id, AuditKind::TurnStarted, now, "turn started");
                let messages = build_message_stack(system_prompt, &history, query);
                match self
                    .run_tool_loop(llm, actions, messages, now, turn_id)
                    .await?
                {
                    ToolLoopOutcome::Answer(text) => {
                        // stop_reason = answered. Do NOT record the reply text (may
                        // contain user data) â€” only the length, for observability.
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::TurnAnswered,
                            now,
                            format!("answered ({} chars)", text.chars().count()),
                        );
                        Ok(ThinkOutcome::Reply(text))
                    }
                    ToolLoopOutcome::AwaitingApproval {
                        tool,
                        approval_id,
                        redacted_summary,
                        messages,
                        tool_call_id,
                        arguments,
                    } => {
                        // Suspend is a STATE: a tool is waiting for human approval.
                        // NOT to the user â€” `approval_id` is the operator's
                        // (ActionRuntime) information. We return it as a first-class
                        // Suspended state, which the caller records to durable state
                        // for resume (not into the reply pipe).
                        //
                        // Resume bridge (roadmap Â§6): save the resumable turn
                        // persistently, so `resume_approved` can continue the loop
                        // from where it left off â€” even across a process crash, if
                        // the surface is crash-resistant. `arguments` is given ONLY
                        // to be hashed (ResumableTurn::new computes the SHA-256,
                        // does not store the raw value). `now` (D1) is the
                        // resumable turn's `created_at`; the TTL is derived from the
                        // pending approval's expiry, if known.
                        let expires_at = self
                            .pending_expiry_for(actions, approval_id)
                            .await
                            .unwrap_or_else(|| {
                                now + chrono::Duration::minutes(RESUMABLE_DEFAULT_TTL_MINUTES)
                            });
                        // Secrecy invariant: redact the message stack's
                        // tool call arguments before saving to disk â€”
                        // raw payload/keys never end up on the durable surface.
                        let safe_messages = redact_messages_for_resume(&messages);
                        let resumable = ResumableTurn::new(
                            approval_id,
                            self.being_id.to_string(),
                            origin.cloned(),
                            safe_messages,
                            tool_call_id,
                            tool.clone(),
                            &arguments,
                            redacted_summary.clone(),
                            now,
                            expires_at,
                        )
                        .with_policy_snapshot(format!("tool '{tool}' requires human approval"))
                        .with_durable_position(self.turn_counter, 0);
                        if let Err(e) = self.resumable.put(resumable) {
                            // A persistence failure must not crash the turn,
                            // but resume then won't succeed â†’ log as a warning.
                            warn!(
                                agent = self.config.name,
                                %approval_id,
                                error = %e,
                                "resumable turn persist failed â€” resume will not be possible for this approval"
                            );
                        }
                        debug!(
                            agent = self.config.name,
                            tool = tool.as_str(),
                            %approval_id,
                            "tool loop: awaiting human approval â€” suspending turn (resumable persisted, not routed to user)"
                        );
                        // stop_reason = suspended. `approval_id` + redacted
                        // summary â€” NOT a raw payload (the summary is already
                        // operator-safe, and redact_free_text still protects it).
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::TurnSuspended,
                            now,
                            format!(
                                "suspended awaiting approval {approval_id}: {redacted_summary}"
                            ),
                        );
                        Ok(ThinkOutcome::Suspended {
                            approval_id,
                            redacted_summary,
                        })
                    }
                    ToolLoopOutcome::MaxIterations { iterations } => {
                        debug!(
                            agent = self.config.name,
                            iterations,
                            "tool loop: reached max iterations without a final answer â€” attempting recovery reply"
                        );
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::TurnMaxIterations,
                            now,
                            format!("reached max iterations ({iterations}) without answer"),
                        );
                        Ok(ThinkOutcome::Reply(recovery_fallback_reply()))
                    }
                }
            }
        }
    }

    /// `FAMILYCLAW_STREAMING=1` â†’ the LLM response is streamed and progress is updated every ~2s.
    fn llm_streaming_enabled() -> bool {
        std::env::var("FAMILYCLAW_STREAMING")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    }

    /// One LLM call; with streaming, sends [`OutboundKind::Progress`] updates.
    async fn llm_complete_with_progress(
        &self,
        llm: &LlmFailover,
        messages: &[LlmMessage],
        origin: Option<&familyclaw_bus::MessageOrigin>,
    ) -> Result<String> {
        if !Self::llm_streaming_enabled() {
            return llm
                .complete(messages)
                .await
                .map_err(|e| FamilyClawError::llm(tag_llm_error(&e)));
        }
        let target = origin
            .map(familyclaw_bus::MessageOrigin::reply_target)
            .or(self.reply_target.as_deref());
        let mut stream = llm
            .complete_stream(messages)
            .await
            .map_err(|e| FamilyClawError::llm(tag_llm_error(&e)))?;
        let mut full = String::new();
        let emit_progress = should_emit_public_progress(origin);
        let mut last_progress = Instant::now();
        let frames = ["â–±â–±â–±", "â–°â–±â–±", "â–°â–°â–±", "â–°â–°â–°"];
        let mut frame_idx = 0usize;
        let mut progress_gate = ProgressGate::new();
        while let Some(chunk) = stream.next().await {
            let delta = chunk.map_err(|e| FamilyClawError::llm(tag_llm_error(&e)))?;
            full.push_str(&delta);
            if emit_progress
                && last_progress.elapsed() >= PROGRESS_MIN_INTERVAL
                && progress_gate.allow()
            {
                if let (Some(sink), Some(target)) = (&self.reply_sink, target) {
                    let body = format!("â†³ {} Drafting responseâ€¦", frames[frame_idx]);
                    if let Ok(msg) = OutboundMessage::progress(target, body) {
                        let _ = sink.send(msg);
                        progress_gate.record();
                    }
                    frame_idx = (frame_idx + 1) % frames.len();
                }
                last_progress = Instant::now();
            }
        }
        Ok(full)
    }

    /// Builds a conversation key from the message's origin for short-term memory.
    ///
    /// Key = `"{channel_id}:{conversation}"` â€” the same format as the F4
    /// session key, but derived from the per-message
    /// [`familyclaw_bus::MessageOrigin`]. If there is no origin (e.g. an
    /// internal/test message without a channel), the reply target is used
    /// as a fallback, and ultimately a shared `"default"` key. This way, an
    /// agent that doesn't yet have origin information uses one shared
    /// history instead of losing continuity entirely.
    pub(crate) fn conversation_key(
        &self,
        origin: Option<&familyclaw_bus::MessageOrigin>,
    ) -> String {
        if let Some(o) = origin {
            return format!("{}:{}", o.channel_id, o.conversation);
        }
        self.reply_target
            .clone()
            .unwrap_or_else(|| "default".to_string())
    }

    /// Returns the conversation's short-term memory messages (oldestâ†’newest)
    /// for building the LLM stack. Uses the in-process RAM window first; if
    /// empty (cold start / new process), hydrates the most recent
    /// `chat:*`-tagged memories with the session scope.
    async fn conversation_history(
        &self,
        conv_key: &str,
        origin: Option<&MessageOrigin>,
    ) -> Vec<LlmMessage> {
        let ram = self.history_for(conv_key);
        if !ram.is_empty() {
            return ram;
        }
        self.hydrate_history_from_store(origin).await
    }

    /// Fetches the most recent session-scoped chat messages from the memory store.
    async fn hydrate_history_from_store(&self, origin: Option<&MessageOrigin>) -> Vec<LlmMessage> {
        let Some(session_tag) = self.session_tag_for_recall(origin) else {
            return Vec::new();
        };
        let all = match self.memory.all().await {
            Ok(m) => m,
            Err(e) => {
                warn!("chat history hydration failed (non-fatal): {e}");
                return Vec::new();
            }
        };
        let mut chat: Vec<(Timestamp, LlmMessage)> = all
            .into_iter()
            .filter(|m| {
                m.source == CHAT_HISTORY_SOURCE
                    && m.tags.iter().any(|t| t == &session_tag)
                    && m.status == familyclaw_memory::MemoryStatus::Active
            })
            .filter_map(|m| {
                let role = if m.tags.iter().any(|t| t == CHAT_USER_TAG) {
                    Some(LlmMessage::user(truncate_for_history(&m.content)))
                } else if m.tags.iter().any(|t| t == CHAT_ASSISTANT_TAG) {
                    Some(LlmMessage::assistant(truncate_for_history(&m.content)))
                } else {
                    None
                };
                role.map(|msg| (m.created_at, msg))
            })
            .collect();
        chat.sort_by_key(|a| a.0);
        if chat.len() > HISTORY_HYDRATE_LIMIT {
            chat.drain(0..chat.len() - HISTORY_HYDRATE_LIMIT);
        }
        chat.into_iter().map(|(_, msg)| msg).collect()
    }

    /// Derives the session tag per message from origin or the static `with_session`.
    pub(super) fn session_tag_for_recall(&self, origin: Option<&MessageOrigin>) -> Option<String> {
        origin.map(session_tag_from_origin).or_else(|| {
            self.session
                .as_ref()
                .map(crate::session::MessageOrigin::session_tag)
        })
    }

    /// Saves a single chat-role message to the memory store with the session
    /// scope (for cold-start hydration). Non-duplicate: `turn_key` is
    /// missing â†’ every turn is its own entry.
    async fn persist_chat_turn(
        &self,
        origin: &MessageOrigin,
        role_tag: &str,
        text: &str,
    ) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let memory = Memory::builder(truncate_for_history(text))
            .source(CHAT_HISTORY_SOURCE)
            .decay_policy(DecayPolicy::Normal)
            .tags([session_tag_from_origin(origin), role_tag.to_string()])
            .build();
        self.memory
            .add(memory)
            .await
            .map_err(|e| FamilyClawError::memory(format!("chat history persist failed: {e}")))?;
        Ok(())
    }

    /// Returns the conversation's short-term memory messages (oldestâ†’newest)
    /// for building the LLM stack. Empty if there's no history yet for the conversation.
    pub(crate) fn history_for(&self, conv_key: &str) -> Vec<LlmMessage> {
        self.history
            .get(conv_key)
            .map(|dq| dq.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Appends a successful turn (user message + agent reply) to the
    /// conversation's short-term memory as a sliding window. Truncates each
    /// message to [`HISTORY_MAX_CHARS_PER_MSG`] and applies the agent's
    /// [`CompactionConfig`](super::CompactionConfig) via
    /// [`compact_history`] once the count exceeds
    /// [`CompactionConfig::max_messages`](super::CompactionConfig::max_messages).
    ///
    /// **Call ONLY on a fresh turn** (`!self.durable.is_replaying()`) â€”
    /// otherwise replay would double-record. Empty messages are not saved.
    pub(crate) fn append_history(&mut self, conv_key: &str, user_text: &str, assistant_text: &str) {
        let user_text = user_text.trim();
        let assistant_text = assistant_text.trim();
        if user_text.is_empty() || assistant_text.is_empty() {
            return;
        }
        let dq = self.history.entry(conv_key.to_string()).or_default();
        dq.push_back(LlmMessage::user(truncate_for_history(user_text)));
        dq.push_back(LlmMessage::assistant(truncate_for_history(assistant_text)));
        compact_history(dq, &self.compaction);
    }

    /// Builds [`think`](Agent::think)'s shared context: RAG recall +
    /// system prompt (soul essence + memories) and the message text (`query`).
    ///
    /// Shared between both paths (single-shot + tool loop), so that memory
    /// retrieval and prompt building are identical regardless of whether
    /// tools are installed. F4 session isolation: if a session is set,
    /// recall requires the session tag (only that session's memories are visible).
    #[allow(clippy::format_push_string)]
    async fn build_think_context(
        &self,
        current_message: &BusMessage,
        origin: Option<&MessageOrigin>,
    ) -> (String, String) {
        let query = bus_message_text(current_message);

        // ORIENT: fetch relevant memories FIRST (RAG â€” before the LLM call).
        let semantic_weight = std::env::var("FAMILYCLAW_SEMANTIC_WEIGHT")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .map_or(0.6, |w| w.clamp(0.0, 1.0));
        let mut recall_ctx = RetrievalContext::new(query.clone())
            .with_limit(5)
            .with_semantic_weight(semantic_weight);
        if let Some(tag) = self.session_tag_for_recall(origin) {
            recall_ctx = recall_ctx.with_required_tags([tag]);
        }
        let memories = self.recall(&recall_ctx).await.unwrap_or_else(|e| {
            warn!("recall failed in think (non-fatal): {e}");
            Vec::new()
        });
        let memories = crate::identity::filter_memories_for_operator(memories, origin, |hit| {
            hit.memory.content.as_str()
        });

        // System prompt: soul essence + memories as context.
        let mut system_prompt = self.soul.essence.clone();
        system_prompt.push_str(&crate::identity::identity_guard_prompt(origin));
        if !memories.is_empty() {
            system_prompt.push_str("\n\n[RELEVANT MEMORIES FROM ETERNAL THREAD]:\n");
            for (i, mem) in memories.iter().enumerate() {
                system_prompt.push_str(&format!(
                    "  {}. (relevance: {:.2}) {}\n",
                    i + 1,
                    mem.relevance,
                    mem.memory.content
                ));
            }
            system_prompt.push_str("[END MEMORIES]\n");
        }
        if self.actions.is_some() {
            system_prompt.push_str(
                "\n[TOOL CONTRACT]\n\
                 Disk and web changes require tool calls in THIS turn. \
                 Never claim DONE/evidence/file paths without a tool_result. \
                 If unsure, say what tool you will call next.\n",
            );
        }

        (system_prompt, query)
    }

    /// Runs the **tool loop** journaled durably (D1, roadmap Â§6 green-gate e):
    /// the same engine as [`run_tool_loop`](Self::run_tool_loop), but every
    /// tool dispatch ([`ActionRuntime::submit_task`]) is wrapped in its own
    /// durable step `turn-{turn}-dispatch-{k}`, so **partial progress
    /// within a turn survives a crash**.
    ///
    /// ## Why this exists (red-team finding)
    /// Without this, the loop only journals the entire `think` as a single
    /// `-think` step **after the loop**. If the process crashes BETWEEN two
    /// tool dispatches, nothing has been recorded yet â†’ replay runs the
    /// entire `think` from scratch â†’ (a) the first tool's side effect runs
    /// AGAIN, and (b) [`ActionRuntime::submit_task`] produces a NEW random
    /// [`ApprovalId`] ([`uuid::Uuid::new_v4`], not clock-derived) â†’
    /// determinism breaks. Per-dispatch journaling closes the gap: during
    /// replay an already-recorded dispatch is returned from the log
    /// (`SubmitOutcome` value-identical, including the random `ApprovalId` +
    /// clock-derived TTL) **without** running the skill executor again.
    ///
    /// `durable` is a `&mut` context, so this is a **free function that takes
    /// fields separately** (not `&self`): `handle_turn_with_origin` can borrow
    /// `&mut self.durable` and the other `self` fields immutably as separate
    /// (disjoint field borrows) â€” as a `&self` method the borrows would overlap.
    ///
    /// `dispatch_base` is the number of dispatches already recorded for this
    /// turn (usually 0 for a fresh turn); it continues `-dispatch-{k}`
    /// numbering from the correct point during replay.
    ///
    /// `being_id` is **this being's** identifier (usually the string form of
    /// [`Agent::being_id`]). It is passed to
    /// [`ActionRuntime::submit_task_as`], so the **per-being rate limit** for
    /// dangerous (approval-requiring) tool calls applies to the correct
    /// being and does not fall back to the runtime's generic default â€” this
    /// way one being cannot consume another being's quota through the same
    /// shared runtime.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails unrecoverably.
    /// - [`FamilyClawError::Bus`] if a durable step fails (wrapped).
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn drive_tool_loop_durable(
        llm: &LlmFailover,
        actions: &Arc<Mutex<ActionRuntime>>,
        durable: &mut DurableContext<ErasedJournal>,
        turn: u64,
        dispatch_base: u32,
        agent_name: &str,
        being_id: &str,
        turn_audit: Option<&Arc<AuditCollector>>,
        metrics_sink: Option<&MetricEventSink>,
        progress_sink: Option<ReplySink>,
        progress_target: Option<String>,
        mut messages: Vec<LlmMessage>,
        mut last_text: String,
        budget: u32,
        now: Timestamp,
        turn_id: ActionId,
        origin: Option<&MessageOrigin>,
        user_query: &str,
    ) -> Result<(ToolLoopOutcome, u32)> {
        // Phase 2: emit the tool-call metric if a sink is installed. Called
        // at every dispatch site guarded by `!replaying` (replay must not
        // double-count). Generic, carries no user/Layer B data.
        let emit_tool = |replaying: bool| {
            if !replaying {
                if let Some(sink) = metrics_sink {
                    // try_send: drop on overflow, do not block/grow the queue unbounded.
                    let _ = sink.try_send(MetricEvent::ToolDispatched);
                }
            }
        };
        // `-dispatch-{k}` and `-llm-{k}` running numbers: continue from the
        // correct point after replay. Shared across ALL rounds (not reset
        // per round), so every step gets a unique, deterministic name.
        let mut dispatch_index = dispatch_base;
        let emit_progress = should_emit_public_progress(origin);
        let mut last_progress_label: Option<String> = None;
        let mut progress_gate = ProgressGate::new();
        let mut tool_use_counts: HashMap<String, u32> = HashMap::new();
        let mut fs_read_count = 0u32;
        let mut dispatch_count = 0u32;
        for iteration in 0..budget {
            // D1: wrap **the LLM call too** in a durable step. Without this,
            // replay would call the LLM again (non-deterministic + a network
            // call), and replay of an already-fully-completed turn would
            // diverge. On a fresh run the call is made and its result
            // (content + tool_calls) is journaled; during replay the saved
            // result is returned without making the network call. A
            // serializable projection is journaled (`CompletionResult` is not
            // `Serialize`). `iteration` is the loop's sequence number â†’
            // deterministic step name.
            let llm_step = format!("turn-{turn}-llm-{iteration}");
            let replaying_llm = durable.is_replaying();
            let (content, tool_calls_opt): (Option<String>, Option<Vec<ToolCall>>) =
                if replaying_llm {
                    durable
                        .step(&llm_step, || {
                            Err("unreachable: replay returns journaled LLM result".to_string())
                        })
                        .map_err(|e| {
                            FamilyClawError::bus(format!("durable llm replay failed: {e}"))
                        })?
                } else {
                    let tools = {
                        let rt = actions.lock().await;
                        build_tool_definitions(&rt.tool_definitions())
                    };
                    let result = llm
                        .complete_with_tools(&messages, &tools)
                        .await
                        .map_err(|e| FamilyClawError::llm(tag_llm_error(&e)))?;
                    let mut content = result.content;
                    let mut tool_calls_opt = result.tool_calls;
                    let no_tools = tool_calls_opt.as_ref().is_none_or(Vec::is_empty);
                    if no_tools
                        && iteration == 0
                        && dispatch_index == dispatch_base
                        && !tools.is_empty()
                        && crate::grounding::looks_like_action_request(user_query)
                    {
                        let retry = llm
                            .complete_with_tools_choice(&messages, &tools, Some("required"))
                            .await
                            .map_err(|e| FamilyClawError::llm(tag_llm_error(&e)))?;
                        content = retry.content;
                        tool_calls_opt = retry.tool_calls;
                    }
                    let projection = (content.clone(), tool_calls_opt.clone());
                    durable
                        .step(&llm_step, {
                            let projection = projection.clone();
                            move || Ok(projection)
                        })
                        .map_err(|e| {
                            FamilyClawError::bus(format!("durable llm step failed: {e}"))
                        })?;
                    (content, tool_calls_opt)
                };

            let text = content.clone().unwrap_or_default();
            if !text.is_empty() {
                last_text = text;
            }

            let Some(tool_calls) = tool_calls_opt.filter(|c| !c.is_empty()) else {
                let answer = content.filter(|c| !c.is_empty()).unwrap_or(last_text);
                let guarded = crate::grounding::apply_grounding_guard(&answer, dispatch_count);
                return Ok((ToolLoopOutcome::Answer(guarded), dispatch_count));
            };

            messages.push(
                LlmMessage::assistant(content.unwrap_or_default())
                    .with_tool_calls(tool_calls.clone()),
            );

            for call in tool_calls {
                let tool_key = call.name.clone();
                let per_tool = tool_use_counts.entry(tool_key.clone()).or_insert(0);
                *per_tool += 1;
                let is_fs_read = tool_key.contains("fs_read");
                if is_fs_read {
                    fs_read_count += 1;
                }
                if *per_tool > TOOL_BUDGET_PER_NAME
                    || (is_fs_read && fs_read_count > TOOL_BUDGET_FS_READ)
                {
                    emit_tool(durable.is_replaying());
                    record_turn_audit_into(
                        turn_audit,
                        turn_id,
                        AuditKind::ToolDispatched,
                        now,
                        format!("tool '{}' skipped: per-turn budget exceeded", call.name),
                    );
                    messages.push(LlmMessage::tool_result(
                        call.id,
                        "SYSTEM: Tool budget exceeded for this turn. Reply to the operator now with your best answer â€” do not call more tools.",
                    ));
                    continue;
                }

                let Some(skill_id) = actions.lock().await.map_name_to_skill(&call.name) else {
                    emit_tool(durable.is_replaying());
                    record_turn_audit_into(
                        turn_audit,
                        turn_id,
                        AuditKind::ToolDispatched,
                        now,
                        format!("tool '{}' dispatched: unknown tool", call.name),
                    );
                    messages.push(LlmMessage::tool_result(
                        call.id,
                        format!("error: unknown tool '{}'", call.name),
                    ));
                    continue;
                };

                // D1: wrap the dispatch side effect + result in ITS OWN durable
                // step. In a fresh run the executor runs and the `SubmitOutcome`
                // (random `ApprovalId` + clock-derived TTL) is serialized to the
                // journal; on replay the stored `SubmitOutcome` is returned without
                // running the executor again â†’ side effect exactly once, ApprovalId/TTL
                // value-identical. `now` is injected (not `time::now()` inside the closure).
                let dispatch_step = format!("turn-{turn}-dispatch-{dispatch_index}");
                dispatch_index += 1;
                let replaying = durable.is_replaying();
                if emit_progress && !replaying {
                    if let (Some(sink), Some(target)) = (&progress_sink, &progress_target) {
                        let label = tool_progress_label(&call.name);
                        let should_send = last_progress_label.as_deref() != Some(label.as_str())
                            && progress_gate.allow();
                        if should_send {
                            let step = dispatch_index;
                            let body = format!("â†³ Step {step} Â· {label}");
                            if let Ok(msg) = OutboundMessage::progress(target, body) {
                                let _ = sink.send(msg);
                                progress_gate.record();
                            }
                            last_progress_label = Some(label);
                        }
                    }
                }
                // Both the replay and fresh-run branches journal/replay the SAME
                // type ([`DispatchRecord`]), so the serde format matches (the step
                // name and type are deterministic under replay). Any submit
                // error is carried in the `DispatchRecord::error` field, NOT
                // as the step's own error â€” this way replay returns the same record.
                let record: DispatchRecord = if replaying {
                    // Replay branch: the step is already in the journal â†’ return the
                    // stored record without running `submit_task` again. The closure
                    // is NOT run on replay, so its `Ok` value is irrelevant (but its
                    // TYPE must be `DispatchRecord` so serde matches).
                    durable
                        .step(&dispatch_step, || {
                            Ok(DispatchRecord {
                                task_id: ActionTaskId::nil(),
                                status: familyclaw_actions::task::TaskStatus::Failed,
                                pending_approval: None,
                                error: Some(
                                    "unreachable: replay returns journaled value".to_string(),
                                ),
                            })
                        })
                        .map_err(|e| {
                            FamilyClawError::bus(format!("durable dispatch replay failed: {e}"))
                        })?
                } else {
                    // Fresh-run branch: run `submit_task` (the side effect) NOW, in the
                    // proper async context, and record its result in the step. We do not
                    // run it inside the `step` closure (the closure is synchronous) â€” we
                    // run it before wrapping and journal the finished result.
                    //
                    // ðŸ”‘ KEYSTONE (exactly-once across a SIGKILL boundary): the dispatch
                    // runs **idempotently** through the runtime's outbox, keyed by the
                    // same deterministic `dispatch_step` name. This closes the window
                    // BETWEEN the side effect's (`submit_task`) execution and the
                    // `durable.step` journaling BELOW it: if the process is killed in
                    // between, the side effect is already committed in the outbox, so a
                    // restart/replay returns the same outcome without running the side
                    // effect again â€” regardless of whether the journal row managed to be
                    // created. (`submit_task_as` alone does not protect this window.)
                    let outcome = {
                        let mut rt = actions.lock().await;
                        // Submit the task under THIS being's (`being_id`) name, so that
                        // the per-being rate limit for approval-requiring tools applies
                        // correctly and does not collapse onto the runtime's generic
                        // default being.
                        rt.submit_task_idempotent(
                            &dispatch_step,
                            being_id,
                            skill_id,
                            call.arguments.clone(),
                            now,
                        )
                        .await
                    };
                    let record = DispatchRecord::from_outcome(&outcome);
                    durable
                        .step(&dispatch_step, {
                            let record = record.clone();
                            move || Ok(record)
                        })
                        .map_err(|e| {
                            FamilyClawError::bus(format!("durable dispatch step failed: {e}"))
                        })?
                };

                if let Some(err) = record.error.clone() {
                    // Execution error (journaled): feed the error back to the model
                    // (it may correct the call), CONTINUE within the budget. Same
                    // behavior as the non-durable path; replay returns the same error.
                    {
                        let e = err;
                        warn!(
                            agent = agent_name,
                            tool = call.name.as_str(),
                            error = %e,
                            "tool loop (durable): submit_task failed â€” feeding error result, continuing"
                        );
                        emit_tool(replaying);
                        record_turn_audit_into(
                            turn_audit,
                            turn_id,
                            AuditKind::ToolDispatched,
                            now,
                            format!("tool '{}' dispatched: failed: {e}", call.name),
                        );
                        messages.push(LlmMessage::tool_result(
                            call.id,
                            format_tool_failure_for_model(&call.name, &e),
                        ));
                        continue;
                    }
                }

                if let Some(approval_id) = record.pending_approval {
                    // Tool requiring approval â†’ INTERNAL control state. The redacted
                    // summary is fetched from the pending record (no secrets).
                    let redacted_summary = {
                        let rt = actions.lock().await;
                        rt.pending_summary_for(approval_id).unwrap_or_else(|| {
                            format!("tool '{}' awaiting human approval", call.name)
                        })
                    };
                    emit_tool(replaying);
                    record_turn_audit_into(
                        turn_audit,
                        turn_id,
                        AuditKind::ToolDispatched,
                        now,
                        format!(
                            "tool '{}' dispatched: awaiting approval ({redacted_summary})",
                            call.name
                        ),
                    );
                    return Ok((
                        ToolLoopOutcome::AwaitingApproval {
                            tool: call.name.clone(),
                            approval_id,
                            redacted_summary,
                            messages: messages.clone(),
                            tool_call_id: call.id.clone(),
                            arguments: call.arguments.clone(),
                        },
                        dispatch_count,
                    ));
                }

                if record.error.is_none() {
                    dispatch_count = dispatch_count.saturating_add(1);
                }

                // Safe / auto-run: feed the (redacted) result back to the model.
                // The proof is fetched from the runtime by task_id (fresh + replay:
                // the same task_id was journaled, so the proof is found on both
                // paths in a fresh run; on replay the proof may be missing â†’
                // tool_result_text falls back to a status description, which is
                // acceptable because replay never leaks out to the user).
                let result_text = {
                    let rt = actions.lock().await;
                    tool_result_text_for(&rt, record.task_id, record.status)
                };
                emit_tool(replaying);
                record_turn_audit_into(
                    turn_audit,
                    turn_id,
                    AuditKind::ToolDispatched,
                    now,
                    format!("tool '{}' dispatched: {result_text}", call.name),
                );
                messages.push(LlmMessage::tool_result(call.id, result_text));
            }
        }

        if last_text.is_empty() {
            if let Some(recovered) = recover_user_visible_reply(llm, &messages).await {
                let guarded = crate::grounding::apply_grounding_guard(&recovered, dispatch_count);
                return Ok((ToolLoopOutcome::Answer(guarded), dispatch_count));
            }
            Ok((
                ToolLoopOutcome::MaxIterations { iterations: budget },
                dispatch_count,
            ))
        } else {
            let guarded = crate::grounding::apply_grounding_guard(&last_text, dispatch_count);
            Ok((ToolLoopOutcome::Answer(guarded), dispatch_count))
        }
    }

    /// Disk-backed operator status (no LLM theater).
    fn execute_operator_status_bootstrap(&mut self, step_name: &str) -> Result<String> {
        let bootstrap_step = format!("{step_name}-operator-status");
        if self.durable.is_replaying() {
            return self
                .durable
                .step(&bootstrap_step, || {
                    Err("unreachable: replay returns journaled status reply".to_string())
                })
                .map_err(|e| FamilyClawError::bus(format!("status replay failed: {e}")));
        }
        let home = crate::identity::operator_home_root().ok_or_else(|| {
            FamilyClawError::config(
                "operator status requires FAMILYCLAW_FS_READ_ALLOW or FAMILYCLAW_FILE_WRITE_ALLOW"
                    .to_string(),
            )
        })?;
        let reply = crate::identity::operator_status_report(&home);
        self.durable
            .step(&bootstrap_step, {
                let reply = reply.clone();
                move || Ok(reply)
            })
            .map_err(|e| FamilyClawError::bus(format!("status step failed: {e}")))?;
        Ok(reply)
    }

    /// Handles operator APPROVE/DENY chat commands via `ActionRuntime`.
    async fn handle_operator_approval_command(
        &self,
        message: &BusMessage,
        origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>> {
        if !crate::identity::is_operator_origin(origin) || self.durable.is_replaying() {
            return Ok(None);
        }
        let Some(cmd) = crate::identity::parse_operator_approval_command(message) else {
            return Ok(None);
        };
        let Some(actions) = self.actions.as_ref() else {
            return Ok(Some("ActionRuntime ei ole kytketty.".to_string()));
        };
        let now = time::now();
        match cmd {
            crate::identity::OperatorApprovalCommand::Approve(id_str) => {
                let id: ApprovalId = id_str.parse().map_err(|_| {
                    FamilyClawError::config(format!("invalid approval id: {id_str}"))
                })?;
                {
                    let mut rt = actions.lock().await;
                    rt.approve(id, now)
                        .await
                        .map_err(|e| FamilyClawError::bus(e.to_string()))?;
                }
                self.handle_resume_signal(&id.to_string(), now).await?;
                Ok(Some(format!(
                    "APPROVE OK: {id} â€” resume signaali lÃ¤hetetty."
                )))
            }
            crate::identity::OperatorApprovalCommand::Deny(id_str) => {
                let id: ApprovalId = id_str.parse().map_err(|_| {
                    FamilyClawError::config(format!("invalid approval id: {id_str}"))
                })?;
                let mut rt = actions.lock().await;
                rt.deny_pending(id, now)
                    .await
                    .map_err(|e| FamilyClawError::bus(e.to_string()))?;
                Ok(Some(format!("DENY OK: {id} â€” tehtÃ¤vÃ¤ peruutettu.")))
            }
        }
    }

    async fn record_verified_tool_memory(&self, dispatch_count: u32, summary: &str) -> Result<()> {
        let snippet: String = summary.chars().take(240).collect();
        let content = format!("verified tool turn (dispatch={dispatch_count}): {snippet}");
        let memory = Memory::builder(content)
            .source("tool_verified")
            .tags(["verified:tool".to_string()])
            .build();
        self.memory.add(memory).await?;
        Ok(())
    }

    /// Deterministic TOP 20 #1 bootstrap for operator "Tee se!" / "JATKA".
    ///
    /// Runs real `file_write_allowlisted` tasks (memory.md + research/log.md),
    /// journals the final reply for replay, and never hallucinates completion.
    async fn execute_operator_top20_bootstrap(&mut self, step_name: &str) -> Result<String> {
        let bootstrap_step = format!("{step_name}-operator-bootstrap");
        if self.durable.is_replaying() {
            return self
                .durable
                .step(&bootstrap_step, || {
                    Err("unreachable: replay returns journaled bootstrap reply".to_string())
                })
                .map_err(|e| FamilyClawError::bus(format!("bootstrap replay failed: {e}")));
        }

        let actions = self.actions.as_ref().ok_or_else(|| {
            FamilyClawError::config("operator bootstrap requires ActionRuntime".to_string())
        })?;
        let home = crate::identity::operator_home_root().ok_or_else(|| {
            FamilyClawError::config(
                "operator bootstrap requires FAMILYCLAW_FILE_WRITE_ALLOW or FAMILYCLAW_FS_READ_ALLOW"
                    .to_string(),
            )
        })?;
        let now = time::now();
        let now_iso = familyclaw_core::time::to_rfc3339(now);
        let writes = crate::identity::operator_top20_bootstrap_plan(&home, &now_iso);
        let being_id = self.being_id.to_string();
        let mut written = Vec::new();
        let mut blocked: Option<String> = None;

        for (idx, (rel_display, payload)) in writes.into_iter().enumerate() {
            let dispatch_step = format!("{step_name}-bootstrap-{idx}");
            let skill_id = {
                let rt = actions.lock().await;
                rt.map_name_to_skill("file_write_allowlisted")
                    .ok_or_else(|| {
                        FamilyClawError::config(
                            "file_write_allowlisted not registered in ActionRuntime".to_string(),
                        )
                    })?
            };
            let outcome = {
                let mut rt = actions.lock().await;
                rt.submit_task_idempotent(&dispatch_step, &being_id, skill_id, payload, now)
                    .await?
            };
            if outcome.awaiting_approval() {
                blocked = Some(format!(
                    "file_write odottaa hyvÃ¤ksyntÃ¤Ã¤ (task {})",
                    outcome.task_id
                ));
                break;
            }
            if outcome.status != familyclaw_actions::task::TaskStatus::Done {
                blocked = Some(format!(
                    "file_write status {:?} for {rel_display}",
                    outcome.status
                ));
                break;
            }
            written.push(rel_display);
        }

        let reply = if let Some(reason) = blocked {
            crate::identity::operator_top20_bootstrap_blocked_reply(&reason)
        } else {
            crate::identity::operator_top20_bootstrap_done_reply(&home, &written)
        };

        self.durable
            .step(&bootstrap_step, {
                let reply = reply.clone();
                move || Ok(reply)
            })
            .map_err(|e| FamilyClawError::bus(format!("bootstrap step failed: {e}")))?;
        Ok(reply)
    }

    /// **Fresh actions-turn thinking, durable-journaled** (D1 production
    /// path). Runs [`drive_tool_loop_durable`](Self::drive_tool_loop_durable)
    /// over `&mut self.durable`, records the outcome (`-think` text or
    /// `-suspend` summary), and persists any resumable turn â€”
    /// exactly like the actions branch of [`think_with_origin`](Self::think_with_origin),
    /// but every tool dispatch is in its own durable step, so
    /// partial progress within the turn survives a crash.
    ///
    /// Handles BOTH a fresh run AND a replay: the loop checks
    /// `durable.is_replaying()` per dispatch, so in a replayed turn already
    /// journaled dispatches are returned from the journal without running the
    /// executor again, and only past the end of the journal does it continue fresh.
    /// Finally the `-think`/`-suspend` step closes the turn (same naming as the
    /// single-shot path).
    ///
    /// Returns `(thought, suspend)`: at most one is `Some`
    /// (mutually exclusive). `now` is read **once** and injected through the whole loop
    /// (D1) â€” the clock is not read inside the loop logic.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails unrecoverably.
    /// - [`FamilyClawError::Bus`] if a durable step fails.
    // The fresh/replay branches + Answer/Suspend/MaxIter outcome mapping +
    // resumable persistence form one logical unit; splitting it up would
    // fragment the turn-closing logic without a clarity benefit.
    #[allow(clippy::type_complexity, clippy::too_many_lines)]
    async fn think_actions_durable(
        &mut self,
        message: &BusMessage,
        origin: Option<&familyclaw_bus::MessageOrigin>,
        turn: u64,
    ) -> Result<(Option<String>, Option<(ApprovalId, String)>)> {
        // Fail-closed: if the LLM or the action runtime is not installed,
        // this path is a no-op (the single-shot path handles the reply). NO
        // `expect`/panic on the production path â€” return a harmless "no answer"
        // (same semantics as the former `is_none` guard, but without a separate
        // `expect` unwrap later on).
        let Some(actions) = self.actions.as_ref() else {
            return Ok((None, None));
        };
        // Clone the shared handles before the `&mut self.durable` borrow, so that
        // disjoint-borrow works (Arc handles + owned values, not `&self`).
        let actions = Arc::clone(actions);
        let max_iterations = self.tool_loop.max_iterations;
        let agent_name = self.config.name.clone();
        let being_id = self.being_id;
        let turn_audit = self.turn_audit.clone();
        // Clone the metrics sink before the `&mut self.durable` borrow (disjoint-borrow).
        let metrics_sink = self.metrics_sink.clone();

        let (system_prompt, query) = self.build_think_context(message, origin).await;
        // Short-term memory: include this conversation's earlier turns (RAM + store).
        let conv_key = self.conversation_key(origin);
        let history = self.conversation_history(&conv_key, origin).await;
        let now = time::now();
        let turn_id = ActionId::new();
        // TURN-AUDIT records do not belong to a replayed turn (the trail was
        // already recorded in the original run); record only in a fresh loop.
        let audit = if self.durable.is_replaying() {
            None
        } else {
            turn_audit.as_ref()
        };
        record_turn_audit_into(audit, turn_id, AuditKind::TurnStarted, now, "turn started");

        let messages = build_message_stack(system_prompt, &history, query.clone());
        // The LLM handle is an `LlmFailover` (not `Clone`); it is read from `self`
        // separately at the same time as `&mut self.durable` â€” disjoint field
        // borrow works because `llm` and `durable` are different fields.
        //
        // Fail-closed: if the LLM handle is (no longer) present, return harmlessly
        // without an answer â€” NO `expect`/panic on the production path. (The
        // earlier `actions` guard does not guarantee the LLM's presence, so this
        // is checked separately right before borrowing the handle.)
        let Some(llm) = self.llm.as_ref() else {
            return Ok((None, None));
        };
        let progress_sink = if should_emit_public_progress(origin) {
            self.reply_sink.clone()
        } else {
            None
        };
        let progress_target = if should_emit_public_progress(origin) {
            self.reply_target_for_origin(origin)
        } else {
            None
        };
        let outcome = Self::drive_tool_loop_durable(
            llm,
            &actions,
            &mut self.durable,
            turn,
            0,
            &agent_name,
            &being_id.to_string(),
            turn_audit.as_ref(),
            metrics_sink.as_ref(),
            progress_sink,
            progress_target,
            messages,
            String::new(),
            max_iterations,
            now,
            turn_id,
            origin,
            &query,
        )
        .await?;

        let think_step = format!("turn-{turn}-think");
        match outcome.0 {
            ToolLoopOutcome::Answer(text) => {
                if outcome.1 > 0 && !self.durable.is_replaying() {
                    let _ = self.record_verified_tool_memory(outcome.1, &text).await;
                }
                record_turn_audit_into(
                    audit,
                    turn_id,
                    AuditKind::TurnAnswered,
                    now,
                    format!("answered ({} chars)", text.chars().count()),
                );
                // Close the turn with the `-think` step (returned from the journal on replay).
                let thought = self
                    .durable
                    .step(&think_step, {
                        let text = text.clone();
                        move || Ok(text)
                    })
                    .map_err(|e| FamilyClawError::bus(format!("durable think step failed: {e}")))?;
                Ok((Some(thought).filter(|s| !s.is_empty()), None))
            }
            ToolLoopOutcome::AwaitingApproval {
                tool,
                approval_id,
                redacted_summary,
                messages,
                tool_call_id,
                arguments,
            } => {
                // Close `-think` as empty, so the replay cursor stays aligned
                // (suspend produces a separate `-suspend` entry below).
                let _ = self
                    .durable
                    .step(&think_step, || Ok(String::new()))
                    .map_err(|e| FamilyClawError::bus(format!("durable think step failed: {e}")))?;

                // Persist the resumable turn (resume bridge) â€” only on a fresh
                // run (on replay it was already persisted in the original run).
                if !self.durable.is_replaying() {
                    let expires_at = self
                        .pending_expiry_for(&actions, approval_id)
                        .await
                        .unwrap_or_else(|| {
                            now + chrono::Duration::minutes(RESUMABLE_DEFAULT_TTL_MINUTES)
                        });
                    let safe_messages = redact_messages_for_resume(&messages);
                    let resumable = ResumableTurn::new(
                        approval_id,
                        being_id.to_string(),
                        origin.cloned(),
                        safe_messages,
                        tool_call_id,
                        tool.clone(),
                        &arguments,
                        redacted_summary.clone(),
                        now,
                        expires_at,
                    )
                    .with_policy_snapshot(format!("tool '{tool}' requires human approval"))
                    .with_durable_position(turn, 0);
                    if let Err(e) = self.resumable.put(resumable) {
                        warn!(
                            agent = agent_name,
                            %approval_id,
                            error = %e,
                            "resumable turn persist failed â€” resume will not be possible for this approval"
                        );
                    }
                }
                record_turn_audit_into(
                    audit,
                    turn_id,
                    AuditKind::TurnSuspended,
                    now,
                    format!("suspended awaiting approval {approval_id}: {redacted_summary}"),
                );
                // `-suspend` entry (same format as the single-shot path):
                // "<approval_id>|<redacted_summary>" â€” no raw payload.
                let suspend_step = format!("turn-{turn}-suspend");
                let payload = format!("{approval_id}|{redacted_summary}");
                if let Err(e) = self.durable.step(&suspend_step, {
                    let payload = payload.clone();
                    move || Ok(payload)
                }) {
                    warn!("durable suspend step failed (non-fatal): {e}");
                }
                Ok((None, Some((approval_id, redacted_summary))))
            }
            ToolLoopOutcome::MaxIterations { iterations } => {
                record_turn_audit_into(
                    audit,
                    turn_id,
                    AuditKind::TurnMaxIterations,
                    now,
                    format!("reached max iterations ({iterations}) without answer"),
                );
                let text = recovery_fallback_reply();
                let thought = self
                    .durable
                    .step(&think_step, {
                        let text = text.clone();
                        move || Ok(text)
                    })
                    .map_err(|e| FamilyClawError::bus(format!("durable think step failed: {e}")))?;
                Ok((Some(thought).filter(|s| !s.is_empty()), None))
            }
        }
    }
    /// Runs the **tool loop** (Phase 1 keystone): a SpatialClaw-style loop
    /// in which the model calls a tool, the result is fed back, and it
    /// cycles until it stops.
    ///
    /// ## Steps per round
    /// 1. Build tool definitions from the runtime's published MCP descriptions
    ///    ([`ActionRuntime::tool_definitions`] â†’ [`ToolDefinition`]) â€” only
    ///    valid ones ([`ToolDefinition::validate`]) are offered to the model.
    /// 2. Call [`LlmFailover::complete_with_tools`].
    /// 3. **No tool calls** â†’ return the model's text (stop).
    /// 4. For each tool call:
    ///    - **unknown tool** ([`ActionRuntime::map_name_to_skill`] = `None`)
    ///      â†’ push an error `tool_result` and CONTINUE (consumes the round,
    ///      does not abort and does not get stuck in an infinite retry),
    ///    - **requires approval** ([`SubmitOutcome::pending_approval`] = `Some`)
    ///      â†’ return [`ToolLoopOutcome::AwaitingApproval`] (an internal control
    ///      state with a typed `approval_id` + redacted summary, NOT to the
    ///      user); [`think`](Self::think) translates it into
    ///      [`ThinkOutcome::Suspended`],
    ///    - **safe / auto-run** â†’ push the result as a `tool_result` and continue.
    /// 5. **Budget exhausted** â†’ return [`ToolLoopOutcome::Answer`] (the latest text)
    ///    or [`ToolLoopOutcome::MaxIterations`] (no answer).
    ///
    /// ## User boundary
    /// Only [`ToolLoopOutcome::Answer`] is intended for the end user.
    /// The control states ([`ToolLoopOutcome::AwaitingApproval`],
    /// [`ToolLoopOutcome::MaxIterations`]) are internal to the developer â€” their
    /// separation at the type level prevents a transient marker (e.g. a raw
    /// `approval_id`) from leaking through the reply pipeline to the user.
    ///
    /// Never panics: all error paths return as a [`Result`] or
    /// continue the loop within the budget.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails unrecoverably.
    async fn run_tool_loop(
        &self,
        llm: &LlmFailover,
        actions: &Arc<Mutex<ActionRuntime>>,
        messages: Vec<LlmMessage>,
        now: Timestamp,
        turn_id: ActionId,
    ) -> Result<ToolLoopOutcome> {
        // Run the loop from a fresh message stack with the full round budget.
        // `now` is injected (D1): the clock is not read inside the loop logic,
        // so that task dispatch uses this same, journalable timestamp.
        // `turn_id` (TURN-AUDIT) correlates the loop's tool calls to this turn.
        self.drive_tool_loop(
            llm,
            actions,
            messages,
            String::new(),
            self.tool_loop.max_iterations,
            now,
            turn_id,
            // Fresh turn: no idempotent continuation key (former behavior).
            None,
        )
        .await
    }

    /// **Shared engine** for the tool loop: runs the loop from the given
    /// message stack until the model stops, a tool requires approval, or the
    /// round budget is exhausted.
    ///
    /// Shared between two entry points, so the logic is exactly one:
    /// - [`run_tool_loop`](Self::run_tool_loop) â€” fresh turn (system + user).
    /// - [`resume_approved`](Self::resume_approved) â€” resumed turn: restored
    ///   message stack + the already-fed result of the approved tool.
    ///
    /// `budget` is the remaining round count (resume continues with the same
    /// overall budget, it does not reset it). `last_text` is the latest model
    /// text (typically empty on resume). Behavior is otherwise identical
    /// to the original `run_tool_loop` â€” see its phase description.
    ///
    /// **Idempotent dispatch (`idempotent_key_prefix`):**
    /// - `None` â†’ fresh turn: dispatch via [`ActionRuntime::submit_task_as`]
    ///   (former behavior, unchanged â€” this path is already durable-protected
    ///   via [`drive_tool_loop_durable`] in production).
    /// - `Some(prefix)` â†’ **post-approval continuation** (resume): every
    ///   tool dispatch runs **idempotently**
    ///   ([`ActionRuntime::submit_task_idempotent`]) keyed by
    ///   `{prefix}-dispatch-{k}`, where `k` is the continuation's internal
    ///   dispatch number running across all rounds. The key is
    ///   **deterministic across a restart**: the same `prefix` (derived from
    ///   the approval id) + the same dispatch index â†’ the same key, so the
    ///   outbox deduplicates a crash-then-replay situation and the side
    ///   effect does not fire twice. This closes the last double-fire window
    ///   that remained on the non-durable dispatch path AFTER an approval was
    ///   granted.
    ///
    /// **Determinism (D1):** `now` is injected â€” the clock is **not** read
    /// inside the loop logic; all task dispatches use this same timestamp.
    /// This lets the caller journal the timestamp inside the step (value
    /// identical on replay) so the loop is not non-deterministic.
    ///
    /// **Turn audit (roadmap Â§6 D6):** `turn_id` correlates every tool call
    /// of this loop ([`AuditKind::ToolDispatched`]) to one turn.
    /// Events are only recorded if a collector is installed
    /// ([`with_turn_audit`](Self::with_turn_audit)); otherwise a no-op. `detail`
    /// is redacted before storage (no raw payload).
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails unrecoverably.
    // The tool loop engine is one tight loop; the arguments (llm, actions,
    // messages, last_text, budget, now, turn_id) are all state needed to
    // run the loop, and TURN-AUDIT added one more (`turn_id`).
    // Bundling them into a context struct would add indirection with no clarity benefit.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) async fn drive_tool_loop(
        &self,
        llm: &LlmFailover,
        actions: &Arc<Mutex<ActionRuntime>>,
        mut messages: Vec<LlmMessage>,
        mut last_text: String,
        budget: u32,
        now: Timestamp,
        turn_id: ActionId,
        idempotent_key_prefix: Option<&str>,
    ) -> Result<ToolLoopOutcome> {
        // Post-approval continuation dispatch number: running across ALL
        // rounds (not reset per round), so every idempotent dispatch key
        // is unique and deterministic under replay.
        // Only used when `idempotent_key_prefix` is `Some`.
        let mut dispatch_index: u32 = 0;
        for _ in 0..budget {
            // 1. Build tools from the runtime's MCP descriptions (lock held
            //    only for the duration of the descriptions â€” released before the LLM call).
            let tools = {
                let rt = actions.lock().await;
                build_tool_definitions(&rt.tool_definitions())
            };

            // 2. LLM call with tools.
            let result = llm
                .complete_with_tools(&messages, &tools)
                .await
                .map_err(|e| FamilyClawError::llm(tag_llm_error(&e)))?;

            if !result.text().is_empty() {
                last_text = result.text().to_string();
            }

            // 3. No tool calls â†’ the model stopped, return the text as the answer.
            //    Empty but present content (`Some("")`, which some
            //    OpenAI-compatible providers produce) is filtered out,
            //    so we return the earlier non-empty text instead of going silent.
            let Some(tool_calls) = result.tool_calls.filter(|c| !c.is_empty()) else {
                let answer = result
                    .content
                    .filter(|c| !c.is_empty())
                    .unwrap_or(last_text);
                return Ok(ToolLoopOutcome::Answer(answer));
            };

            // Append the model's assistant turn (with its tool calls) to the history,
            // so subsequent tool_result messages bind to the right call ids.
            messages.push(
                LlmMessage::assistant(result.content.unwrap_or_default())
                    .with_tool_calls(tool_calls.clone()),
            );

            // 4. Dispatch every tool call and feed the result back.
            for call in tool_calls {
                let Some(skill_id) = actions.lock().await.map_name_to_skill(&call.name) else {
                    // Unknown tool: feed back an error result, CONTINUE (consumes
                    // the round, does not abort the loop and does not get stuck in
                    // an infinite retry â€” the budget also bounds the error path).
                    debug!(
                        agent = self.config.name,
                        tool = call.name.as_str(),
                        "tool loop: unknown tool â€” feeding error result, continuing"
                    );
                    // TURN-AUDIT: the tool call was dispatched but the name was unknown.
                    // Only the skill name + status â€” no arguments and no payload.
                    self.record_turn_audit(
                        turn_id,
                        AuditKind::ToolDispatched,
                        now,
                        format!("tool '{}' dispatched: unknown tool", call.name),
                    );
                    messages.push(LlmMessage::tool_result(
                        call.id,
                        format!("error: unknown tool '{}'", call.name),
                    ));
                    continue;
                };

                let outcome = {
                    let mut rt = actions.lock().await;
                    // D1: injected `now` (not `time::now()` inside the loop) â€”
                    // the same timestamp that can be journaled deterministically.
                    // Submit under THIS being's (`being_id`) name, so the per-being
                    // rate limit for approval-requiring tools applies correctly and
                    // does not collapse onto the runtime's generic default being.
                    if let Some(prefix) = idempotent_key_prefix {
                        // ðŸ”‘ Post-approval continuation: dispatch idempotently
                        // with the deterministic key `{prefix}-dispatch-{k}`.
                        // `k` (dispatch_index) is stable across a restart (same prefix
                        // + same index â†’ same key), so the outbox deduplicates a
                        // crash-then-replay situation and the side effect does not
                        // fire twice. The index is incremented BEFORE dispatch, so
                        // replay hits the same key in the same order.
                        let key = format!("{prefix}-dispatch-{dispatch_index}");
                        dispatch_index += 1;
                        rt.submit_task_idempotent(
                            &key,
                            &self.being_id.to_string(),
                            skill_id,
                            call.arguments.clone(),
                            now,
                        )
                        .await
                    } else {
                        // Fresh (non-continuation) turn: the former non-idempotent path.
                        // This path is already durable-protected via `drive_tool_loop_durable`
                        // in production; here `submit_task_as` preserves the
                        // original behavior byte-for-byte.
                        rt.submit_task_as(
                            &self.being_id.to_string(),
                            skill_id,
                            call.arguments.clone(),
                            now,
                        )
                        .await
                    }
                };

                match outcome {
                    Ok(submit) if submit.pending_approval.is_some() => {
                        // Tool requiring approval â†’ return the INTERNAL control
                        // state [`ToolLoopOutcome::AwaitingApproval`], NOT a string
                        // to be routed to the user. `approval_id` remains in
                        // [`ActionRuntime`]'s state for the operator's later
                        // `approve` call â€” it is never sent to the end user. The
                        // turn does not hang, and the approval-requiring action
                        // does not execute without permission.
                        // [`think`](Self::think) translates this into a first-class
                        // [`ThinkOutcome::Suspended`].
                        //
                        // `pending_approval` is `Some` in this branch (the branch
                        // condition guarantees it), so we read the typed id directly.
                        // A redacted, operator-safe summary is fetched from the
                        // pending record â€” derived only from the skill's name and
                        // identifiers, not secrets. If the summary is not found for
                        // some reason, we use a neutral placeholder
                        // (never a raw payload/arguments).
                        let Some(approval_id) = submit.pending_approval else {
                            // Unreachable (branch condition = is_some), but we do not
                            // panic on the production path â€” continue the loop.
                            continue;
                        };
                        let redacted_summary = {
                            let rt = actions.lock().await;
                            rt.pending_summary_for(approval_id).unwrap_or_else(|| {
                                format!("tool '{}' awaiting human approval", call.name)
                            })
                        };
                        // TURN-AUDIT: the tool was dispatched, but requires approval
                        // (redacted summary â€” no arguments and no payload).
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::ToolDispatched,
                            now,
                            format!(
                                "tool '{}' dispatched: awaiting approval ({redacted_summary})",
                                call.name
                            ),
                        );
                        // Resume state (roadmap Â§6): the message stack is in
                        // EXACTLY the right state for resuming â€” the assistant turn
                        // (with its tool calls) has already been appended (above), but
                        // THIS call's `tool_result` does NOT exist yet. Resume injects
                        // the approved tool's result into the `tool_call_id` and
                        // continues from the stack. `arguments` is given only to be
                        // hashed ([`ResumableTurn::new`] computes SHA-256, does not
                        // store the raw value). We clone the stack because the loop's
                        // own `messages` only moves out via this return.
                        return Ok(ToolLoopOutcome::AwaitingApproval {
                            tool: call.name.clone(),
                            approval_id,
                            redacted_summary,
                            messages: messages.clone(),
                            tool_call_id: call.id.clone(),
                            arguments: call.arguments.clone(),
                        });
                    }
                    Ok(submit) => {
                        // Safe / auto-run: feed the (redacted) result back to
                        // the model. The proof contains redacted output
                        // without secrets.
                        let result_text = {
                            let rt = actions.lock().await;
                            tool_result_text(&rt, &submit)
                        };
                        // TURN-AUDIT: the tool was dispatched and executed. The result is
                        // already redacted (`tool_result_text` uses the redacted
                        // proof), and `record_turn_audit` redacts it once more
                        // (defense in depth).
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::ToolDispatched,
                            now,
                            format!("tool '{}' dispatched: {result_text}", call.name),
                        );
                        messages.push(LlmMessage::tool_result(call.id, result_text));
                    }
                    Err(e) => {
                        // Execution error: feed the error back to the model (it may
                        // correct the call), CONTINUE within the budget.
                        warn!(
                            agent = self.config.name,
                            tool = call.name.as_str(),
                            error = %e,
                            "tool loop: submit_task failed â€” feeding error result, continuing"
                        );
                        // TURN-AUDIT: the tool was dispatched but execution failed.
                        // The error text is redacted (it may reflect arguments).
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::ToolDispatched,
                            now,
                            format!("tool '{}' dispatched: failed: {e}", call.name),
                        );
                        messages.push(LlmMessage::tool_result(
                            call.id,
                            format_tool_failure_for_model(&call.name, &e),
                        ));
                    }
                }
            }
        }

        // 5. Budget exhausted before stopping. NO panic. If the model managed
        //    to produce text, it is the best available answer â†’ `Answer`.
        //    Otherwise return the INTERNAL control state [`ToolLoopOutcome::MaxIterations`]
        //    â€” a robotic max-iter marker is NOT routed to the user.
        //    `iterations` reports the budget given for this run (resume continues
        //    with the remaining budget, so the number reflects the actual limit).
        if last_text.is_empty() {
            if let Some(recovered) = recover_user_visible_reply(llm, &messages).await {
                return Ok(ToolLoopOutcome::Answer(recovered));
            }
            Ok(ToolLoopOutcome::MaxIterations { iterations: budget })
        } else {
            Ok(ToolLoopOutcome::Answer(last_text))
        }
    }
    async fn pending_expiry_for(
        &self,
        actions: &Arc<Mutex<ActionRuntime>>,
        approval_id: ApprovalId,
    ) -> Option<Timestamp> {
        let rt = actions.lock().await;
        rt.pending_expiry_for(approval_id)
    }

    /// **Resumes a suspended turn once approval has been granted** (suspend/resume
    /// bridge, roadmap Â§6 â€” resume side).
    ///
    /// When [`think`](Self::think)/[`think_with_origin`](Self::think_with_origin)'s
    /// tool loop suspended awaiting approval, the resumable turn's state
    /// ([`ResumableTurn`]) was persisted to the resumable turn store
    /// ([`resumable_store`](Self::resumable_store)).
    /// This method:
    ///
    /// 1. **loads** the resumable turn by `approval_id` (fail-closed: unknown
    ///    or expired â†’ error, no panic, no side effects),
    /// 2. **consumes the approval** ([`ActionRuntime::approve`]) â†’ the suspended
    ///    action is executed to completion **exactly once** (payload-bound,
    ///    single-use â€” see [`familyclaw_actions::approval::ApprovalLedger::consume`]),
    /// 3. **injects** the approved tool's (redacted) result back into the
    ///    restored message stack, bound to the `tool_call_id`,
    /// 4. **continues the tool loop** from where it left off
    ///    (the internal tool loop engine) â€” the model may now respond
    ///    conclusively or request more tools (possibly a new suspend),
    /// 5. **consumes the resumable turn** (removes it from the store) after a
    ///    successful `approve`, so it cannot be resumed a second time.
    ///
    /// Returns a [`ThinkOutcome`]:
    /// - [`Reply`](ThinkOutcome::Reply) when the model produced a final answer,
    /// - [`Suspended`](ThinkOutcome::Suspended) when the continuation required a
    ///   **new** approval (a new resumable turn has then already been persisted),
    /// - [`NoReply`](ThinkOutcome::NoReply) when the continuation hit the round
    ///   budget without text, or there is no LLM client.
    ///
    /// ## Determinism (D1)
    /// `now` is **injected** â€” the clock is not read inside this method. The
    /// same timestamp governs both the approval consumption's expiry check AND
    /// the continued tool loop's task dispatches, so the caller can journal it
    /// inside the step (value identical on replay).
    ///
    /// ## User boundary + secrets
    /// Only `Reply` is intended for the user. Raw secrets were never stored in
    /// the resumable turn (see [`ResumableTurn`]), and the injected tool result
    /// is derived from a **redacted** proof.
    ///
    /// ## Isolation between beings (defense in depth)
    /// A resumable turn belongs to the being that persisted it. Resume
    /// **refuses** to continue a turn whose [`ResumableTurn::being_id`]
    /// differs from this agent's own identifier â€” one being cannot continue
    /// another being's suspended turn nor consume its approval.
    /// The check is done **before** consuming the approval, so a mismatch leaves
    /// no trace (fail-closed: no consumption, no removal, no side effect).
    ///
    /// # Errors
    /// - [`FamilyClawError::InvalidInput`] if there is no resumable turn for
    ///   `approval_id` (unknown/consumed), it belongs to **another
    ///   being** (ownership mismatch), it is expired, the agent has no
    ///   action runtime installed ([`with_actions`](Self::with_actions)),
    ///   or consuming the approval ([`ActionRuntime::approve`]) fails (e.g.
    ///   payload mismatch) â€” all fail-closed, no panic.
    /// - [`FamilyClawError::Llm`] if the continued LLM call fails.
    // Resume is a coherent sequence (load â†’ consume approval â†’ inject result
    // â†’ turn-audit resumed â†’ continue tool loop â†’ map outcome + stop_reason).
    // The TURN-AUDIT records pushed the line count over the ceiling; splitting
    // this up would fragment this logical unit.
    #[allow(clippy::too_many_lines)]
    pub async fn resume_approved(
        &self,
        approval_id: ApprovalId,
        now: Timestamp,
    ) -> Result<ThinkOutcome> {
        // 1. Load the resumable turn fail-closed. Unknown/consumed â†’ error
        //    (no panic, no side effects).
        let turn = self
            .resumable
            .get(approval_id)
            .map_err(|e| FamilyClawError::invalid_input(format!("resumable load failed: {e}")))?
            .ok_or_else(|| {
                FamilyClawError::invalid_input(format!(
                    "no resumable turn for approval {approval_id} (unknown or already resumed)"
                ))
            })?;

        // 1b. OWNERSHIP CHECK (isolation invariant, defense in depth).
        //
        // A resumable turn belongs exactly to the being that persisted it at
        // suspension time ([`ResumableTurn::being_id`]). The store can be shared
        // across multiple beings, and the key is just `approval_id` â€”
        // so without this check, one being could resume **another being's**
        // suspended turn (and consume its approval + run its side effect
        // in its own context). That would break the isolation between beings.
        //
        // **Invariant:** `turn.being_id == self.being_id`. This is
        // defense in depth â€” the caller should route resume to the correct
        // being before arriving here, but this still verifies it at the
        // boundary where the approval is about to be consumed.
        //
        // **Fail-closed:** a mismatch â†’ error BEFORE consuming the approval and
        // before running any of the tool loop. The approval is NOT consumed, the
        // resumable turn is NOT removed, the side effect is NOT run â€” the
        // foreign being leaves empty-handed and no trace is left. No panic.
        let self_being = self.being_id.to_string();
        if turn.being_id != self_being {
            return Err(FamilyClawError::invalid_input(format!(
                "resumable turn for approval {approval_id} belongs to another being \
                 (owner mismatch) â€” refusing to resume across beings"
            )));
        }

        // An expired resumable turn is refused fail-closed (same boundary as
        // the approval) â€” the permission is not consumed, no side effect is run.
        if turn.is_expired(now) {
            return Err(FamilyClawError::invalid_input(format!(
                "resumable turn for approval {approval_id} expired"
            )));
        }

        // Resume requires an action runtime (the same runtime that granted the permission).
        let Some(actions) = self.actions.as_ref() else {
            return Err(FamilyClawError::invalid_input(
                "resume_approved requires an ActionRuntime (call with_actions first)".to_string(),
            ));
        };

        // 2. Consume the approval â†’ the suspended action is executed to
        //    completion EXACTLY ONCE (payload-bound, single-use). `now` injected (D1).
        let submit = {
            let mut rt = actions.lock().await;
            rt.approve(approval_id, now)
                .await
                .map_err(|e| FamilyClawError::invalid_input(format!("approve failed: {e}")))?
        };

        // 3. Inject the approved tool's (redacted) result into the restored
        //    message stack, bound to the original tool_call_id.
        let mut messages = turn.messages;
        let result_text = {
            let rt = actions.lock().await;
            tool_result_text(&rt, &submit)
        };
        messages.push(LlmMessage::tool_result(turn.tool_call_id, result_text));

        // Approval consumed successfully â†’ consume the resumable turn
        // (single-use: cannot be resumed twice). Done BEFORE continuing the loop,
        // so that a possible new suspend persists ITS OWN resumable turn
        // without the old one being left hanging.
        if let Err(e) = self.resumable.remove(approval_id) {
            warn!(
                agent = self.config.name,
                %approval_id,
                error = %e,
                "resumable remove after approve failed (non-fatal) â€” turn already advanced"
            );
        }

        // TURN-AUDIT (roadmap Â§6 D6): this is a resumed turn â†’ a new
        // correlation id + a `TurnResumed` event. Recorded only once the
        // approval has been consumed successfully (above), so the audit trail
        // reflects an actual resume and not a failed attempt. No-op when
        // audit is not wired up.
        let turn_id = ActionId::new();
        self.record_turn_audit(
            turn_id,
            AuditKind::TurnResumed,
            now,
            format!("turn resumed after approval {approval_id}"),
        );

        // 4. Continue the tool loop from the restored stack. No LLM â†’ NoReply.
        let Some(llm) = self.llm.as_ref() else {
            return Ok(ThinkOutcome::NoReply);
        };
        // ðŸ”‘ IDEMPOTENT CONTINUATION (closes the last double-fire window):
        // tool dispatches made AFTER the approval is granted are routed through
        // `submit_task_idempotent` with a deterministic key prefix derived from
        // this continuation's approval id. The same
        // `approval_id` + the same continuation-internal dispatch index produce the
        // same key across a restart, so a crash-then-replay does not fire the
        // side effect again. (Previously this path called `submit_task_as`
        // directly, which made post-approval dispatches susceptible
        // to double-firing in the crash window.)
        let resume_dispatch_prefix = format!("resume-{approval_id}");
        // Continue with the FULL round budget: resume is a new "episode" in which
        // the model gets room to proceed again. The safety limit still bounds an
        // infinite loop.
        // `turn_id` correlates the continued loop's tool calls to this resume turn.
        let outcome = self
            .drive_tool_loop(
                llm,
                actions,
                messages,
                String::new(),
                self.tool_loop.max_iterations,
                now,
                turn_id,
                Some(&resume_dispatch_prefix),
            )
            .await?;

        match outcome {
            ToolLoopOutcome::Answer(text) => {
                // stop_reason = answered.
                self.record_turn_audit(
                    turn_id,
                    AuditKind::TurnAnswered,
                    now,
                    format!("answered ({} chars)", text.chars().count()),
                );
                Ok(ThinkOutcome::Reply(text))
            }
            ToolLoopOutcome::AwaitingApproval {
                tool,
                approval_id: next_id,
                redacted_summary,
                messages,
                tool_call_id,
                arguments,
            } => {
                // The continuation required a NEW approval â†’ persist a new resumable
                // turn (same invariant as the original suspend). The original
                // origin is preserved, so the reply is routed to the same
                // conversation even after a chained approval.
                let expires_at = self
                    .pending_expiry_for(actions, next_id)
                    .await
                    .unwrap_or_else(|| {
                        now + chrono::Duration::minutes(RESUMABLE_DEFAULT_TTL_MINUTES)
                    });
                let safe_messages = redact_messages_for_resume(&messages);
                let next_turn = ResumableTurn::new(
                    next_id,
                    self.being_id.to_string(),
                    turn.conversation_origin,
                    safe_messages,
                    tool_call_id,
                    tool.clone(),
                    &arguments,
                    redacted_summary.clone(),
                    now,
                    expires_at,
                )
                .with_policy_snapshot(format!("tool '{tool}' requires human approval"))
                .with_durable_position(self.turn_counter, 0);
                if let Err(e) = self.resumable.put(next_turn) {
                    warn!(
                        agent = self.config.name,
                        approval_id = %next_id,
                        error = %e,
                        "chained resumable turn persist failed â€” further resume not possible"
                    );
                }
                // stop_reason = suspended (chained new approval).
                self.record_turn_audit(
                    turn_id,
                    AuditKind::TurnSuspended,
                    now,
                    format!("suspended awaiting approval {next_id}: {redacted_summary}"),
                );
                Ok(ThinkOutcome::Suspended {
                    approval_id: next_id,
                    redacted_summary,
                })
            }
            ToolLoopOutcome::MaxIterations { iterations } => {
                self.record_turn_audit(
                    turn_id,
                    AuditKind::TurnMaxIterations,
                    now,
                    format!("reached max iterations ({iterations}) without answer"),
                );
                Ok(ThinkOutcome::Reply(recovery_fallback_reply()))
            }
        }
    }

    /// **The agent's side of the suspend/resume bridge**: handles the operator's
    /// approval-granted control signal
    /// ([`BusMessage::ResumeApproval`])
    /// by continuing the suspended turn to completion and pushing the final
    /// answer OUT to the reply sink.
    ///
    /// This is a **different path from a normal turn**: it does NOT start a new
    /// LLM turn and does not go through [`handle_turn_with_origin`](Self::handle_turn_with_origin),
    /// but is routed directly to [`resume_approved`](Self::resume_approved),
    /// which consumes the approval and continues the suspended tool loop from
    /// where it left off (idempotently). `now` (D1) is injected as on the resume path.
    ///
    /// ## Deriving the reply target (same logic as the normal route)
    /// The reply target is derived by **the same** rule as
    /// [`handle_turn_with_origin`](Self::handle_turn_with_origin)'s reply branch:
    /// primarily from the suspended turn's persisted
    /// [`conversation_origin`](crate::resumable::ResumableTurn::conversation_origin)
    /// (`reply_target()`), and if absent, from the agent's static
    /// [`with_reply_target`](Self::with_reply_target). The origin is peeked from
    /// the resumable turn **before** `resume_approved` consumes it;
    /// a peek error does not block continuation (fallback `None` â†’ static target).
    ///
    /// ## Outcome mapping
    /// - [`ThinkOutcome::Reply`] â†’ build an [`OutboundMessage`] and route it via
    ///   [`route_reply`](Self::route_reply) (NO bus publish â†’ echo-loop protection).
    /// - [`ThinkOutcome::Suspended`] â†’ the turn requires **further approval**;
    ///   no-op + `info!` (the operator grants the next approval separately).
    /// - [`ThinkOutcome::NoReply`] â†’ no-op.
    ///
    /// ## Fail-closed (a single resume does not crash the actor)
    /// - Invalid `approval_id` (parse error): `warn!` + `Ok(())`, no panic.
    /// - `resume_approved` error (unknown/consumed/expired/ownership
    ///   mismatch): `warn!` + `Ok(())` â€” same error boundary as the turn handler.
    /// - Reply-routing failure (closed sink): `warn!`, does not crash.
    ///
    /// # Errors
    /// Always returns `Ok(())` â€” all errors are handled fail-closed
    /// (logged), so a single resume signal cannot crash the actor.
    pub async fn handle_resume_signal(&self, approval_id: &str, now: Timestamp) -> Result<()> {
        // Parse the approval identifier fail-closed: an invalid string â†’ log +
        // no-op (no panic, no side effect).
        let Ok(id) = approval_id.parse::<ApprovalId>() else {
            warn!(
                agent = self.config.name,
                approval_id, "resume signal: invalid approval id â€” ignoring"
            );
            return Ok(());
        };

        // Peek the resumable turn's origin for the reply target BEFORE
        // `resume_approved` consumes (removes) the turn. A peek error does not
        // block continuation: fallback `None` â†’ the agent's static reply target
        // (same as the normal route's fallback). No `?` propagation: we do not
        // want a peek error to block consuming the approval.
        let reply_origin = match self.resumable.get(id) {
            Ok(turn) => turn.and_then(|t| t.conversation_origin),
            Err(e) => {
                debug!(
                    agent = self.config.name,
                    %id, error = %e,
                    "resume signal: resumable peek failed â€” falling back to static reply target"
                );
                None
            }
        };

        // Continue the turn to completion. Error (unknown/consumed/expired/
        // ownership mismatch) â†’ log + Ok (a single resume does not crash the
        // actor, same boundary as the turn handler).
        let outcome = match self.resume_approved(id, now).await {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!(
                    agent = self.config.name,
                    %id, error = %e,
                    "resume signal: resume_approved failed (non-fatal) â€” no reply"
                );
                return Ok(());
            }
        };

        match outcome {
            ThinkOutcome::Reply(text) => {
                // Reply target: same rule as the normal route â€” origin FIRST
                // (the suspended turn's `conversation_origin`), FALLBACK to the
                // agent's static reply target.
                let target: Option<&str> = reply_origin
                    .as_ref()
                    .map(familyclaw_bus::MessageOrigin::reply_target)
                    .or(self.reply_target.as_deref());
                let Some(target) = target else {
                    debug!(
                        agent = self.config.name,
                        %id,
                        "resume signal: no reply target (no origin, no static target) â€” dropping reply"
                    );
                    return Ok(());
                };
                match OutboundMessage::new(target, text) {
                    Ok(reply) => {
                        if let Err(e) = self.route_reply(reply) {
                            // A closed sink must not crash the actor â€” log and continue.
                            warn!(
                                agent = self.config.name,
                                %id, error = %e,
                                "resume signal: reply routing failed (non-fatal)"
                            );
                        }
                    }
                    Err(e) => warn!(
                        agent = self.config.name,
                        %id, error = %e,
                        "resume signal: reply build failed (non-fatal)"
                    ),
                }
            }
            ThinkOutcome::Suspended { approval_id, .. } => {
                // The continuation required a NEW approval (chained). This signal does
                // not grant it â€” no-op + info. The operator grants the next one separately.
                info!(
                    agent = self.config.name,
                    next_approval = %approval_id,
                    "resume signal: turn re-suspended awaiting further approval â€” no reply yet"
                );
            }
            ThinkOutcome::NoReply => {
                // No answer (e.g. max-iter or no LLM) â€” no-op.
            }
        }

        Ok(())
    }

    /// Handles a single turn **crash-resiliently** (without a per-message
    /// origin — uses the static reply target if set).
    ///
    /// This is a backward-compatible shell for
    /// [`handle_turn_with_origin`](Self::handle_turn_with_origin)
    /// with `origin = None`. The reply is routed to the agent's static
    /// [`with_reply_target`](Self::with_reply_target) target.
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] if the memory write fails.
    /// - [`FamilyClawError`] (wrapped) if a durable step fails.
    pub async fn handle_turn(
        &mut self,
        sender: BeingId,
        message: &BusMessage,
    ) -> Result<TurnOutcome> {
        self.handle_turn_with_origin(sender, message, None).await
    }

    /// Handles a single turn **crash-resiliently**, with a per-message origin
    /// ([`familyclaw_bus::MessageOrigin`]) (F2).
    ///
    /// The turn's *outcome* ([`TurnOutcome`]) is recorded in a durable step
    /// ([`DurableContext::step`]), so on restart already-executed turns are
    /// replayed from the journal without running side effects again
    /// (design Â§2.1). The memory write itself is performed according to the
    /// flag inferred by the step.
    ///
    /// ## Deriving the reply target (F2 core)
    /// The reply target is derived **per message**: if `origin` is given, the target
    /// is `origin.reply_target()` (the conversation the message came from). Otherwise
    /// it falls back to the agent's static [`with_reply_target`](Self::with_reply_target)
    /// value. This lets a single agent serve many conversations without
    /// replies leaking to the wrong target â€” and does not break the single-channel +
    /// static-target MVP behavior (`origin = None` â†’ the former path).
    ///
    /// Returns the turn's outcome.
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] if the memory write fails.
    /// - [`FamilyClawError`] (wrapped) if a durable step fails.
    // Turn handling is a coherent, sequential process; splitting it up just
    // for the line count would fragment this logical unit.
    #[allow(clippy::too_many_lines)]
    pub async fn handle_turn_with_origin(
        &mut self,
        sender: BeingId,
        message: &BusMessage,
        origin: Option<&familyclaw_bus::MessageOrigin>,
    ) -> Result<TurnOutcome> {
        let turn = self.turn_counter;
        self.turn_user_reply_sent.store(false, Ordering::Relaxed);
        self.turn_reply_suppressed.store(false, Ordering::Relaxed);
        let step_name = format!("turn-{turn}");

        // 1. Deterministic, side-effect-free inference in a durable step:
        //    what should happen in this turn? On replay this is returned
        //    from the journal â€” we do not query the clock or randomness inside the closure.
        let summary = summarize(sender, message);
        let remembered = should_remember(message);
        let outcome = TurnOutcome {
            turn,
            remembered,
            summary,
        };

        // Deterministic inference: turn outcome ready.
        // Idempotent handling of the (memory) side effect follows below (step 2).
        let recorded: TurnOutcome = self
            .durable
            .step(&step_name, {
                let outcome = outcome.clone();
                move || Ok(outcome)
            })
            .map_err(|e| FamilyClawError::bus(format!("durable turn step failed: {e}")))?;

        // 2. The side effect (memory write) runs only on a FRESH turn, not on
        //    replay: the memory was already recorded in the original run. This way
        //    the side effect happens exactly once over the whole workflow's
        //    lifetime, even if the process crashes and turns are replayed from the journal.
        //
        //    We build the memory SYNCHRONOUSLY (borrowing `&self`) and then
        //    move only the needed owned values (the Arc memory handle + the
        //    finished memory) across the `.await` boundary. This way the async
        //    future does not capture an `&Agent` reference, and it stays `Send`
        //    (Ractor requires it).
        // Idempotent memory write: runs ALWAYS (also on replay), because
        // MemoryStore::add skips duplicates based on turn_key.
        // This resolves the dual-write problem: if durable.step succeeds
        // but the process crashes before the memory_store.add call,
        // on replay the memory is written again and the store ignores it.
        if recorded.remembered {
            let memory_store = Arc::clone(&self.memory);
            let mut memory = self.build_memory(sender, message, origin);
            memory.turn_key = Some(format!("{}:turn-{}", self.config.name, turn));
            memory_store
                .add(memory)
                .await
                .map_err(|e| FamilyClawError::memory(format!("remember failed: {e}")))?;
        }

        // 3. Update the emotional state based on the message (local, non-durable).
        self.apply_emotional_effect(message);

        // 4. Emotional homeostasis: after every turn the emotional state drifts
        //    slightly back toward neutral. This prevents exponential saturation
        //    (a feedback loop) in continuous sibling conversations: without
        //    dampening, CONTAGION_FACTOR piles up emotional states without bound
        //    and agents "burn out" within a few dozen turns.
        self.apply_emotional_homeostasis();

        // 5. LLM thinking (a side effect): if an LLM client is configured,
        //    the agent "thinks" based on the message. LLM generation is an EXTERNAL
        //    side effect â†’ we run it in a fresh turn in the PROPER async
        //    context (not `block_on` inside a durable closure, which would
        //    panic on a `current_thread` runtime / could deadlock) and
        //    store the RESULT in a durable step. On replay we do not run
        //    `think` again â€” `durable.step` returns the stored text.
        //
        //    **Phase 1 governor filtering:** When a governor is installed AND
        //    the message is an `EmotionPulse` (siblings' "blood", not speech), it does
        //    NOT think at all. This prevents unnecessary LLM calls in affective
        //    pulse chains and ensures that only spoken messages produce an
        //    LLM answer. This is a deliberate fix for one of the most important
        //    Phase 1 gaps (on the pitfall list).
        let think_step = format!("{step_name}-think");
        let governor_filtered_pulse =
            self.governor.is_some() && matches!(message, BusMessage::EmotionPulse { .. });
        let governor_hesitate = self.governor.as_deref().is_some_and(|g| {
            let gov = EmotionActionGovernor::new(g);
            gov.decide(&self.emotion) == ActionDecision::Hesitate
        });
        if governor_filtered_pulse || governor_hesitate {
            self.turn_reply_suppressed.store(true, Ordering::Relaxed);
        }
        let will_think = self.llm.is_some() && !governor_filtered_pulse && !governor_hesitate;
        // Operator TOP20 bootstrap: deterministic file_write (no LLM theater).
        let operator_bootstrap_response = if will_think
            && !self.durable.is_replaying()
            && self.actions.is_some()
            && crate::identity::operator_execute_message(message, origin)
        {
            match self.execute_operator_top20_bootstrap(&step_name).await {
                Ok(reply) => Some(reply),
                Err(e) => {
                    warn!("operator bootstrap failed (non-fatal): {e}");
                    Some(crate::identity::operator_top20_bootstrap_blocked_reply(
                        &e.to_string(),
                    ))
                }
            }
        } else {
            None
        };
        let operator_status_response = if operator_bootstrap_response.is_none()
            && will_think
            && !self.durable.is_replaying()
            && crate::identity::operator_status_message(message, origin)
        {
            match self.execute_operator_status_bootstrap(&step_name) {
                Ok(reply) => Some(reply),
                Err(e) => {
                    warn!("operator status bootstrap failed (non-fatal): {e}");
                    Some(format!("STATUS ERROR: {e}"))
                }
            }
        } else {
            None
        };
        let operator_approval_response = if self.durable.is_replaying() {
            None
        } else {
            match self.handle_operator_approval_command(message, origin).await {
                Ok(reply) => reply,
                Err(e) => Some(format!("Approval command failed: {e}")),
            }
        };
        // Operator diagnostics fast path + brief ping fast path:
        // skip LLM when we can answer deterministically.
        let brief_ping_response = if operator_bootstrap_response.is_some() {
            operator_bootstrap_response
        } else if operator_approval_response.is_some() {
            operator_approval_response
        } else if operator_status_response.is_some() {
            operator_status_response
        } else if will_think && !self.durable.is_replaying() {
            let diag = crate::identity::operator_diagnostic_reply(message, origin);
            if diag.is_some() {
                diag
            } else {
                crate::identity::brief_ping_reply(&self.config.name, message)
            }
        } else {
            None
        };
        let will_think = will_think && brief_ping_response.is_none();
        let typing_abort = if !self.durable.is_replaying() && will_think {
            self.notify_turn_started(origin);
            self.spawn_typing_heartbeat(origin)
        } else {
            None
        };
        if let Ok(mut slot) = self.typing_abort.lock() {
            if let Some(old) = slot.take() {
                old.abort();
            }
            *slot = typing_abort;
        }
        // `thought_response` = the model's text reply (if `ThinkOutcome::Reply`),
        // `suspend` = the turn's suspension awaiting approval (if
        // `ThinkOutcome::Suspended`). These are mutually exclusive: one turn
        // produces at most one of them. `Suspended` does NOT go into the reply
        // pipeline â€” it is recorded in the turn's durable state for resume
        // (id + redacted summary), and is never routed to the user.
        let mut suspend: Option<(ApprovalId, String)> = None;
        let thought_response: Option<String> = if let Some(ref brief) = brief_ping_response {
            if !self.durable.is_replaying() {
                let think_step = format!("{step_name}-think");
                let _ = self.durable.step(&think_step, {
                    let brief = brief.clone();
                    move || Ok(brief)
                });
            }
            brief_ping_response
        } else if self.llm.is_none() {
            None
        } else if governor_filtered_pulse {
            // Phase 1: EmotionPulse = "blood", not thought. Log that this is
            // not run during replay, but think returns None on this turn.
            debug!(
                agent = self.config.name,
                "governor: skipping think() for EmotionPulse (filtered as 'blood', not speech)"
            );
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
        } else if governor_hesitate {
            // Phase 1: safety veto (fear/anger/shame over the ceiling) â†’ do
            // not think with the LLM on this turn. Log it.
            debug!(
                agent = self.config.name,
                "governor: Hesitate decision blocks think() (safety veto)"
            );
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
        } else if self.actions.is_some() {
            // **Actions path (D1 durable tool-loop).** Run the loop journaled
            // per-dispatch over `&mut self.durable`. This handles BOTH a
            // fresh run AND a replay together: the loop checks
            // `is_replaying()` per dispatch, so dispatches already recorded
            // in a replayed turn are returned from the log (the side effect
            // does NOT repeat, the `SubmitOutcome`/ApprovalId/TTL value is
            // identical) and only once the log ends does it continue fresh.
            // The `-think`/`-suspend` step is closed inside the method.
            // Replaces the former "one `-think` step for the whole think"
            // model, which lost the turn's internal progress on a crash
            // between two dispatches (red-team finding).
            let (thought, susp) = match self.think_actions_durable(message, origin, turn).await {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("think_actions_durable failed (non-fatal): {e}");
                    (Some(recovery_fallback_reply_for_error(&e)), None)
                }
            };
            suspend = susp;
            thought.filter(|s| !s.is_empty())
        } else if self.durable.is_replaying() {
            // Replay (non-actions path): return the stored LLM reply from
            // the log without a new call.
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
        } else {
            // Fresh turn (non-actions path): one LLM call, store the result
            // in the step. Origin is passed through so that a possible
            // suspend records the correct conversation origin for resume.
            match self.think_with_origin(message, origin).await {
                Ok(ThinkOutcome::Reply(text)) => self
                    .durable
                    .step(&think_step, {
                        let text = text.clone();
                        move || Ok(text)
                    })
                    .ok()
                    .filter(|s| !s.is_empty()),
                Ok(ThinkOutcome::Suspended {
                    approval_id,
                    redacted_summary,
                }) => {
                    // Suspend is a STATE: the turn was suspended awaiting
                    // approval. It does NOT go into the reply pipeline. Store
                    // a safe summary in the durable step ("{step}-suspend") so
                    // that resume (a later `approve`) and replay can find the
                    // suspension. The stored form is
                    // `"<approval_id>|<redacted_summary>"` â€” NOT the raw
                    // payload, NOT secrets (redacted_summary is already safe
                    // for the operator). No reply text is produced â†’ None.
                    // (This branch is not actually reached in practice, since
                    // suspend requires the actions runtime, but it is kept
                    // for completeness.)
                    let suspend_step = format!("{step_name}-suspend");
                    let payload = format!("{approval_id}|{redacted_summary}");
                    if let Err(e) = self.durable.step(&suspend_step, {
                        let payload = payload.clone();
                        move || Ok(payload)
                    }) {
                        warn!("durable suspend step failed (non-fatal): {e}");
                    }
                    debug!(
                        agent = self.config.name,
                        %approval_id,
                        "turn suspended awaiting approval â€” recorded in durable turn, no user reply"
                    );
                    suspend = Some((approval_id, redacted_summary));
                    None
                }
                Ok(ThinkOutcome::NoReply) => None,
                Err(e) => {
                    warn!("think failed (non-fatal): {e}");
                    Some(recovery_fallback_reply_for_error(&e))
                }
            }
        };

        self.clear_typing_heartbeat();

        // Per-turn provider observability (deployment wishlist item, wired
        // in alongside the failover gap #1 fix): one greppable INFO line per
        // turn recording which provider/model produced the final answer (or
        // "none" if the whole LLM chain failed), how many failovers it
        // took, and the final error class on failure. Only emitted when this
        // turn actually went through the LLM failover layer
        // (`take_last_turn_summary` is `None` otherwise â€” e.g. a
        // brief_ping/governor-filtered turn that never called an LLM).
        if let Some(llm) = self.llm.as_ref() {
            if let Some(summary) = llm.take_last_turn_summary() {
                let model = summary.model.as_deref().unwrap_or("none");
                let error_class = summary.final_error_class.unwrap_or("none");
                info!(
                    "turn-provider: turn={turn} model={model} failovers={} final_error_class={error_class}",
                    summary.failovers
                );
            }
        }

        // 5aÂ½. Short-term memory: append the successful exchange (user â†’ agent)
        //      to this conversation's history, so the NEXT turn sees the
        //      continuity. This is the "reply more than once" fix. Only on a
        //      fresh turn (`!is_replaying()`) â€” replay must not double-record â€”
        //      and only when the turn produced a genuine text reply (suspend/
        //      no-reply does not belong in the conversation history).
        if !self.durable.is_replaying() {
            if let Some(reply) = thought_response.as_ref() {
                let user_text = bus_message_text(message);
                let conv_key = self.conversation_key(origin);
                self.append_history(&conv_key, &user_text, reply);
                if let Some(origin) = origin {
                    if let Err(e) = self
                        .persist_chat_turn(origin, CHAT_USER_TAG, &user_text)
                        .await
                    {
                        warn!("chat user persist failed (non-fatal): {e}");
                    }
                    if let Err(e) = self
                        .persist_chat_turn(origin, CHAT_ASSISTANT_TAG, reply)
                        .await
                    {
                        warn!("chat assistant persist failed (non-fatal): {e}");
                    }
                }
                // Phase 2: fresh turn produced a reply â†’ into the metric (turn counter).
                self.emit_metric(MetricEvent::TurnCompleted);
            }
        }

        // 5b. Reply path (C1 Model A, TASK C2): if `think()` produced text
        //     AND a reply sink + reply target is installed, push the reply
        //     OUT to the channel. This is a DIFFERENT path than bus
        //     publishing â€” the gateway owns the recv end and calls
        //     `Channel::send`. We do NOT publish to the bus (infinite-loop
        //     guard: a bus reply would trigger a new handle_turn). Run ONLY
        //     on a fresh turn, not during replay: sending to the outside
        //     world is a non-idempotency boundary (it would duplicate the
        //     message to the user), so replay must not repeat it.
        //
        //     **Phase 1 governor gatekeeper:** When the governor is installed
        //     AND the decision is `Hesitate`, do NOT reply at all. This is a
        //     critical safety net: a flooded agent (high fear/anger) cannot
        //     send a destructive reply before the situation settles. The
        //     same applies to the `Reflect` state (the agent thinks
        //     internally and does not speak, even if the LLM produced text).
        let reply_decision_blocks = self.governor.as_deref().and_then(|g| {
            let gov = EmotionActionGovernor::new(g);
            match gov.decide(&self.emotion) {
                ActionDecision::Hesitate | ActionDecision::Reflect => {
                    debug!(
                        agent = self.config.name,
                        "governor: Hesitate/Reflect decision blocks reply (silenced)"
                    );
                    self.turn_reply_suppressed.store(true, Ordering::Relaxed);
                    Some(())
                }
                _ => None,
            }
        });
        if !self.durable.is_replaying() && reply_decision_blocks.is_none() {
            let outbound_text: Option<String> = thought_response
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    suspend
                        .as_ref()
                        .map(|(id, summary)| suspended_approval_user_reply(*id, summary))
                });
            if let Some(thought) = outbound_text.as_deref() {
                // F2: derive the reply target per message. Origin FIRST (the
                // conversation the message came from), FALLBACK to the
                // static reply target. This way >1 conversation routes
                // correctly, and the single-channel + static-target MVP
                // behavior is preserved (origin = None).
                let target: Option<&str> = origin
                    .map(familyclaw_bus::MessageOrigin::reply_target)
                    .or(self.reply_target.as_deref());
                if let Some(target) = target {
                    match OutboundMessage::new(target, thought) {
                        Ok(reply) => {
                            if let Err(e) = self.route_reply(reply) {
                                // A routing failure (closed sink) must not
                                // bring down the turn â€” log and continue.
                                warn!("reply routing failed (non-fatal): {e}");
                            }
                        }
                        // Empty target/body is already rejected earlier; just to be safe.
                        Err(e) => warn!("reply build failed (non-fatal): {e}"),
                    }
                }
            }
        }

        // Attach the LLM thought summary OR the suspend marker to the turn
        // summary. Reply and Suspended are mutually exclusive: at most one
        // of them is `Some`. The suspend marker carries only the redacted,
        // operator-safe summary + the approval identifier â€” NOT the raw
        // payload and NOT secrets (resume/audit context).
        let recorded = match (thought_response, suspend) {
            (Some(thought), _) if !thought.is_empty() => {
                let snippet: String = thought.chars().take(160).collect();
                TurnOutcome {
                    summary: format!("{} | thought: {snippet}", recorded.summary),
                    ..recorded
                }
            }
            (_, Some((approval_id, redacted_summary))) => TurnOutcome {
                summary: format!(
                    "{} | suspended(approval={approval_id}): {redacted_summary}",
                    recorded.summary
                ),
                ..recorded
            },
            _ => recorded,
        };

        self.turn_counter += 1;

        // Introspection probe: mirror the final emotion state (contagion +
        // homeostasis already applied) into an optional observation handle,
        // so that an external observer sees the spawned agent's emotion
        // state after contagion over the bus. No-op if no handle is installed.
        if let Some(probe) = self.emotion_probe.as_ref() {
            match probe.lock() {
                Ok(mut guard) => *guard = self.emotion,
                // The lock can only be poisoned if the observer panicked
                // while holding it; mirror anyway rather than propagating
                // the panic into this turn.
                Err(poisoned) => *poisoned.into_inner() = self.emotion,
            }
        }

        Ok(recorded)
    }
}
