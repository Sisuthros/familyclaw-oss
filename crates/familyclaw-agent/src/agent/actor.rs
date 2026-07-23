//! Ractor actor shell wrapping [`super::Agent`] for bus message delivery.

use super::helpers::{send_watchdog_notice, watchdog_two_stage};
use super::prelude::*;
use super::Agent;

/// Type-erased agent for actor (no generics).
type ErasedAgent = Agent;

/// [`Agent`]'s Ractor actor shell.
///
/// The state is [`Agent`] itself. The message type is [`ResonanceMessage`]
/// (the bus's language), so the actor connects to the bus through the same
/// interface as any being.
///
/// The actor is stateless (all state lives in the [`Agent`] value).
pub struct AgentActor {
    _marker: std::marker::PhantomData<fn() -> ErasedAgent>,
}

impl AgentActor {
    /// Builds a new (stateless) actor shell.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl Actor for AgentActor {
    type Msg = ResonanceMessage;
    type State = ErasedAgent;
    type Arguments = ErasedAgent;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        agent: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        debug!(agent = agent.name(), being = %agent.being_id(), "agentti kÃ¤ynnistyy");
        Ok(agent)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        envelope: Self::Msg,
        agent: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        let sender = envelope.from;
        // Do not process our own echoes (the bus does not send them, but
        // just to be safe â€” hearing yourself is not a turn).
        if sender == agent.being_id {
            return Ok(());
        }

        // TASK 1: RESUME CONTROL SIGNAL before normal turn routing.
        // `ResumeApproval` is NOT a conversation but a control signal: it is
        // routed directly to the resume path (`handle_resume_signal` â†’
        // `resume_approved` + `route_reply`) and does NOT start a new LLM
        // turn (`handle_turn_with_origin`). The self-echo guard above
        // applies here too.
        if let BusMessage::ResumeApproval { approval_id } = &envelope.payload {
            if let Err(err) = agent.handle_resume_signal(approval_id, time::now()).await {
                // `handle_resume_signal` already handles errors fail-closed
                // (always returns Ok); this branch is defense in depth in
                // case the contract ever changes. One signal's error does
                // not bring down the being.
                warn!(agent = agent.name(), error = %err, "resume-signaalin kÃ¤sittely epÃ¤onnistui");
            }
            return Ok(());
        }

        // F2: per-message origin from the envelope â†’ the reply target is
        // derived per message (origin.reply_target()), fallback to the
        // static target.
        let origin = envelope.origin.clone();
        let payload = envelope.payload.clone();
        let watchdog_secs = watchdog::turn_watchdog_secs();
        let hard_secs = watchdog::turn_watchdog_hard_secs(watchdog_secs);

        // Precompute the reply route now: once `turn_future` below captures
        // `agent`'s exclusive borrow for the turn's lifetime (via
        // `handle_turn_with_origin(&mut self, ...)`), `agent` cannot be
        // touched again â€” not even for a shared read â€” until that future is
        // dropped. An interim notice sent *while* the turn is still running
        // therefore has to use values resolved up front, not `agent` itself.
        let notice_sink = agent.reply_sink.clone();
        let notice_target = agent.reply_target_for_origin(origin.as_ref());
        let agent_label = agent.name().to_string();

        let turn_future =
            Box::pin(agent.handle_turn_with_origin(sender, &payload, origin.as_ref()));
        // Soft deadline (`watchdog_secs`): send an interim "still working"
        // notice but keep awaiting the same future. Hard cap (`hard_secs`):
        // give up for good â€” this is the only point work is now discarded,
        // vs. the old behavior of dropping at the soft deadline every time.
        let turn_result = watchdog_two_stage(turn_future, watchdog_secs, hard_secs, || {
            warn!(
                agent = agent_label.as_str(),
                soft_secs = watchdog_secs,
                hard_secs,
                "turn-watchdog: soft deadline reached, turn still running â€” sending interim notice"
            );
            send_watchdog_notice(
                notice_sink.as_ref(),
                notice_target.as_deref(),
                &watchdog::watchdog_still_working_msg(hard_secs),
            );
        })
        .await;

        match turn_result {
            Ok(Ok(outcome)) => {
                debug!(
                    agent = agent.name(),
                    turn = outcome.turn,
                    remembered = outcome.remembered,
                    "vuoro kÃ¤sitelty"
                );
                if let Err(err) = agent.enforce_watchdog_after_turn(&payload, origin.as_ref()) {
                    warn!(agent = agent.name(), error = %err, "turn-watchdog silence fallback failed");
                }
            }
            Ok(Err(err)) => {
                warn!(agent = agent.name(), error = %err, "vuoron kÃ¤sittely epÃ¤onnistui");
                if let Err(e) =
                    agent.force_watchdog_reply(origin.as_ref(), watchdog::WATCHDOG_ERROR_MSG)
                {
                    warn!(agent = agent.name(), error = %e, "turn-watchdog error reply failed");
                }
            }
            Err(()) => {
                agent.clear_typing_heartbeat();
                warn!(
                    agent = agent.name(),
                    soft_secs = watchdog_secs,
                    hard_secs,
                    "turn-watchdog: vuoro ylitti kovan aikarajan"
                );
                if let Err(e) =
                    agent.force_watchdog_reply(origin.as_ref(), watchdog::WATCHDOG_TIMEOUT_MSG)
                {
                    warn!(agent = agent.name(), error = %e, "turn-watchdog timeout reply failed");
                }
            }
        }
        Ok(())
    }
}
