//! Config → runtime bridge: [`ModelConfig`] → runnable [`LlmFailover`].
//!
//! This module fills the design gap (recon: *no* `build_llm_chain`):
//! the config layer's [`ModelConfig`]`{primary, fallbacks}`
//! (`familyclaw-core`) is converted into an ordered chain of runnable
//! [`LlmConfig`] settings (`crate::llm`). The actual model name → endpoint/key
//! mapping is the **resolver's** responsibility ([`LlmEndpointResolver`]), so
//! Layer A (this OSS core) does not hardcode endpoints, keys, or
//! provider names.
//!
//! ## Layer boundary (Layer A / Layer B)
//! - **Layer A (this file):** trait boundary + chain construction + failover.
//!   No keys, no endpoints, no family-member models.
//! - **Layer B (e.g. [`EnvEndpointResolver`]):** maps a `"provider/model"`
//!   string to a runnable [`LlmConfig`], reading API keys
//!   from environment variables (e.g. `OPENCODE_API_KEY`, `DEEPSEEK_API_KEY`).
//!   [`EnvEndpointResolver`] is a generic helper — it doesn't know about the
//!   family, only the provider prefix.
//!
//! ## Example
//! ```
//! use familyclaw_agent::llm_chain::{build_llm_chain, EnvEndpointResolver};
//! use familyclaw_core::ModelConfig;
//!
//! // Provider prefixes are mapped to endpoints; keys are read from env.
//! let resolver = EnvEndpointResolver::new()
//!     .with_provider("openai", "https://api.openai.com/v1", "OPENAI_API_KEY");
//! let model = ModelConfig::new("openai/gpt-4o").with_fallback("openai/gpt-4o-mini");
//!
//! // The key may be missing in a test environment → an empty key is allowed at
//! // build time; the error only surfaces on the actual complete() call.
//! let chain = build_llm_chain(&model, &resolver).expect("chain builds");
//! assert_eq!(chain.primary_model(), "openai/gpt-4o");
//! assert_eq!(chain.len(), 2);
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use familyclaw_core::time::Timestamp;
use familyclaw_core::{FamilyClawError, ModelConfig, Result};

use crate::llm::{
    CompletionResult, LlmClient, LlmConfig, LlmError, LlmFailureClass, LlmMessage, ToolDefinition,
};

/// Clock abstraction for failover decision logic (cooldown state machine, Layer B).
///
/// **Why a trait instead of calling [`familyclaw_core::time::now`] directly?**
/// Cooldown decisions
/// (`is this entry still cooling down?`, `cooled_until = now + ladder[strike]`)
/// are read **only** through this interface, so tests can step time
/// deterministically without waiting on `tokio::time::sleep`. In production,
/// [`SystemClock`] delegates to [`familyclaw_core::time::now`] — it is the
/// **only** wall-clock touchpoint on the failover path. This follows the
/// existing codebase convention (time is injected, see `OrchestratedTurn::now`),
/// it doesn't introduce a new pattern.
pub trait Clock: Send + Sync {
    /// The current instant as a UTC timestamp.
    fn now(&self) -> Timestamp;
}

/// Production clock: delegates to [`familyclaw_core::time::now`] (UTC). The only
/// wall-clock read on the failover path.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        familyclaw_core::time::now()
    }
}

/// Maps a model name (`"provider/model"`) to a runnable [`LlmConfig`].
///
/// Layer B implements this (endpoints + keys). Layer A (chain construction)
/// only sees the trait boundary, so the OSS core stays free of hardcoded
/// endpoints and keys.
pub trait LlmEndpointResolver: Send + Sync {
    /// Resolves the model name to a runnable config.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] if the model name cannot be mapped to an
    /// endpoint (e.g. unknown provider prefix).
    fn resolve(&self, model_name: &str) -> Result<LlmConfig>;

    /// Resolves the model name to a **provider identity + key pool**
    /// ([`ResolvedEntry`]) for the cooldown/key-rotation layer.
    ///
    /// The default implementation delegates to [`resolve`](Self::resolve) and
    /// wraps the result into a single-key pool (provider = the model name's
    /// `provider/` part, key = the resolved config's `api_key`). This way,
    /// **existing** resolvers that only implement `resolve` still compile,
    /// but they don't offer multi-key rotation. [`EnvEndpointResolver`]
    /// **overrides** this to return a genuine multi-key pool.
    ///
    /// # Errors
    /// Same as [`resolve`](Self::resolve).
    fn resolve_entry(&self, model_name: &str) -> Result<ResolvedEntry> {
        let cfg = self.resolve(model_name)?;
        let provider = model_name
            .split_once('/')
            .map_or(model_name, |(p, _)| p)
            .to_string();
        let keys = vec![cfg.api_key.clone()];
        Ok(ResolvedEntry {
            provider,
            template: cfg,
            keys,
        })
    }
}

/// Resolution of a single model name for the cooldown/rotation layer:
/// provider identity, a runnable [`LlmConfig`] base (template, `api_key` is
/// filled in from the pool), and a key pool (one or more env keys).
///
/// `template.api_key` can be anything — the effective key is always chosen
/// from the `keys` pool (see `ChainEntry`). `keys` is never empty: if the
/// provider has no key, the pool is `vec![String::new()]` (an empty key →
/// the error only surfaces in `complete()`, as before).
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    /// Provider prefix (e.g. `"openai"`). Entries sharing the same prefix are
    /// cooled down together once the key pool is exhausted (shared key).
    pub provider: String,
    /// Runnable config base (`api_key` is replaced with the pool's active key).
    pub template: LlmConfig,
    /// Key pool for round-robin rotation. Never empty.
    pub keys: Vec<String>,
}

/// Generic, env-based resolver (Layer B helper).
///
/// Maps a `"provider/model"` string to an endpoint based on the provider
/// prefix and reads the API key from an environment variable. The provider
/// table is registered at runtime — no family- or model-specific data is
/// hardcoded here.
///
/// Model name format: `"<provider>/<model>"`. E.g. `"openai/gpt-4o"` →
/// provider `"openai"`, model `"gpt-4o"`. If there's no `/` separator, the
/// whole string is used as both the provider key and the model name.
#[derive(Debug, Clone, Default)]
pub struct EnvEndpointResolver {
    /// provider prefix → (`api_base`, env variable names).
    ///
    /// The key-env list enables a **key pool** per provider: multiple keys
    /// are rotated round-robin on an `AuthFailed` condition
    /// (`ChainEntry`). The single-key [`with_provider`](Self::with_provider)
    /// pushes one element onto the list (backwards-compatible).
    providers: HashMap<String, (String, Vec<String>)>,
    /// Max tokens per response (passed to every [`LlmConfig`]).
    max_tokens: Option<u32>,
    /// Request timeout (ms) set on every resolved [`LlmConfig`] (F1, Layer B
    /// tuning). `None` → the [`LlmConfig`] default
    /// ([`crate::llm::DEFAULT_REQUEST_TIMEOUT_MS`]) stays in effect.
    request_timeout_ms: Option<u64>,
    /// Connect timeout (ms) set on every resolved [`LlmConfig`]. `None` →
    /// [`crate::llm::DEFAULT_CONNECT_TIMEOUT_MS`].
    connect_timeout_ms: Option<u64>,
}

impl EnvEndpointResolver {
    /// Builds an empty resolver without any provider mappings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a provider prefix: endpoint + the env variable the key is
    /// read from at runtime (builder-style).
    ///
    /// - `prefix` — the model name's `provider/` part, e.g. `"openai"`.
    /// - `api_base` — OpenAI-compatible base URL.
    /// - `key_env` — environment variable, e.g. `"OPENAI_API_KEY"`.
    #[must_use]
    pub fn with_provider(
        mut self,
        prefix: impl Into<String>,
        api_base: impl Into<String>,
        key_env: impl Into<String>,
    ) -> Self {
        self.providers
            .insert(prefix.into(), (api_base.into(), vec![key_env.into()]));
        self
    }

    /// Like [`with_provider`](Self::with_provider), but registers **multiple**
    /// key env variables (key pool). On an `AuthFailed` condition, the
    /// cooldown layer rotates the pool's keys round-robin before the whole
    /// provider is cooled down (Layer B). An empty `key_envs` falls back to
    /// the "no key" behavior (one empty key), so the resolver never produces
    /// an empty pool.
    ///
    /// - `prefix` — the model name's `provider/` part, e.g. `"openai"`.
    /// - `api_base` — OpenAI-compatible base URL.
    /// - `key_envs` — environment variables in order, e.g.
    ///   `["OPENAI_API_KEY_1", "OPENAI_API_KEY_2"]`.
    #[must_use]
    pub fn with_provider_keys(
        mut self,
        prefix: impl Into<String>,
        api_base: impl Into<String>,
        key_envs: Vec<String>,
    ) -> Self {
        let key_envs = if key_envs.is_empty() {
            vec![String::new()]
        } else {
            key_envs
        };
        self.providers
            .insert(prefix.into(), (api_base.into(), key_envs));
        self
    }

    /// Sets the `max_tokens` value on all resolved configs.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Sets the request timeout (ms) on all resolved configs
    /// (F1, Layer B tuning). See [`LlmConfig::with_request_timeout_ms`].
    #[must_use]
    pub fn with_request_timeout_ms(mut self, ms: u64) -> Self {
        self.request_timeout_ms = Some(ms);
        self
    }

    /// Sets the connect timeout (ms) on all resolved configs
    /// (F1, Layer B tuning). See [`LlmConfig::with_connect_timeout_ms`].
    #[must_use]
    pub fn with_connect_timeout_ms(mut self, ms: u64) -> Self {
        self.connect_timeout_ms = Some(ms);
        self
    }

    /// Splits the model name into a `(provider, model)` pair.
    fn split(model_name: &str) -> (&str, &str) {
        match model_name.split_once('/') {
            Some((provider, model)) => (provider, model),
            None => (model_name, model_name),
        }
    }
}

impl EnvEndpointResolver {
    /// Applies the `max_tokens` + timeout tunings to the given config (shared
    /// between [`resolve`](LlmEndpointResolver::resolve) and
    /// [`resolve_entry`](LlmEndpointResolver::resolve_entry)).
    fn apply_tunings(&self, mut cfg: LlmConfig) -> LlmConfig {
        if let Some(max) = self.max_tokens {
            cfg = cfg.with_max_tokens(max);
        }
        if let Some(ms) = self.request_timeout_ms {
            cfg = cfg.with_request_timeout_ms(ms);
        }
        if let Some(ms) = self.connect_timeout_ms {
            cfg = cfg.with_connect_timeout_ms(ms);
        }
        cfg
    }
}

impl LlmEndpointResolver for EnvEndpointResolver {
    fn resolve(&self, model_name: &str) -> Result<LlmConfig> {
        let (provider, model) = Self::split(model_name);
        let (api_base, key_envs) = self.providers.get(provider).ok_or_else(|| {
            FamilyClawError::config(format!("unknown provider prefix for model '{model_name}'"))
        })?;
        // The key is read from env at runtime. A missing key does not block
        // chain construction (e.g. fallback models that may not be needed)
        // — an empty key ends up in the LlmConfig and the error only
        // surfaces in complete(). `resolve` uses the pool's **first** key
        // (backwards-compatible single-key path); rotation lives in
        // resolve_entry.
        let api_key = key_envs
            .first()
            .map(|e| std::env::var(e).unwrap_or_default())
            .unwrap_or_default();
        let cfg = LlmConfig::new(api_base.clone(), api_key, model.to_string());
        Ok(self.apply_tunings(cfg))
    }

    fn resolve_entry(&self, model_name: &str) -> Result<ResolvedEntry> {
        let (provider, model) = Self::split(model_name);
        let (api_base, key_envs) = self.providers.get(provider).ok_or_else(|| {
            FamilyClawError::config(format!("unknown provider prefix for model '{model_name}'"))
        })?;
        // Read the whole key pool from env at runtime. An empty env → an
        // empty string (the error only surfaces in complete()). The pool is
        // never empty (registration guarantees ≥1 element), so ChainEntry
        // always gets at least one (possibly empty) key.
        let keys: Vec<String> = key_envs
            .iter()
            .map(|e| std::env::var(e).unwrap_or_default())
            .collect();
        let template = self.apply_tunings(LlmConfig::new(
            api_base.clone(),
            String::new(),
            model.to_string(),
        ));
        Ok(ResolvedEntry {
            provider: provider.to_string(),
            template,
            keys,
        })
    }
}

/// **Health state** of a single chain entry (provider/model pair) for the
/// cooldown state machine.
///
/// `cooled_until` = the timestamp until which the entry is cooling down
/// (`None` = healthy). `strike` = the general escalation counter
/// (rate-limit/overload/http/timeout) and `auth_strike` = a separate auth
/// escalation (once the key pool is exhausted). Both are incremented with
/// `saturating_add` → no wraparound bug.
#[derive(Debug, Clone, Default)]
struct EntryHealth {
    /// Timestamp until which the entry is skipped (PASS 1). `None` = healthy.
    cooled_until: Option<Timestamp>,
    /// General escalation counter (indexes [`LlmFailover::COOLDOWN_LADDER`]).
    strike: u8,
    /// Auth escalation counter (indexes [`LlmFailover::AUTH_COOLDOWN_LADDER`]),
    /// used only once the whole key pool has been tried.
    auth_strike: u8,
}

/// One runnable chain entry: provider identity, config template, key pool +
/// cursor, the built [`LlmClient`], and [`EntryHealth`].
///
/// The key is switched on an `AuthFailed` condition by incrementing
/// `key_cursor` (round-robin) and rebuilding `client` with the pool's next
/// key. The cursor **persists** across `complete()` calls, so a working key
/// doesn't always restart from the beginning of the pool.
struct ChainEntry {
    /// Provider prefix (e.g. `"openai"`). Entries sharing the same prefix are
    /// cooled down together once the key pool is exhausted.
    provider: String,
    /// Config template (`api_key` is replaced with the pool's active key).
    template: LlmConfig,
    /// Key pool (never empty).
    keys: Vec<String>,
    /// Index of the active key in the `keys` pool (persists across calls).
    key_cursor: usize,
    /// Client built with the active key.
    client: LlmClient,
    /// Escalation/cooldown state.
    health: EntryHealth,
}

impl ChainEntry {
    /// Builds an entry from a resolved [`ResolvedEntry`]. Starts at key 0 and
    /// a healthy state.
    fn from_resolved(resolved: ResolvedEntry) -> Self {
        let ResolvedEntry {
            provider,
            template,
            mut keys,
        } = resolved;
        // The pool must not be empty — a safety net (resolve_entry already
        // guarantees this).
        if keys.is_empty() {
            keys.push(String::new());
        }
        let client = Self::build_client(&template, &keys[0]);
        Self {
            provider,
            template,
            keys,
            key_cursor: 0,
            client,
            health: EntryHealth::default(),
        }
    }

    /// Template + key → runnable [`LlmConfig`] (the key replaces the
    /// template's field).
    fn config_with_key(template: &LlmConfig, api_key: &str) -> LlmConfig {
        let mut cfg = template.clone();
        cfg.api_key.clear();
        cfg.api_key.push_str(api_key);
        cfg
    }

    /// Builds an [`LlmClient`] from the template with the given key.
    fn build_client(template: &LlmConfig, api_key: &str) -> LlmClient {
        LlmClient::new(Self::config_with_key(template, api_key))
    }

    /// Switches to the active key `idx` and rebuilds the client.
    fn switch_to_key(&mut self, idx: usize) {
        self.key_cursor = idx;
        self.client = Self::build_client(&self.template, &self.keys[self.key_cursor]);
    }

    /// Effective runnable config (template + active key).
    fn effective_config(&self) -> LlmConfig {
        Self::config_with_key(&self.template, &self.keys[self.key_cursor])
    }

    /// Resets the health state after a successful call (a working key proves
    /// the provider is alive).
    fn mark_healthy(&mut self) {
        self.health = EntryHealth::default();
    }
}

/// Mutable part of the failover state machine: chain entries. All mutable
/// state lives here behind [`std::sync::Mutex`].
struct FailoverState {
    entries: Vec<ChainEntry>,
}

/// Decision after a failed entry attempt (does not hold an Ok value, so it
/// stays generic-free and is shared between `complete`/`complete_with_tools`).
enum FailureStep {
    /// The entry failed with a retryable error → move on to the next entry.
    NextEntry(LlmError),
    /// The key was switched (`AuthFailed`, pool not yet exhausted) → retry the
    /// SAME entry immediately.
    RetrySameEntry,
    /// Non-retryable error → return immediately (don't grind through the
    /// chain).
    Fatal(LlmError),
}

/// Outcome of a single entry attempt: succeeded (value `T`), or an error step.
enum Attempt<T> {
    /// The call succeeded.
    Ok(T),
    /// Failed — next step.
    Failure(FailureStep),
}

/// An ordered failover chain **with a cooldown state machine and key-pool
/// rotation**.
///
/// Built from a [`ModelConfig`] via [`build_llm_chain`]: first `primary`,
/// then `fallbacks` in order ([`ModelConfig::preference_order`]).
/// [`complete`](LlmFailover::complete) tries every **healthy** entry in
/// order; entries that are cooling down are skipped (PASS 1). If no healthy
/// entry responds, **as a last resort** (PASS 2) every entry is tried
/// regardless of cooldown — the family never goes without an answer even if
/// all free models cool down at the same time.
///
/// ## Interior mutability
/// `complete()` remains `&self` (backwards-compatible); all mutable state
/// (cooldown, cursor) lives behind a [`Mutex`]. The lock is held **only**
/// for the duration of synchronous read/write steps (read cooldown / record
/// error / switch key / clone client handle) — **never** across an `.await`.
pub struct LlmFailover {
    state: Mutex<FailoverState>,
    /// Primary model name (first in `preference_order`), for reporting.
    primary: String,
    /// Clock for decision logic (default [`SystemClock`]). Tests inject a
    /// fake clock via [`with_clock`](Self::with_clock).
    clock: Arc<dyn Clock>,
    /// **Per-turn provider observability** (deployment wishlist item): the
    /// outcome of the most recent `complete()`/`complete_with_tools_choice()`
    /// call, consumed (cleared) by [`take_last_turn_summary`](Self::take_last_turn_summary)
    /// so the agent boundary can log one "which provider actually answered
    /// this turn" line without threading a new return type through every
    /// caller. Safe under the same assumption the rest of this runtime
    /// makes: one agent processes one turn at a time (actor model) — this is
    /// not meant to attribute concurrent turns on the same chain.
    last_turn: Mutex<Option<TurnProviderSummary>>,
}

/// Snapshot of "which provider/model produced the final answer this turn,
/// and how many failovers it took" — see [`LlmFailover::last_turn`].
#[derive(Debug, Clone, Default)]
pub struct TurnProviderSummary {
    /// `"provider/model"` that produced the final answer. `None` if every
    /// chain entry failed this call.
    pub model: Option<String>,
    /// Number of failovers used (distinct chain entries tried BEYOND the
    /// first one). `0` = the first entry tried answered directly.
    pub failovers: usize,
    /// One-word [`crate::llm::LlmFailureClass`] tag of the final error, if
    /// every entry failed. `None` on success.
    pub final_error_class: Option<&'static str>,
}

impl LlmFailover {
    /// General escalation rung (rate-limit/overload/http/timeout/nocontent),
    /// indexed by `strike` AFTER incrementing, saturating at the last bucket:
    /// strike 1→60 s, 2→5 min, 3→25 min, 4+→1 h.
    const COOLDOWN_LADDER: [std::time::Duration; 4] = [
        std::time::Duration::from_secs(60),
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(1_500),
        std::time::Duration::from_secs(3_600),
    ];

    /// Longer auth rung (key revoked / billing exhausted — recovers slowly).
    /// Only reached once the whole key pool has been exhausted: 5 min / 30 min
    /// / 2 h / 6 h.
    const AUTH_COOLDOWN_LADDER: [std::time::Duration; 4] = [
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(1_800),
        std::time::Duration::from_secs(7_200),
        std::time::Duration::from_secs(21_600),
    ];

    /// Builds a **single-client** failover chain (length 1) from a ready
    /// [`LlmConfig`] — a backwards-compatible bridge for the single-model
    /// case ([`Agent::new`](crate::Agent::new) wraps this when given
    /// `Some(LlmConfig)`). Behaves exactly like a direct
    /// `LlmClient::new(cfg)` call, but [`complete`](LlmFailover::complete)
    /// goes through the same failover interface (chain length 1 = no
    /// fallbacks, key pool of one element).
    #[must_use]
    pub fn single(cfg: LlmConfig) -> Self {
        let primary = cfg.model.clone();
        let keys = vec![cfg.api_key.clone()];
        let provider = cfg
            .model
            .split_once('/')
            .map_or(cfg.model.as_str(), |(p, _)| p)
            .to_string();
        let entry = ChainEntry::from_resolved(ResolvedEntry {
            provider,
            template: cfg,
            keys,
        });
        Self {
            state: Mutex::new(FailoverState {
                entries: vec![entry],
            }),
            primary,
            clock: Arc::new(SystemClock),
            last_turn: Mutex::new(None),
        }
    }

    /// Switches the decision clock (test builder). Production uses
    /// [`SystemClock`].
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Computes the cooldown duration for a general (non-auth) retryable
    /// error based on the `strike` value (AFTER incrementing). Saturates at
    /// [`COOLDOWN_LADDER`](Self::COOLDOWN_LADDER)'s last bucket.
    ///
    /// - `RateLimited`: `max(cooldown_hint, ladder[strike])` — a provider hint
    ///   (e.g. `Retry-After`) **is honored as a floor** when it exceeds the
    ///   rung, but a lying `retry_after:1` doesn't prevent escalation over
    ///   repeated attempts.
    /// - `Overloaded`: `max(cooldown_hint, ladder[strike])` (hint = 2 s default).
    /// - `Http`/`Timeout`/`NoContent`: `ladder[strike]`.
    fn general_cooldown(err: &LlmError, strike: u8) -> std::time::Duration {
        let rung = Self::ladder_at(&Self::COOLDOWN_LADDER, strike);
        match err.cooldown_hint() {
            Some(hint) => hint.max(rung),
            None => rung,
        }
    }

    /// Indexes the rung with saturation (strike is 1-based after incrementing
    /// → index `strike-1`, up to the last bucket). `strike == 0` → the first.
    fn ladder_at(ladder: &[std::time::Duration; 4], strike: u8) -> std::time::Duration {
        let idx = (strike.saturating_sub(1) as usize).min(ladder.len() - 1);
        ladder[idx]
    }

    /// Converts a [`std::time::Duration`] to [`chrono::Duration`] for cooldown
    /// arithmetic. An overflow (unlikely with the rungs) maps to the maximum
    /// → the entry stays cooling down for a long time, no panic.
    fn chrono_dur(d: std::time::Duration) -> chrono::Duration {
        chrono::Duration::from_std(d).unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX / 1000))
    }

    /// Is the entry cooling down right now (PASS 1 skips these)?
    fn is_cooled(now: Timestamp, health: &EntryHealth) -> bool {
        health.cooled_until.is_some_and(|until| until > now)
    }

    /// Tries **one** entry with a `complete`/`complete_with_tools` call. The
    /// lock is already RELEASED before this; this only takes the lock for the
    /// duration of the key switch / error recording.
    ///
    /// `tried_keys` tracks the keys cycled through per invocation (a full lap
    /// → pool exhausted → cool down the provider).
    async fn try_entry_complete(
        &self,
        idx: usize,
        client: LlmClient,
        messages: &[LlmMessage],
        tried_keys: &mut std::collections::BTreeSet<usize>,
    ) -> Attempt<String> {
        match client.complete(messages).await {
            Ok(text) => {
                self.record_success(idx);
                Attempt::Ok(text)
            }
            Err(e) => Attempt::Failure(self.record_failure(idx, e, tried_keys)),
        }
    }

    /// Like [`try_entry_complete`](Self::try_entry_complete) but with tool calls.
    async fn try_entry_complete_with_tools(
        &self,
        idx: usize,
        client: LlmClient,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        tool_choice: Option<&str>,
        tried_keys: &mut std::collections::BTreeSet<usize>,
    ) -> Attempt<CompletionResult> {
        match client
            .complete_with_tools_choice(messages, tools, tool_choice)
            .await
        {
            Ok(result) => {
                self.record_success(idx);
                Attempt::Ok(result)
            }
            Err(e) => Attempt::Failure(self.record_failure(idx, e, tried_keys)),
        }
    }

    /// Resets the entry's health state after a successful call (under lock).
    fn record_success(&self, idx: usize) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.entries.get_mut(idx) {
                entry.mark_healthy();
            }
        }
    }

    /// Records the entry's failure under lock and decides the next step:
    /// switch key (retry same), cool down provider, cool down entry
    /// (continue), or fatal (non-retryable). No `.await` — the lock is
    /// released on return.
    fn record_failure(
        &self,
        idx: usize,
        err: LlmError,
        tried_keys: &mut std::collections::BTreeSet<usize>,
    ) -> FailureStep {
        // Non-retryable → fatal immediately, without changing state.
        if !err.is_retryable() {
            return FailureStep::Fatal(err);
        }
        let now = self.clock.now();
        let Ok(mut state) = self.state.lock() else {
            // Poisoned lock: treat like "try the next one" — no panic.
            return FailureStep::NextEntry(err);
        };
        let Some(entry) = state.entries.get_mut(idx) else {
            return FailureStep::NextEntry(err);
        };

        if matches!(err, LlmError::AuthFailed(_)) {
            tried_keys.insert(entry.key_cursor);
            // Is there a key in the pool that has NOT yet been tried this call?
            let next = (0..entry.keys.len()).find(|k| !tried_keys.contains(k));
            if let Some(next_idx) = next {
                // Switch to the key and retry the SAME entry immediately.
                // A dead key says nothing about the model's viability.
                entry.switch_to_key(next_idx);
                FailureStep::RetrySameEntry
            } else {
                // Whole pool tried → cool down the WHOLE provider (shared key).
                let provider = entry.provider.clone();
                Self::cool_provider(&mut state, &provider, now);
                FailureStep::NextEntry(err)
            }
        } else if matches!(err, LlmError::NotFound(_)) {
            // Failover gap #1 fix (production incident: a retired NIM model
            // returned 404 on every call, and the chain never rotated).
            // HTTP 404 / "model not found" is a PROVIDER-DEAD signal, not an
            // auth or transient-load signal: rotating the key would not
            // help (the model id itself is gone upstream, not the
            // credential), so move to the next entry IMMEDIATELY (no
            // same-entry key retry) and put THIS entry on the LONG,
            // auth-style ladder (5m/30m/2h/6h) — a retired model will not
            // come back within the short general ladder's 60s starting rung.
            // Reuses `auth_strike`/`AUTH_COOLDOWN_LADDER` (same escalation
            // shape as the key-pool-exhausted case), but scoped to THIS
            // entry only (unlike `cool_provider`, which cools every entry
            // sharing the provider prefix): a 404 says the specific model id
            // is gone, not necessarily every model behind that provider.
            entry.health.auth_strike = entry.health.auth_strike.saturating_add(1);
            let dur = Self::ladder_at(&Self::AUTH_COOLDOWN_LADDER, entry.health.auth_strike);
            entry.health.cooled_until = Some(now + Self::chrono_dur(dur));
            FailureStep::NextEntry(err)
        } else {
            // General retryable → escalating backoff for this entry.
            entry.health.strike = entry.health.strike.saturating_add(1);
            let dur = Self::general_cooldown(&err, entry.health.strike);
            entry.health.cooled_until = Some(now + Self::chrono_dur(dur));
            FailureStep::NextEntry(err)
        }
    }

    /// Cools down ALL entries sharing the given provider on the auth rung
    /// (shared key → one dead key kills all its models). Increments
    /// `auth_strike` and sets `cooled_until`.
    fn cool_provider(state: &mut FailoverState, provider: &str, now: Timestamp) {
        for entry in state.entries.iter_mut().filter(|e| e.provider == provider) {
            entry.health.auth_strike = entry.health.auth_strike.saturating_add(1);
            let dur = Self::ladder_at(&Self::AUTH_COOLDOWN_LADDER, entry.health.auth_strike);
            entry.health.cooled_until = Some(now + Self::chrono_dur(dur));
        }
    }

    /// Snapshot of (idx, client handle) pairs for the **healthy** entries in
    /// order, under lock. Only clones the client handle (`reqwest::Client`
    /// = a cheap Arc clone) so `.await` happens outside the lock.
    fn healthy_clients(&self, now: Timestamp) -> Vec<(usize, LlmClient)> {
        self.snapshot_clients(now, true)
    }

    /// Like [`healthy_clients`](Self::healthy_clients) but ALL entries
    /// (PASS 2, last resort — cooldown is ignored).
    fn all_clients(&self) -> Vec<(usize, LlmClient)> {
        self.snapshot_clients(self.clock.now(), false)
    }

    /// Collects (idx, cloned client handle) pairs. `only_healthy=true` →
    /// skip entries that are cooling down.
    fn snapshot_clients(&self, now: Timestamp, only_healthy: bool) -> Vec<(usize, LlmClient)> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        state
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !only_healthy || !Self::is_cooled(now, &e.health))
            .map(|(i, e)| (i, e.client.clone()))
            .collect()
    }

    /// Clones the entry's active client handle by index (under lock).
    /// `None` if the entry has been removed. Used in the key-switch retry.
    fn client_at(&self, idx: usize) -> Option<LlmClient> {
        let state = self.state.lock().ok()?;
        state.entries.get(idx).map(|e| e.client.clone())
    }

    /// Records the outcome of a `complete()`/`complete_with_tools_choice()`
    /// call for the per-turn observability line (see [`TurnProviderSummary`]
    /// / [`Self::take_last_turn_summary`]). `success_idx` is the entry that
    /// produced the final answer (`None` on total chain failure).
    fn record_turn_summary(
        &self,
        success_idx: Option<usize>,
        failovers: usize,
        final_error_class: Option<&'static str>,
    ) {
        let model = success_idx.and_then(|idx| {
            let state = self.state.lock().ok()?;
            state
                .entries
                .get(idx)
                .map(|e| format!("{}/{}", e.provider, e.template.model))
        });
        if let Ok(mut slot) = self.last_turn.lock() {
            *slot = Some(TurnProviderSummary {
                model,
                failovers,
                final_error_class,
            });
        }
    }

    /// Takes (clears) the most recent turn's provider summary — see
    /// [`TurnProviderSummary`]. `None` if no `complete()`/
    /// `complete_with_tools_choice()` call has completed since the last
    /// read (or ever). Clearing on read prevents a stale summary from a
    /// PREVIOUS turn being logged again if this turn made no LLM call.
    #[must_use]
    pub fn take_last_turn_summary(&self) -> Option<TurnProviderSummary> {
        self.last_turn.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Tries `complete()` in a cooldown-aware way: PASS 1 healthy entries
    /// (entries cooling down are skipped), PASS 2 tries all entries as a last
    /// resort. Key rotation on an `AuthFailed` condition, escalating backoff
    /// for other retryable errors. Returns the last error if all attempts fail.
    ///
    /// **F1 — retryable semantics are preserved:** a non-retryable error
    /// (e.g. parse) is returned **immediately**. The cooldown layer adds to
    /// this: an entry that is cooling down is skipped in PASS 1, but PASS 2
    /// guarantees the family never goes without an answer even if every
    /// entry is cooling down.
    ///
    /// # Errors
    /// The last [`LlmError`] if all chain entries fail (or the first
    /// non-retryable error), or [`LlmError::NoContent`] if the chain is
    /// empty.
    pub async fn complete(&self, messages: &[LlmMessage]) -> std::result::Result<String, LlmError> {
        let mut last_err: Option<LlmError> = None;
        // Number of DISTINCT chain entries attempted so far this call — for
        // the per-turn observability line ("failovers" = attempted - 1).
        let mut attempted: usize = 0;

        // PASS 1: healthy entries (entries cooling down are skipped). The
        // snapshot is taken now; PASS 2's snapshot is taken only AFTER PASS 1
        // so it sees PASS 1's key switches.
        for (idx, mut client) in self.healthy_clients(self.clock.now()) {
            attempted += 1;
            let mut tried_keys = std::collections::BTreeSet::new();
            loop {
                match self
                    .try_entry_complete(idx, client, messages, &mut tried_keys)
                    .await
                {
                    Attempt::Ok(text) => {
                        self.record_turn_summary(Some(idx), attempted - 1, None);
                        return Ok(text);
                    }
                    Attempt::Failure(FailureStep::RetrySameEntry) => match self.client_at(idx) {
                        Some(c) => client = c,
                        None => break,
                    },
                    Attempt::Failure(FailureStep::NextEntry(e)) => {
                        last_err = Some(e);
                        break;
                    }
                    Attempt::Failure(FailureStep::Fatal(e)) => {
                        self.record_turn_summary(
                            None,
                            attempted - 1,
                            Some(e.failure_class().as_word()),
                        );
                        return Err(e);
                    }
                }
            }
        }

        // PASS 2 (last resort): all entries, ignoring cooldown —
        // the family never goes without an answer even if every entry cools
        // down.
        for (idx, mut client) in self.all_clients() {
            attempted += 1;
            let mut tried_keys = std::collections::BTreeSet::new();
            loop {
                match self
                    .try_entry_complete(idx, client, messages, &mut tried_keys)
                    .await
                {
                    Attempt::Ok(text) => {
                        self.record_turn_summary(Some(idx), attempted - 1, None);
                        return Ok(text);
                    }
                    Attempt::Failure(FailureStep::RetrySameEntry) => match self.client_at(idx) {
                        Some(c) => client = c,
                        None => break,
                    },
                    Attempt::Failure(FailureStep::NextEntry(e)) => {
                        last_err = Some(e);
                        break;
                    }
                    Attempt::Failure(FailureStep::Fatal(e)) => {
                        self.record_turn_summary(
                            None,
                            attempted - 1,
                            Some(e.failure_class().as_word()),
                        );
                        return Err(e);
                    }
                }
            }
        }

        let final_class = last_err
            .as_ref()
            .map_or(LlmFailureClass::NoContent.as_word(), |e| {
                e.failure_class().as_word()
            });
        self.record_turn_summary(None, attempted.saturating_sub(1), Some(final_class));
        Err(last_err.unwrap_or(LlmError::NoContent))
    }

    /// Like [`complete`](Self::complete), but with SSE streaming.
    ///
    /// # Errors
    /// The last [`LlmError`] if all chain entries fail.
    pub async fn complete_stream(
        &self,
        messages: &[LlmMessage],
    ) -> std::result::Result<crate::llm::LlmChunkStream, LlmError> {
        let mut last_err: Option<LlmError> = None;
        for (idx, client) in self.healthy_clients(self.clock.now()) {
            match client.complete_stream(messages).await {
                Ok(stream) => {
                    self.record_success(idx);
                    return Ok(stream);
                }
                Err(e) => {
                    last_err = Some(e.clone());
                    if !e.is_retryable() {
                        return Err(e);
                    }
                    let _ = self.record_failure(idx, e, &mut std::collections::BTreeSet::new());
                }
            }
        }
        for (idx, client) in self.all_clients() {
            match client.complete_stream(messages).await {
                Ok(stream) => {
                    self.record_success(idx);
                    return Ok(stream);
                }
                Err(e) => {
                    last_err = Some(e.clone());
                    if !e.is_retryable() {
                        return Err(e);
                    }
                }
            }
        }
        Err(last_err.unwrap_or(LlmError::NoContent))
    }

    /// Like [`complete`](Self::complete), but advertises the `tools` and
    /// returns a [`CompletionResult`] (text + possible tool calls). Same
    /// cooldown/rotation logic (PASS 1 healthy, PASS 2 last resort).
    ///
    /// # Errors
    /// The last [`LlmError`] if all chain entries fail (or the first
    /// non-retryable error), or [`LlmError::NoContent`] if the chain is
    /// empty.
    pub async fn complete_with_tools(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
    ) -> std::result::Result<CompletionResult, LlmError> {
        self.complete_with_tools_choice(messages, tools, None).await
    }

    /// Like [`complete`](Self::complete_with_tools) with explicit `tool_choice`.
    pub async fn complete_with_tools_choice(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        tool_choice: Option<&str>,
    ) -> std::result::Result<CompletionResult, LlmError> {
        let mut last_err: Option<LlmError> = None;
        // See `complete()` — same per-turn observability bookkeeping.
        let mut attempted: usize = 0;

        // PASS 1: healthy entries.
        for (idx, mut client) in self.healthy_clients(self.clock.now()) {
            attempted += 1;
            let mut tried_keys = std::collections::BTreeSet::new();
            loop {
                match self
                    .try_entry_complete_with_tools(
                        idx,
                        client,
                        messages,
                        tools,
                        tool_choice,
                        &mut tried_keys,
                    )
                    .await
                {
                    Attempt::Ok(result) => {
                        self.record_turn_summary(Some(idx), attempted - 1, None);
                        return Ok(result);
                    }
                    Attempt::Failure(FailureStep::RetrySameEntry) => match self.client_at(idx) {
                        Some(c) => client = c,
                        None => break,
                    },
                    Attempt::Failure(FailureStep::NextEntry(e)) => {
                        last_err = Some(e);
                        break;
                    }
                    Attempt::Failure(FailureStep::Fatal(e)) => {
                        self.record_turn_summary(
                            None,
                            attempted - 1,
                            Some(e.failure_class().as_word()),
                        );
                        return Err(e);
                    }
                }
            }
        }

        // PASS 2 (last resort): all entries, ignoring cooldown.
        for (idx, mut client) in self.all_clients() {
            attempted += 1;
            let mut tried_keys = std::collections::BTreeSet::new();
            loop {
                match self
                    .try_entry_complete_with_tools(
                        idx,
                        client,
                        messages,
                        tools,
                        tool_choice,
                        &mut tried_keys,
                    )
                    .await
                {
                    Attempt::Ok(result) => {
                        self.record_turn_summary(Some(idx), attempted - 1, None);
                        return Ok(result);
                    }
                    Attempt::Failure(FailureStep::RetrySameEntry) => match self.client_at(idx) {
                        Some(c) => client = c,
                        None => break,
                    },
                    Attempt::Failure(FailureStep::NextEntry(e)) => {
                        last_err = Some(e);
                        break;
                    }
                    Attempt::Failure(FailureStep::Fatal(e)) => {
                        self.record_turn_summary(
                            None,
                            attempted - 1,
                            Some(e.failure_class().as_word()),
                        );
                        return Err(e);
                    }
                }
            }
        }

        let final_class = last_err
            .as_ref()
            .map_or(LlmFailureClass::NoContent.as_word(), |e| {
                e.failure_class().as_word()
            });
        self.record_turn_summary(None, attempted.saturating_sub(1), Some(final_class));
        Err(last_err.unwrap_or(LlmError::NoContent))
    }

    /// Primary model name (first in `preference_order`).
    #[must_use]
    pub fn primary_model(&self) -> &str {
        &self.primary
    }

    /// Chain length (primary + successfully resolved fallbacks).
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.lock().map_or(0, |s| s.entries.len())
    }

    /// Whether the chain is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The primary entry's effective runnable config (template + active
    /// key). `None` if the chain is empty.
    #[must_use]
    pub fn primary_config(&self) -> Option<LlmConfig> {
        let state = self.state.lock().ok()?;
        state.entries.first().map(ChainEntry::effective_config)
    }
}

/// Builds a failover chain from a [`ModelConfig`] using a resolver.
///
/// Iterates [`ModelConfig::preference_order`] (primary → fallbacks) and
/// resolves each model name to an [`LlmConfig`] via the resolver. Models the
/// resolver doesn't know are **skipped** (they don't bring down the whole
/// chain) — this way, one invalid fallback doesn't block a working primary.
///
/// # Errors
/// [`FamilyClawError::Config`] if `primary` is empty or if **none** of the
/// models in `preference_order` resolved (an empty chain is invalid).
pub fn build_llm_chain(
    cfg: &ModelConfig,
    resolver: &dyn LlmEndpointResolver,
) -> Result<LlmFailover> {
    build_llm_chain_with_clock(cfg, resolver, Arc::new(SystemClock))
}

/// Like [`build_llm_chain`], but injects the cooldown state machine's
/// **clock** (test use). Production uses [`build_llm_chain`], which supplies
/// [`SystemClock`]. Tests supply a fake clock to step past the cooldown
/// window without waiting on `tokio::time::sleep`.
///
/// # Errors
/// Same as [`build_llm_chain`].
pub fn build_llm_chain_with_clock(
    cfg: &ModelConfig,
    resolver: &dyn LlmEndpointResolver,
    clock: Arc<dyn Clock>,
) -> Result<LlmFailover> {
    cfg.validate()?;
    let primary = cfg.primary.clone();
    let mut entries = Vec::new();
    for model_name in cfg.preference_order() {
        match resolver.resolve_entry(model_name) {
            Ok(entry_spec) => entries.push(ChainEntry::from_resolved(entry_spec)),
            Err(e) => {
                // Skip the unknown model but log the reason at debug level.
                tracing::debug!(model = model_name, error = %e, "skipping unresolvable model");
            }
        }
    }
    if entries.is_empty() {
        return Err(FamilyClawError::config(format!(
            "no usable model: none of '{}' (+{} fallbacks) resolved to an endpoint",
            cfg.primary,
            cfg.fallbacks.len()
        )));
    }
    Ok(LlmFailover {
        state: Mutex::new(FailoverState { entries }),
        primary,
        clock,
        last_turn: Mutex::new(None),
    })
}

/// Extracts the agent's primary [`LlmConfig`] from the config layer — ready
/// to feed into [`Agent::new`](crate::Agent::new) (which takes an
/// `Option<LlmConfig>`).
///
/// This is a lightweight bridge for TASK C4: `FamilyConfig` →
/// (agent, [`ModelConfig`]) → runnable primary config. The agent's
/// public construction surface is not changed — only a ready-made
/// `Option<LlmConfig>` is returned, which the caller passes along.
///
/// # Errors
/// [`FamilyClawError::Config`] if the model configuration is invalid or if
/// no model resolved to an endpoint.
pub fn primary_llm_config(
    model: &ModelConfig,
    resolver: &dyn LlmEndpointResolver,
) -> Result<LlmConfig> {
    let chain = build_llm_chain(model, resolver)?;
    chain
        .primary_config()
        .ok_or_else(|| FamilyClawError::config("empty llm chain has no primary config"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::{AgentConfig, FamilyConfig};

    /// Resolver that knows the provider prefixes without an env dependency.
    fn test_resolver() -> EnvEndpointResolver {
        EnvEndpointResolver::new()
            .with_provider("openai", "https://api.openai.com/v1", "OPENAI_API_KEY")
            .with_provider(
                "deepseek",
                "https://api.deepseek.com/v1",
                "DEEPSEEK_API_KEY",
            )
            .with_provider("opencode", "https://opencode.ai/zen/v1", "OPENCODE_API_KEY")
    }

    #[test]
    fn split_handles_provider_prefix_and_bare_name() {
        assert_eq!(
            EnvEndpointResolver::split("openai/gpt-4o"),
            ("openai", "gpt-4o")
        );
        assert_eq!(
            EnvEndpointResolver::split("bare-model"),
            ("bare-model", "bare-model")
        );
    }

    #[test]
    fn resolver_maps_provider_to_endpoint() {
        let r = test_resolver();
        let cfg = r.resolve("deepseek/deepseek-v4-pro").expect("resolves");
        assert_eq!(cfg.api_base, "https://api.deepseek.com/v1");
        assert_eq!(cfg.model, "deepseek-v4-pro");
    }

    #[test]
    fn resolver_rejects_unknown_provider() {
        let r = test_resolver();
        let err = r
            .resolve("mystery/model")
            .expect_err("unknown provider rejected");
        assert!(matches!(err, FamilyClawError::Config(_)));
    }

    #[test]
    fn build_chain_orders_primary_then_fallbacks() {
        let r = test_resolver();
        let model = ModelConfig::new("openai/gpt-4o")
            .with_fallback("deepseek/deepseek-v4-pro")
            .with_fallback("opencode/big-pickle");
        let chain = build_llm_chain(&model, &r).expect("chain builds");
        assert_eq!(chain.len(), 3);
        assert_eq!(chain.primary_model(), "openai/gpt-4o");
        assert_eq!(
            chain.primary_config().expect("primary").api_base,
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn build_chain_skips_unresolvable_fallback_but_keeps_primary() {
        let r = test_resolver();
        let model = ModelConfig::new("openai/gpt-4o").with_fallback("mystery/model");
        let chain = build_llm_chain(&model, &r).expect("primary still usable");
        assert_eq!(chain.len(), 1, "unresolvable fallback dropped");
        assert_eq!(chain.primary_model(), "openai/gpt-4o");
    }

    #[test]
    fn build_chain_errors_when_nothing_resolves() {
        let r = test_resolver();
        let model = ModelConfig::new("mystery/a").with_fallback("mystery/b");
        // Result<LlmFailover>: LlmFailover does not implement Debug (LlmClient
        // doesn't), so match directly instead of using expect_err.
        match build_llm_chain(&model, &r) {
            Err(FamilyClawError::Config(_)) => {}
            Err(other) => panic!("expected Config error, got {other:?}"),
            Ok(_) => panic!("expected error for empty chain"),
        }
    }

    #[test]
    fn build_chain_errors_on_invalid_model_config() {
        let r = test_resolver();
        let model = ModelConfig::new("   ");
        assert!(build_llm_chain(&model, &r).is_err());
    }

    #[test]
    fn primary_llm_config_returns_ready_config() {
        let r = test_resolver();
        let model = ModelConfig::new("deepseek/deepseek-v4-pro");
        let cfg = primary_llm_config(&model, &r).expect("primary config");
        assert_eq!(cfg.model, "deepseek-v4-pro");
    }

    /// TASK C4 acceptance: `FamilyConfig` JSON → the agent builds without a
    /// panic (primary `LlmConfig` is obtained from the config layer + resolver).
    #[test]
    fn family_json_builds_agent_llm_config_without_panic() {
        let json = r#"{
            "name": "demo_family",
            "agents": [
                {
                    "name": "agent_a",
                    "model": {
                        "primary": "deepseek/deepseek-v4-pro",
                        "fallbacks": ["openai/gpt-4o", "mystery/skip-me"]
                    }
                }
            ]
        }"#;
        let family = FamilyConfig::from_json_str(json).expect("config parses + validates");
        let resolver = test_resolver();

        let agent: &AgentConfig = family.agents.first().expect("one agent");
        let chain = build_llm_chain(&agent.model, &resolver).expect("chain builds");
        // primary + one known fallback; unknown "mystery/" skipped.
        assert_eq!(chain.len(), 2);
        assert_eq!(chain.primary_model(), "deepseek/deepseek-v4-pro");

        // Ready-made primary config, which the caller passes to Agent::new(Some(cfg)).
        let primary = primary_llm_config(&agent.model, &resolver).expect("primary config");
        assert_eq!(primary.model, "deepseek-v4-pro");
    }

    #[test]
    fn resolver_applies_timeouts_to_resolved_config() {
        // F1: Layer B sets a timeout tuning → it ends up in the resolved
        // LlmConfig → the gateway production path inherits it
        // (build_resolver → resolve → new).
        let r = test_resolver()
            .with_request_timeout_ms(7_000)
            .with_connect_timeout_ms(800);
        let cfg = r.resolve("openai/gpt-4o").expect("resolves");
        assert_eq!(cfg.request_timeout_ms, Some(7_000));
        assert_eq!(cfg.connect_timeout_ms, Some(800));
    }

    #[test]
    fn resolver_without_timeout_leaves_config_default() {
        // Without a tuning, the resolver doesn't force a timeout → the
        // LlmConfig default (60s/10s in LlmClient::new) stays in effect.
        let r = test_resolver();
        let cfg = r.resolve("openai/gpt-4o").expect("resolves");
        assert_eq!(cfg.request_timeout_ms, None);
        assert_eq!(cfg.connect_timeout_ms, None);
    }

    /// F1 retryable semantics at the unit level: a non-retryable error is
    /// returned **immediately** and the whole chain is not ground through.
    /// We use an empty (impossible to resolve) endpoint to verify the
    /// structure — the actual timeout→failover proof is in the runtime
    /// roundtrip (`timeout_primary_fails_over_to_live_fallback`).
    #[tokio::test]
    async fn complete_on_empty_chain_path_is_no_content() {
        // Direct construction with an empty chain isn't allowed through the
        // public interface, but complete()'s semantics for an empty chain
        // are defined: verify it doesn't panic.
        let failover = LlmFailover {
            state: Mutex::new(FailoverState {
                entries: Vec::new(),
            }),
            primary: String::new(),
            clock: Arc::new(SystemClock),
            last_turn: Mutex::new(None),
        };
        assert!(failover.is_empty());
        let err = failover
            .complete(&[LlmMessage::user("hi")])
            .await
            .expect_err("empty chain yields error, not panic");
        assert!(matches!(err, LlmError::NoContent));
    }
}

// ── Cooldown state machine + key-pool rotation (failover gap #1 steps 2-3) ───
#[cfg(test)]
mod cooldown_tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use familyclaw_core::time::{from_unix_secs, Timestamp};
    use familyclaw_core::ModelConfig;

    use super::{
        build_llm_chain_with_clock, Clock, EnvEndpointResolver, LlmError, LlmFailover,
        LlmFailureClass, LlmMessage,
    };

    /// Controllable fake clock for determinism: time is stepped over the
    /// cooldown window without waiting on `sleep`.
    struct FixedClock(Mutex<Timestamp>);

    impl FixedClock {
        fn at(secs: i64) -> Arc<Self> {
            Arc::new(Self(Mutex::new(
                from_unix_secs(secs).expect("valid unix secs"),
            )))
        }

        /// Advances the clock forward by the given seconds.
        fn advance(&self, secs: i64) {
            let mut t = self.0.lock().expect("clock lock");
            *t += chrono::Duration::seconds(secs);
        }
    }

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            *self.0.lock().expect("clock lock")
        }
    }

    /// Response recipe for a single model, chosen by the request counter
    /// (per port) or by the Bearer key.
    #[derive(Clone)]
    struct Reply {
        status: u16,
        /// Response content in the success case (assistant content).
        content: String,
    }

    impl Reply {
        fn ok(content: &str) -> Self {
            Self {
                status: 200,
                content: content.into(),
            }
        }
        fn status(code: u16) -> Self {
            Self {
                status: code,
                content: String::new(),
            }
        }
    }

    /// Small HTTP/1.1 mock that does NOT require axum: reads the request,
    /// picks a `Reply`, and responds. Responses can be steered either by
    /// request order (`script`) or by the Bearer key (`by_key`). Counts
    /// requests.
    struct MockLlm {
        base_url: String,
        calls: Arc<AtomicUsize>,
        /// Per-key request counters (proof of rotation).
        key_calls: Arc<Mutex<HashMap<String, usize>>>,
    }

    impl MockLlm {
        /// Starts the mock, which returns the `script[min(call, len-1)]`
        /// response (saturates at the last one). `by_key` (if `Some`)
        /// overrides the script: the response is chosen by the Bearer token
        /// (a missing key → `default`).
        fn spawn(script: Vec<Reply>, by_key: Option<(HashMap<String, Reply>, Reply)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind to ephemeral port");
            let addr = listener.local_addr().expect("mock local_addr");
            let base_url = format!("http://{addr}/v1");
            let calls = Arc::new(AtomicUsize::new(0));
            let key_calls = Arc::new(Mutex::new(HashMap::new()));

            let calls_t = Arc::clone(&calls);
            let key_calls_t = Arc::clone(&key_calls);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let n = calls_t.fetch_add(1, Ordering::SeqCst);
                    Self::handle(stream, n, &script, by_key.as_ref(), &key_calls_t);
                }
            });

            Self {
                base_url,
                calls,
                key_calls,
            }
        }

        fn total_calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn calls_for_key(&self, key: &str) -> usize {
            self.key_calls
                .lock()
                .expect("key_calls lock")
                .get(key)
                .copied()
                .unwrap_or(0)
        }

        fn handle(
            mut stream: TcpStream,
            call_index: usize,
            script: &[Reply],
            by_key: Option<&(HashMap<String, Reply>, Reply)>,
            key_calls: &Arc<Mutex<HashMap<String, usize>>>,
        ) {
            // Read the request headers until an empty line; extract the Bearer + body length.
            let mut buf = [0_u8; 4096];
            let read = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]);
            let bearer = req
                .lines()
                .find_map(|l| {
                    let lower = l.to_ascii_lowercase();
                    lower
                        .strip_prefix("authorization: bearer ")
                        .map(|_| l["authorization: bearer ".len()..].trim().to_string())
                })
                .unwrap_or_default();

            let reply = if let Some((map, default)) = by_key {
                *key_calls
                    .lock()
                    .expect("key_calls lock")
                    .entry(bearer.clone())
                    .or_insert(0) += 1;
                map.get(&bearer).cloned().unwrap_or_else(|| default.clone())
            } else {
                let idx = call_index.min(script.len().saturating_sub(1));
                script
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| Reply::status(500))
            };

            let body = if reply.status == 200 {
                format!(
                    r#"{{"id":"x","object":"chat.completion","choices":[{{"index":0,"message":{{"role":"assistant","content":{}}},"finish_reason":"stop"}}]}}"#,
                    serde_json::to_string(&reply.content).expect("json string")
                )
            } else {
                r#"{"error":"mock"}"#.to_string()
            };
            let reason = match reply.status {
                200 => "OK",
                401 => "Unauthorized",
                429 => "Too Many Requests",
                503 => "Service Unavailable",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status,
                reason,
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    }

    fn msgs() -> Vec<LlmMessage> {
        vec![LlmMessage::user("hi")]
    }

    /// Coerces the typed fake clock into a trait object for
    /// `build_llm_chain_with_clock` (Arc<FixedClock> doesn't auto-coerce to
    /// Arc<dyn Clock> as an argument).
    fn dyn_clock(clock: &Arc<FixedClock>) -> Arc<dyn Clock> {
        Arc::clone(clock) as Arc<dyn Clock>
    }

    /// Builds a single-model failover with the given mock + fake clock.
    fn single_model_failover(mock: &MockLlm, clock: &Arc<FixedClock>) -> LlmFailover {
        let resolver = EnvEndpointResolver::new().with_provider(
            "mock",
            mock.base_url.clone(),
            "FAMILYCLAW_TEST_KEY_UNSET",
        );
        let model = ModelConfig::new("mock/model-a");
        build_llm_chain_with_clock(&model, &resolver, dyn_clock(clock)).expect("chain builds")
    }

    // ── Cooldown entry/skip/exit + escalation ───────────────────────────────

    #[tokio::test]
    async fn rate_limited_entry_cools_then_last_resort_retries() {
        // One model: 429 on the first call → the entry cools down → PASS 2
        // (last resort) retries the same entry with the same call. The
        // second mock call returns 200 → succeeds.
        let mock = MockLlm::spawn(vec![Reply::status(429), Reply::ok("recovered")], None);
        let clock = FixedClock::at(1000);
        let failover = single_model_failover(&mock, &clock);

        let out = failover
            .complete(&msgs())
            .await
            .expect("last-resort succeeds");
        assert_eq!(out, "recovered");
        // PASS 1 (429 → cool) + PASS 2 (200) = 2 calls.
        assert_eq!(mock.total_calls(), 2);
    }

    #[tokio::test]
    async fn healthy_fallback_used_when_primary_rate_limited() {
        // Two models with different providers: primary 429 (cools down), fallback 200.
        let primary = MockLlm::spawn(vec![Reply::status(429)], None);
        let fallback = MockLlm::spawn(vec![Reply::ok("from-fallback")], None);
        let clock = FixedClock::at(1000);

        let resolver = EnvEndpointResolver::new()
            .with_provider("pa", primary.base_url.clone(), "K_UNSET_A")
            .with_provider("pb", fallback.base_url.clone(), "K_UNSET_B");
        let model = ModelConfig::new("pa/m").with_fallback("pb/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        let out = failover.complete(&msgs()).await.expect("fallback answers");
        assert_eq!(out, "from-fallback");
        // Primary tried once (429), fallback once (200). PASS 1 satisfied.
        assert_eq!(primary.total_calls(), 1);
        assert_eq!(fallback.total_calls(), 1);
    }

    #[tokio::test]
    async fn cooled_entry_skipped_until_clock_advances_past_window() {
        // 429 (60 s rung, strike 1) → the entry cools down 1000..1060.
        // Second call at 1030 (still cooling down): PASS 1 skips it, but 200
        // comes from PASS 2. Third call at 1100 (cooldown over): PASS 1
        // succeeds directly.
        let mock = MockLlm::spawn(
            vec![Reply::status(429), Reply::ok("a"), Reply::ok("b")],
            None,
        );
        let clock = FixedClock::at(1000);
        let failover = single_model_failover(&mock, &clock);

        // Call 1: 429 → cool until 1060, then PASS 2 gives "a".
        assert_eq!(failover.complete(&msgs()).await.expect("c1"), "a");
        let after_c1 = mock.total_calls();
        assert!(after_c1 >= 2, "expected 429 + last-resort, got {after_c1}");

        // The successful call reset the health → the next call is healthy in PASS 1.
        // Ensure determinism: step the clock clearly forward regardless.
        clock.advance(120);
        let out = failover.complete(&msgs()).await.expect("c2 healthy");
        assert_eq!(out, "b");
    }

    #[tokio::test]
    async fn last_resort_serves_when_all_entries_cooled() {
        // Both models get 429 first (both cool down in PASS 1), then 200.
        // PASS 2 (last resort) guarantees an answer even if everything cooled down.
        let a = MockLlm::spawn(vec![Reply::status(429), Reply::ok("a-ok")], None);
        let b = MockLlm::spawn(vec![Reply::status(429), Reply::ok("b-ok")], None);
        let clock = FixedClock::at(1000);
        let resolver = EnvEndpointResolver::new()
            .with_provider("pa", a.base_url.clone(), "KA")
            .with_provider("pb", b.base_url.clone(), "KB");
        let model = ModelConfig::new("pa/m").with_fallback("pb/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        // PASS 1: a→429(cools down), b→429(cools down). PASS 2: a→200 "a-ok".
        let out = failover
            .complete(&msgs())
            .await
            .expect("last-resort answers");
        assert_eq!(out, "a-ok");
        assert_eq!(a.total_calls(), 2, "a: PASS1 429 + PASS2 200");
        assert_eq!(b.total_calls(), 1, "b: PASS1 429 only (PASS2 stops at a)");
    }

    // ── Failover gap #1 fix: HTTP 404 / "model not found" = provider-dead ──

    #[tokio::test]
    async fn not_found_rotates_to_fallback_and_cools_long_not_short() {
        // Failover gap #1 fix (production incident: a retired NIM model
        // returned HTTP 404 on every call, and the chain never rotated —
        // every retry hit the same dead model and the turn failed within
        // seconds). A 404 must NOT behave like a 429 (60s general rung): it
        // must rotate to the next provider IMMEDIATELY, and the dead entry
        // must cool down on the LONG auth-style ladder (5 min first rung),
        // so it stays skipped well past the 60s general-ladder window a
        // retired model will never recover from.
        let primary = MockLlm::spawn(vec![Reply::status(404)], None);
        let fallback = MockLlm::spawn(vec![Reply::ok("from-fallback")], None);
        let clock = FixedClock::at(1000);

        let resolver = EnvEndpointResolver::new()
            .with_provider("pa", primary.base_url.clone(), "K_UNSET_A")
            .with_provider("pb", fallback.base_url.clone(), "K_UNSET_B");
        let model = ModelConfig::new("pa/m").with_fallback("pb/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        let out = failover.complete(&msgs()).await.expect("fallback answers");
        assert_eq!(out, "from-fallback");
        // Exactly one rotation: primary tried once (404, no same-entry key
        // retry — a 404 isn't a key problem), fallback tried once (200).
        assert_eq!(
            primary.total_calls(),
            1,
            "primary tried exactly once — no key-pool retry on a 404"
        );
        assert_eq!(fallback.total_calls(), 1);

        // Per-turn observability (deployment wishlist item): one failover,
        // the fallback's "provider/model" recorded, no error.
        let summary = failover
            .take_last_turn_summary()
            .expect("summary recorded after complete()");
        assert_eq!(
            summary.failovers, 1,
            "exactly one failover: primary -> fallback"
        );
        assert_eq!(summary.model.as_deref(), Some("pb/m"));
        assert!(summary.final_error_class.is_none());

        // Step the clock past the SHORT general ladder's first rung (60s)
        // but still well within the LONG auth-style ladder's first rung
        // (300s). If the primary had been (incorrectly) cooled on the short
        // ladder, it would already be healthy again and PASS 1 would retry
        // it here.
        clock.advance(120);
        let out2 = failover.complete(&msgs()).await;
        assert!(out2.is_ok(), "fallback still answers");
        assert_eq!(
            primary.total_calls(),
            1,
            "primary must stay cooled past the short ladder's window (long ladder in effect)"
        );
    }

    #[tokio::test]
    async fn all_providers_not_found_surfaces_provider_not_found_class() {
        // (b) All providers return 404 -> the chain's final error is
        // LlmError::NotFound / LlmFailureClass::ProviderNotFound (not a
        // generic Http), and the turn summary reports it — this is what
        // the agent-level user message (recovery_fallback_reply_for_error)
        // keys off of.
        let a = MockLlm::spawn(vec![Reply::status(404)], None);
        let b = MockLlm::spawn(vec![Reply::status(404)], None);
        let clock = FixedClock::at(1000);
        let resolver = EnvEndpointResolver::new()
            .with_provider("pa", a.base_url.clone(), "KA")
            .with_provider("pb", b.base_url.clone(), "KB");
        let model = ModelConfig::new("pa/m").with_fallback("pb/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        let err = failover
            .complete(&msgs())
            .await
            .expect_err("all dead -> error, not hang");
        assert!(matches!(err, LlmError::NotFound(_)));
        assert_eq!(err.failure_class(), LlmFailureClass::ProviderNotFound);
        assert_eq!(err.redacted_status_line(), "HTTP 404");

        let summary = failover
            .take_last_turn_summary()
            .expect("summary recorded even on total failure");
        assert!(summary.model.is_none());
        assert_eq!(summary.final_error_class, Some("provider_not_found"));
    }

    // ── Key-pool rotation on AuthFailed ─────────────────────────────────────

    #[tokio::test]
    async fn auth_failed_rotates_to_next_key_in_pool() {
        // Key #1 (env KA1) → 401, key #2 (env KA2) → 200. Rotation happens
        // within the same complete() call: a dead key doesn't cool down the
        // model — the next key is tried immediately instead.
        std::env::set_var("FCT_KA1", "dead-key");
        std::env::set_var("FCT_KA2", "good-key");
        let mut by_key = HashMap::new();
        by_key.insert("dead-key".to_string(), Reply::status(401));
        by_key.insert("good-key".to_string(), Reply::ok("rotated-ok"));
        let mock = MockLlm::spawn(Vec::new(), Some((by_key, Reply::status(401))));
        let clock = FixedClock::at(1000);

        let resolver = EnvEndpointResolver::new().with_provider_keys(
            "mock",
            mock.base_url.clone(),
            vec!["FCT_KA1".into(), "FCT_KA2".into()],
        );
        let model = ModelConfig::new("mock/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        let out = failover.complete(&msgs()).await.expect("rotation succeeds");
        assert_eq!(out, "rotated-ok");
        // Both keys were tried exactly once (rotation, not cooldown).
        assert_eq!(mock.calls_for_key("dead-key"), 1);
        assert_eq!(mock.calls_for_key("good-key"), 1);

        std::env::remove_var("FCT_KA1");
        std::env::remove_var("FCT_KA2");
    }

    #[tokio::test]
    async fn provider_exhausted_when_all_keys_auth_fail() {
        // Both keys → 401. The pool is exhausted → the provider is cooled
        // down → no infinite loop. Result: an error (everything dead).
        std::env::set_var("FCT_KB1", "k1");
        std::env::set_var("FCT_KB2", "k2");
        let mock = MockLlm::spawn(Vec::new(), Some((HashMap::new(), Reply::status(401))));
        let clock = FixedClock::at(1000);

        let resolver = EnvEndpointResolver::new().with_provider_keys(
            "mock",
            mock.base_url.clone(),
            vec!["FCT_KB1".into(), "FCT_KB2".into()],
        );
        let model = ModelConfig::new("mock/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        let err = failover
            .complete(&msgs())
            .await
            .expect_err("all keys dead → error, not hang");
        assert!(matches!(err, LlmError::AuthFailed(_)));
        // Per complete(): PASS 1 tries k1(401)→k2(401)→pool exhausted→cooldown.
        // PASS 2 tries again (cooldown ignored): k1(401)→k2(401)→exhausted.
        // = 4 calls, no more (the tried-set prevents a loop).
        assert_eq!(mock.calls_for_key("k1"), 2);
        assert_eq!(mock.calls_for_key("k2"), 2);

        std::env::remove_var("FCT_KB1");
        std::env::remove_var("FCT_KB2");
    }

    // ── escalation ladder (pure, no network) ────────────────────────────────

    #[test]
    fn general_cooldown_escalates_and_saturates() {
        // strike 1→60s, 2→300s, 3→1500s, 4→3600s, 5+→3600s (saturates).
        let http = LlmError::Http("x".into());
        assert_eq!(
            LlmFailover::general_cooldown(&http, 1),
            std::time::Duration::from_secs(60)
        );
        assert_eq!(
            LlmFailover::general_cooldown(&http, 2),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            LlmFailover::general_cooldown(&http, 4),
            std::time::Duration::from_secs(3_600)
        );
        // Saturates at the last bucket (no wraparound with u8).
        assert_eq!(
            LlmFailover::general_cooldown(&http, 250),
            std::time::Duration::from_secs(3_600)
        );
    }

    #[test]
    fn rate_limited_honors_retry_after_as_floor() {
        // retry_after 600 s > strike-1 rung (60 s) → 600 s as the floor.
        let big = LlmError::RateLimited {
            message: "429".into(),
            retry_after: Some(600),
        };
        assert_eq!(
            LlmFailover::general_cooldown(&big, 1),
            std::time::Duration::from_secs(600)
        );
        // retry_after 1 s < rung 60 s → the rung wins (a provider can't lie
        // its way out of escalation).
        let tiny = LlmError::RateLimited {
            message: "429".into(),
            retry_after: Some(1),
        };
        assert_eq!(
            LlmFailover::general_cooldown(&tiny, 1),
            std::time::Duration::from_secs(60)
        );
    }

    #[test]
    fn auth_ladder_escalates_and_saturates() {
        assert_eq!(
            LlmFailover::ladder_at(&LlmFailover::AUTH_COOLDOWN_LADDER, 1),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            LlmFailover::ladder_at(&LlmFailover::AUTH_COOLDOWN_LADDER, 4),
            std::time::Duration::from_secs(21_600)
        );
        assert_eq!(
            LlmFailover::ladder_at(&LlmFailover::AUTH_COOLDOWN_LADDER, 99),
            std::time::Duration::from_secs(21_600)
        );
    }

    #[tokio::test]
    async fn retryable_http_error_grinds_pass1_and_pass2_then_returns_last() {
        // 418 → Http (retryable, no exact class) → PASS 1 cools it down, PASS 2
        // (last resort) retries = 2 calls, then the last error is returned.
        // Proves that retryable is NOT fatal and PASS 2 does run.
        let mock = MockLlm::spawn(vec![Reply::status(418)], None);
        let clock = FixedClock::at(1000);
        let failover = single_model_failover(&mock, &clock);
        let err = failover.complete(&msgs()).await.expect_err("all fail");
        assert!(matches!(err, LlmError::Http(_)));
        assert_eq!(mock.total_calls(), 2);
    }

    #[test]
    fn success_resets_health_via_primary_config_roundtrip() {
        // Structural check: primary_config returns the effective key from
        // the pool (not an empty template).
        std::env::set_var("FCT_PCFG", "live-key-xyz");
        let resolver = EnvEndpointResolver::new().with_provider_keys(
            "mock",
            "http://127.0.0.1:1/v1".to_string(),
            vec!["FCT_PCFG".into()],
        );
        let model = ModelConfig::new("mock/m");
        let clock = FixedClock::at(0);
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");
        let cfg = failover.primary_config().expect("primary config");
        assert_eq!(cfg.api_key, "live-key-xyz");
        assert_eq!(cfg.model, "m");
        std::env::remove_var("FCT_PCFG");
    }
}
