# FamilyClaw v1.0 — Top-Tier Agent Platform Roadmap

> **Disposition: REVISE → folded into spec below → APPROVED as master roadmap (2026-06-16).**
> Produced by a structured architecture-review panel (Designer → Skeptic → Constraint
> Guardian → User Advocate → Arbiter) on **2026-06-16**. 23 objections, 5 blockers, all ruled.
> Every load-bearing fact was verified against the live tree (file:line) before locking.

> **APPROVED with four amendments (independent senior review, 2026-06-16):** this is the
> new master roadmap. Build Phase 0 first; no big-bang Phase 1; no TODO-only commits; no new
> architecture without tests. The four amendments below are canon and are folded into §3/§6:
> 1. **Phase 1 is split internally into checkpoints 1A–1D** (same branch, but distinct commits +
>    milestones — avoid a 4–6 week "cathedral in fog"): **1A** LLM tool schema +
>    `complete_with_tools` failover; **1B** agent tool loop + safe `fs-read` allowlisted flagship
>    skill; **1C** persisted approval + suspend/resume; **1D** gateway approval surface +
>    crash-replay red-team proof.
> 2. **Default flagship skill = `fs-read` with a path allowlist** (NOT http-get). http-get is
>    deferred to ≥1D/P2 and only as require-approval + strict egress allowlist + redirect checks +
>    RFC1918/link-local denial + tainted output. (http-get-first = SSRF/DNS/redirect demon kangaroo.)
> 3. **`SUSPENDED_MARKER` is temporary only.** Move to a first-class `ThinkOutcome`/`TurnOutcome`
>    enum (`Reply(String) | Suspended { approval_id, redacted_summary } | NoReply`) ASAP — suspend
>    is a *state*, not a string, and must not flow through the normal-reply pipe.
> 4. **A later Growth-Loop phase is added (Phase 4.5, after scheduler, before live multi-agent):**
>    proof bundle → safe memory → pattern proposal → eval proposal → **approval-gated** skill/policy
>    update. No silent self-modification, no silent permission expansion. (See §3 + §6.5.)

---

## 1. Understanding (locked + confirmed)

**What:** Turn FamilyClaw (20-crate Rust workspace) into a production-grade, top-tier agent
platform (v1.0). Unified roadmap; implemented phase-by-phase, each phase independently
shippable, compiling, and green.

**North star:** primarily **(2)** an OSS / sellable general-purpose agent platform AND **(3)**
a practical worker that safely does revenue-generating work ("agents that can safely DO WORK");
secondarily **(1)** the home of an AI family who extend it themselves. General-purpose,
adoptable, safe real work — family autonomy is a consequence, not the optimization target.

**Hard constraints (non-negotiable):**
- **Köyhyys (poverty):** only free / already-paid resources. No per-token paid APIs in
  defaults/crons. Embeddings must be **local** (no embedding API).
- **Layer A / Layer B wall:** OSS crates contain no private souls, family names, keys, webhook
  URLs, private paths, SOUL.md, calibration data. Enforced by `scripts/audit-layer-b.sh` + CI.
- **Per-phase green:** each phase independently passes — on MSVC stable (MSRV 1.85) — `cargo
  build`, `cargo test`, `cargo clippy --all-targets -- -D warnings` (pedantic; every public item
  documented), `cargo doc -D warnings`, `scripts/audit-layer-b.sh`, `cargo deny`. No `unsafe`
  (forbidden). No `unwrap`/`expect`/`panic!` on production paths. No TODO-only commits.
- **Per-phase branch:** `feat/familyclaw-vN-<slug>` off `main`; merged only when all gates green.

**Explicit non-goals:** revive SurrealDB (dead feature, solves wrong problem); integrate the
standalone `eternal-thread` crate (broken, redundant — cherry-pick ideas only); embedding APIs;
WhatsApp/Signal channels; k8s/IaC templates; simple-cron parsing in v1.0.

---

## 2. Verified current state (evidence-based, not marketing)

**Production-grade already:** durable crash-replay (`familyclaw-durable`, journal-based,
red-team tested); LLM provider failover (`llm.rs`/`llm_chain.rs` — real reqwest, retryability
classification, timeout tuning); wasmtime sandbox (`familyclaw-sandbox`, fuel + capability
deny-by-default, feature-gated); memory decay (`familyclaw-memory`, Ebbinghaus VAD, concurrent-
safe, 89 tests); agent perceive→think→act loop (`agent.rs:479-526`, real LLM call + RAG recall
+ emotion governor); `familyclaw-actions` (skill registry, 7-risk approval gate with one-shot +
payload-bound + fail-closed-expiry tokens, redacting proof bundles, audit log, MCP boundary,
4 mock skills, CLI facade, 161 tests).

**The 5 gaps (corrected against live tree during review):**
1. **[RED] Tool-calling loop missing.** `agent.think()` calls `llm.complete()` once and returns
   text (`agent.rs:522`). `complete_with_tools`/`CompletionResult`/`LlmMessage::tool_result`
   ALREADY EXIST (`llm.rs:323/387/176`), but `ChatCompletionsRequest` has NO `tools` field
   (`llm.rs:479-483`) — the model can only volunteer tool_calls, the agent never sends tools,
   parses calls, dispatches, or loops. The just-built `familyclaw-actions` + sandbox are
   UNREACHABLE from the agent. Everything depends on this.
2. **[ORANGE] Live multi-agent offline.** Orchestrator + contract-net run only on the abstract
   bridge layer with `MockTurnExecutor`; live `serve` mode runs one agent on the Resonance Bus.
3. **[ORANGE] Memory text-only.** `familyclaw-memory` has `embedding: Option<Vec<f32>>` +
   `cosine_similarity` (`retrieval.rs:497`) + `semantic_weight`, but NO embedding provider (mock
   only); retrieval is keyword+emotion+recency. The 89 tests default `semantic_weight=0.0`.
4. **[YELLOW] No general scheduler.** `DreamCycle` IS spawned (`runtime/lib.rs:296`, hand-rolled
   `tokio::sleep` loop), but there is no generic scheduler; agent is otherwise reactive.
5. **[YELLOW] Observability narrowed.** `tracing_subscriber` IS wired (`main.rs:630`, EnvFilter,
   text formatter `.with_target(false)`). Gap is span propagation inbound→turn→reply + turn-level
   audit, NOT subscriber bootstrap.

---

## 3. Final roadmap (Arbiter-revised order)

**Order: 0 → 1 → 2 → 3 → 4 → 5.** Tool-loop first (keystone), multi-agent last (biggest/riskiest,
zero marginal revenue before one agent can use one tool). Sequencing upheld by panel; Phase 0
inserted to de-risk infrastructure blockers before they poison a phase mid-flight.

| Phase | Gap | Deliverable (one-line) | Effort |
|---|---|---|---|
| **0 (NEW)** | infra | MSVC clippy+doc CI job (per-phase feature flags) + pure-Rust-vs-`ort` embedder build/deny **spike** (picks Phase-3 default). No prod code except CI job. | S |
| **1 (keystone, EXPANDED)** | #1 | Replay-correct bounded tool loop + **persisted** approval + cross-process **operator approval surface** + minimal turn-audit sink + one **safe** flagship skill (`fs-read`, path allowlist) + SPI rename + manifest JSON schema + QUICKSTART/example. **Split into checkpoints 1A→1D** (see §6). | L+ (4-6 wk realistic) |
| &nbsp;&nbsp;↳ **1A** | #1 | LLM tool schema (`ChatCompletionsRequest.tools`) + `complete_with_tools` failover. Compiles + tests. | S |
| &nbsp;&nbsp;↳ **1B** | #1 | Agent tool loop + safe `fs-read` allowlisted flagship skill, end-to-end (auto-run). | M |
| &nbsp;&nbsp;↳ **1C** | #1 | Persisted approval store + suspend/resume; survives restart. First-class `ThinkOutcome::Suspended`. | M |
| &nbsp;&nbsp;↳ **1D** | #1 | Gateway approval surface (HTTP/chat) + crash-replay red-team proof + turn-audit sink. | M |
| **2** | #5 | Full turn-level tracing spans (reuse wired subscriber) + Prometheus turn/tool metrics; promote turn-audit to full AuditCollector. Text default, JSON opt-in. | S-M (1 wk) |
| **3** | #3 | `familyclaw-embeddings` crate: `EmbeddingProvider` trait + deterministic zero-dep default + feature-gated **pure-Rust** local provider (Phase-0-selected); auto-embed-on-write; recall benchmark gate; status/doctor shows active provider | M (1.5-2 wk) |
| **4** | #4 | Minimal `familyclaw-scheduler` (interval-only; cron deferred) + DreamCycle refactored as first task + family-agency controls (opt-in/visible/kill-switch/expire-on-no-human) | M (1-1.5 wk) |
| **4.5 (NEW)** | growth | **Growth loop:** proof bundle → safe memory → pattern proposal → eval proposal → **approval-gated** skill/policy update. No silent self-modification, no silent permission expansion. Reuses Phase-1 approval gate + Phase-3 memory + Phase-2 audit. After scheduler, before multi-agent. | M (1.5-2 wk) |
| **5 (LAST)** | #2 | `LiveBusTurnExecutor` adapts tool-capable bus Agent to bridge `TurnExecutor`; Orchestrator coordinates ≥2 live agents in serve mode. Must specify per-node journal ownership + suspended-node Deliverable semantics. | L (2-3 wk) |

---

## 4. Decision Log (9 decisions)

### D1 — Tool-calling loop: bounded, cooperatively-suspendable multi-turn loop
- **Alternatives:** keep one-shot + offline agentic subcommand (rejected: serve-mode agent stays
  toolless = platform can't DO WORK in its main mode); new tool protocol (rejected: primitives
  already exist `llm.rs:323/137/176`); inline-block awaiting approval (rejected: hangs the ractor
  actor — cooperative suspend is correct); unbounded loop (rejected: violates no-panic + cost).
- **Objections (Skeptic+Guardian blocker):** durable step is clock-free/side-effect-free by design
  (`agent.rs:582`); routing clock-dependent, side-effecting tool dispatch through it regresses
  determinism unless `now` is journaled; in-memory pending map (`facade.rs:145`) loses approvals
  on the exact crash the durable layer exists to survive.
- **Resolution: ACCEPTED.** Phase 1 mandates journaled-clock determinism + persisted approval
  state + a red-team replay test asserting BOTH no-double-side-effect AND value-identical
  `SubmitOutcome`.

### D2 — Operator approval surface: persisted, cross-process store + query/approve surface
- **Alternatives:** rely on `familyclaw-actions-cli` (rejected: `build_runtime()` builds a FRESH
  empty runtime per invocation → cross-process approve returns `ApprovalMissing` = non-functional);
  in-memory + accept loss on crash (rejected: contradicts durable guarantee); defer UX (rejected:
  keystone would suspend for an approval no human can grant).
- **Objections (User Advocate blocker + Guardian major):** only surface rebuilds empty runtime;
  gateway has NO `familyclaw-actions` dependency, no approval route; pending unbounded, no eviction.
- **Resolution: ACCEPTED.** One persisted store satisfies both crash-recovery and operator-surface;
  folded into Phase 1 (the suspend path is meaningless without it). Capacity cap + TTL eviction +
  per-being dangerous-tool rate limit.

### D3 — Live multi-agent: `LiveBusTurnExecutor` adapter, done LAST
- **Alternatives:** keep orchestrate permanently offline (rejected: leaves ORANGE gap); new
  live-only orchestrator (rejected: duplicates proven DAG/contract-net, loses Mock determinism
  guard); replace bus single-agent model (rejected: huge blast radius).
- **Objections (Skeptic+Guardian major):** "just an adapter" understates the impedance mismatch —
  `TurnExecutor::execute` is `&self`/stateless/hermetic (`executor.rs:25-26,130`) vs the Phase-1
  agent turn `&mut self`, journaled, suspend-returning (`agent.rs:572`); reopens cross-agent replay.
- **Resolution: ACCEPTED (scope/risk, not order).** Phase 5 depends on the corrected Phase-1 durable
  contract; must specify per-node journal ownership + suspended-node `Deliverable` semantics. Order
  (last) upheld.

### D4 — Embeddings: `familyclaw-embeddings` crate, **pure-Rust default**, `ort` opt-in after spike
- **Alternatives:** `ort` as default (rejected: zero onnx presence; `deny.toml` lacks onnxruntime
  license; wasmtime precedent is pure-Rust-from-source, NOT a native C++ link; per-phase MSVC+deny
  gate fails as written); adopt eternal-thread (rejected: broken, 12 vs 89 tests); revive SurrealDB
  (rejected: dead feature, RocksDB/Docker vs köyhyys); hosted API (rejected: violates köyhyys+local).
- **Objections (Guardian blocker, Skeptic major, UA minor):** ort-as-default unverified on MSVC,
  trips deny; recall improvement asserted without benchmark; default-mock + naive `semantic_weight>0`
  silently does cosine-over-noise.
- **Resolution: PARTIALLY ACCEPTED.** ort → opt-in-after-Phase-0-spike. Recall benchmark = phase
  gate (semantic must beat keyword on a fixture before `semantic_weight>0` is honored live).
  status/doctor must surface active provider; default deterministic provider refuses/warns on
  `semantic_weight>0`. Crate-vs-inline kept as designed.

### D5 — Scheduling: minimal interval-only `familyclaw-scheduler`; cron deferred
- **Alternatives:** reuse durable substrate for timers (rejected: conflates replay-journal with
  timer); heavy cron crate (rejected: köyhyys); two bespoke loops (rejected: duplication).
- **Objections (Skeptic+Guardian minor, UA minor):** crate + cron parser for N=1 consumer is
  gold-plating; proactive tasks act "as" a family member without consent/visibility + flood the
  approval queue on a timer.
- **Resolution: PARTIALLY ACCEPTED.** Cron parser DEFERRED (gold-plating). Crate kept (hosts
  idempotent proactive tool tasks). Family-agency controls (opt-in/visible/kill-switch/
  expire-on-no-human) into Phase 4.

### D6 — Observability: span tree + turn audit; minimal sink folded into Phase 1
- **Alternatives:** wire a new subscriber (rejected: already present `main.rs:630`); Datadog/OTLP
  default (rejected: köyhyys — off-by-default feature seam only); keep all observability after
  Phase 1 (rejected: design's own logic says a tool loop is untrustworthy without turn audit).
- **Objections (UA major, Skeptic minor, Guardian minor):** folding observability AFTER Phase 1
  means the riskiest keystone merges under-observed and is debugged blind during its own
  crash-replay red-teaming — self-inconsistent. "Structured JSON" inaccurate (text formatter).
- **Resolution: PARTIALLY ACCEPTED.** Minimal turn-audit sink MOVED INTO Phase 1. Full span tree +
  metrics stay Phase 2. Wording corrected: text default, JSON opt-in.

### D7 — Flagship skill + SPI clarity (Phase 1)
- **Alternatives:** keep `MockSkill` name (rejected: signals "not for real use" to adopters); ship
  http-get as ReadOnly auto-run (rejected: verified SSRF auto-run — `policy.rs:194`); defer docs
  (rejected: "tools for free" claim unreproducible without reverse-engineering 5 modules).
- **Objections (UA major×2, Guardian major, Skeptic minor):** public SPI literally `pub trait
  MockSkill` (`skills/mod.rs:73`); manifest free-text hints only (`manifest.rs:50`); zero
  docs/examples; flagship http-get is an unapproved SSRF; `audit-layer-b.sh` can't catch binary
  models or env-interpolated private endpoints.
- **Resolution: ACCEPTED.** Rename SPI off `Mock` (deprecated alias one release); manifest gains a
  real JSON schema (or canonize `McpToolDescriptor.input_schema`); ship QUICKSTART + example;
  flagship skill = fs-read (path allowlist) OR http-get classed require-approval with egress
  allowlist (deny `169.254.169.254`, RFC1918, non-http); add CI binary-commit guard.

### D8 — CI gate reconciliation (cross-cutting blocker → Phase 0)
- **Objections (Guardian blocker, Skeptic major):** the per-phase gate (MSVC AND clippy-pedantic-D
  AND doc-D AND deny AND audit) is run by NO single machine — clippy/doc/deny/all-features are
  ubuntu-only (`ci.yml:46/52/108/181`); windows job is build+test `--features discord` only
  (`110-128`); workspace lints are `warn`, not `deny`.
- **Resolution: ACCEPTED as Phase-0 prerequisite.** Add a windows-latest clippy + doc job, run with
  each phase's actual feature flags, before any "green on MSVC" claim.

### D9 — Sequencing: tool-loop-first, multi-agent-last (UPHELD)
- **Alternatives:** strict 1→2→3→4→5 (rejected: spends largest/riskiest effort before single-agent
  real work); embeddings/multi-agent first (rejected: nothing can call a tool yet); big-bang
  (rejected: violates per-phase-shippable).
- **Resolution: UPHELD.** Order sound; Phase 0 inserted to de-risk infra blockers.

---

## 5. Phase 0 — prerequisite spec

**Branch:** `feat/familyclaw-v0-ci-spike`. **Deliverable:** no production code except a CI job.
1. **MSVC CI job** (windows-latest): `cargo clippy --workspace --all-targets -- -D warnings` and
   `cargo doc --no-deps -D warnings`, parameterized to run with each phase's feature flags (not
   just `--features discord`). This makes the "green on MSVC" conjunction actually enforced by one
   machine.
2. **Embedder spike** (throwaway): attempt to build a pure-Rust embedder (`fastembed`/`candle`, no
   native C++ link) AND `ort`/onnxruntime on MSVC stable; run `cargo deny` against each; record
   which passes deny + MSVC. Result PICKS the Phase-3 default (pure-Rust expected; `ort` relegated
   to opt-in only if it passes deny with its license added to `deny.toml`). Document in a decision
   note. **No production dependency lands.**

---

## 6. Phase 1 — implementation-ready spec (the panel's consensus)

**Branch:** `feat/familyclaw-v1-tool-loop` off `main`. **Prerequisite:** Phase 0 MSVC CI job merged.
**Goal:** `agent.think()` can call a tool, feed the result back, loop to a stop, and (for dangerous
tools) suspend for a human approval that works cross-process and survives a crash — all
replay-deterministic.

### Files touched
1. **`familyclaw-agent/src/llm.rs`** — `ChatCompletionsRequest` (`479-483`): add
   `tools: Option<&[ToolDefinition]>` + `tool_choice: Option<&'static str>`
   (`skip_serializing_if = Option::is_none` → existing requests serialize identically). New
   `pub struct ToolDefinition { name, description, input_schema: Value }` (documented), serialized
   to OpenAI tools-array shape. Extend `complete_with_tools` (`323`) builder to pass the tools slice.
2. **`familyclaw-agent/src/llm_chain.rs`** — `LlmFailover` (`169`): add
   `pub async fn complete_with_tools(&self, messages, tools) -> Result<CompletionResult, LlmError>`
   walking the same ordered failover chain + retryability classification. Documented.
3. **`familyclaw-actions/src/manifest.rs`** — `SkillManifest` (`33`): add
   `input_schema: serde_json::Value` (real JSON Schema). Keep free-text hints for human display.
   Add `to_tool_definition(&self)` (or canonize `McpToolDescriptor.input_schema` at `mcp.rs:55`
   as the single source, manifest delegates).
4. **`familyclaw-actions/src/skills/mod.rs`** — rename `pub trait MockSkill` (`73`) → `pub trait
   Skill`; keep `pub use Skill as MockSkill;` deprecated alias one release. All public items doc'd.
5. **`familyclaw-actions/src/facade.rs`** — `submit_task` (`242`) / `approve` (`296`) already take
   `now: Timestamp` (good). REPLACE in-memory `pending: HashMap<ApprovalId, PendingEntry>` (`145`)
   with a `PendingApprovalStore` trait (in-memory impl for tests; durable-journal-backed impl for
   production) so a crash between suspend and approve is recoverable. Add capacity cap + TTL
   eviction (reuse fail-closed-expiry to GC resumable turn state) + per-being dangerous-tool rate
   limit. Doc all new pub items.
6. **`familyclaw-agent/src/agent.rs`** ← **PRIMARY INTEGRATION POINT.** `Agent` (`102`): add
   `actions: Option<Arc<ActionRuntime>>` + `tool_loop: ToolLoopConfig` fields; builder
   `with_actions(self, rt) -> Self` (additive — `None` preserves current one-shot). New
   `pub struct ToolLoopConfig { max_iterations: u32 }` (default 8, documented). Keep the public
   surface TINY to limit pedantic doc burden.
7. **`familyclaw-durable/src/context.rs`** — no API change; `step()` used as-is.
8. **`familyclaw-runtime/src/lib.rs`** — `build_family` wires `Arc<ActionRuntime>` into the spawned
   Agent via `with_actions`.
9. **`familyclaw-gateway/src/main.rs` + `Cargo.toml`** — add `familyclaw-actions` dependency
   (currently absent); add HTTP routes `GET /approvals/pending` (redacted proof bundles for
   suspended turns) and `POST /approvals/{id}/approve`; chat affordance when a turn suspends. Add a
   minimal turn-audit sink (reuse `familyclaw-actions` `AuditCollector`) recording turn start, each
   tool dispatch + redacted result, suspend/resume, stop_reason — operator-retrievable.

### Precise integration in `agent.think()`
Today (`479-526`): build `recall_ctx` (limit 5) → recall → `system_prompt = soul.essence +
memories` → `messages = [system, user]` → `Some(llm.complete(&messages).await...)` (`522`).
**Suspend is a first-class state, NOT a string (amendment 3).** `think()` returns
`Result<ThinkOutcome>` where:
```
pub enum ThinkOutcome {            // documented; suspend must never flow through the reply pipe
    Reply(String),
    Suspended { approval_id: ApprovalId, redacted_summary: String },
    NoReply,
}
```
(`SUSPENDED_MARKER` may be a throwaway intermediate in the very first 1B spike, but `ThinkOutcome`
lands by 1C — suspend is a state.)

REPLACE step 3 with: if `self.actions` is `None` → keep one-shot `llm.complete` →
`Ok(ThinkOutcome::Reply(text))` (backward compat). If `Some(rt)`:
```
let tools = rt.tool_definitions();              // derived from registry McpToolDescriptor.input_schema
let mut messages = messages;
for _iter in 0..self.tool_loop.max_iterations {
    let result = llm.complete_with_tools(&messages, &tools).await.map_err(FamilyClawError::llm)?;
    if !result.has_tool_calls() { return Ok(ThinkOutcome::Reply(result.content)); }   // stop_reason=stop
    messages.push(assistant_msg_with_tool_calls(result.tool_calls));
    for call in result.tool_calls {
        let skill_id = map_name_to_skill(call.name)
            // UnknownSkill -> push tool_result(call.id, "error: unknown tool") and CONTINUE
            //                 (counts against bound; no abort, no infinite retry)
        let outcome: SubmitOutcome = self.durable.step(&format!("tool-{}", call.id), {
            let rt = Arc::clone(rt); let args = call.arguments.clone();
            move || {
                let now = Timestamp::now();             // generated INSIDE -> journaled -> value-identical on replay
                rt.submit_task_blocking_or_record(skill_id, args, now)
            }
        })?;
        if let Some(approval_id) = outcome.pending_approval {
            persist_resumable_turn_state(approval_id, &ResumableTurn { /* see below */ }); // durable store
            return Ok(ThinkOutcome::Suspended {                 // cooperative suspend: RETURN, never block
                approval_id, redacted_summary: outcome.redacted_proof_summary,
            });
        }
        messages.push(LlmMessage::tool_result(call.id, outcome.result_text));
    }
}
return Ok(ThinkOutcome::Reply(last_content_or_max_iter_notice));   // bound hit, never panic
```
**Resumable turn state (amendment — define precisely; persisted to the durable store, NEVER raw
secrets, NEVER Layer-B data):** `approval_id`, `being_id`, `conversation_origin`, the `messages`
stack so far, `tool_call_id`, `tool_name`, `arguments_hash`, `redacted_arguments`, `created_at`,
`expires_at`, `policy_snapshot`, `audit_ids`, `durable_cursor`/`turn_id`.

**Resume path:** new inbound `approve` event (HTTP route or chat) → load `ResumableTurn` by
`ApprovalId` → `rt.approve(id, Timestamp::now())` → continue the loop from persisted `messages`.

### Approval-gate wiring
- Dangerous tool (`policy.rs` `required_approval`: SendMessage/SpendMoney/Irreversible/ExecuteCode,
  OR a network-read with LLM-controlled target classified to require approval) → `submit_task`
  returns `pending_approval=Some(id)` → SUSPEND (return), never block the actor.
- Safe tool (ReadOnly/WriteLocal, allowlisted target) → AutoRun → result fed back inline.
- Pending state persisted (durable), capacity-capped, TTL-evicted, per-being rate-limited.
- Operator sees pending via `GET /approvals/pending` (redacted proof), approves via POST or chat.

### Loop bounds / safety
- `max_iterations` (default 8) hard bound → returns a notice, never panics.
- All fallible steps route through `ActionError`/`FamilyClawError` — no unwrap/expect/panic on
  production paths (test modules may use `expect`).
- Unknown-tool / bad-args → error `tool_result` (counts against bound), never abort or infinite-retry.

### Flagship real skill (amendment 2: `fs-read` is the DEFAULT, not http-get)
**`fs_read_allowlisted`** — the v1.0 flagship. It proves the loop without opening a network door:
- `risk = read_only`
- approval = NOT required only when the path is under the configured allowlist (outside → require
  approval or reject)
- output = **tainted** unless the file is a trusted project file
- proof = **path hash + size + summary**, NOT full contents by default
- schema/name generic (no family names / private paths) to pass `audit-layer-b.sh`

**`http-get` is DEFERRED to ≥1D / Phase 2** and only ships as: require-approval + strict egress
allowlist (deny link-local `169.254.169.254`, all RFC1918, IPv6 ULA/link-local), redirect-follow
checks (re-validate each hop), non-http(s) scheme denial, DNS-rebinding guard — all enforced as a
skill **precondition** (not just JSON-schema validation), with tainted output. http-get-first is an
SSRF/DNS/redirect footgun; fs-read-first proves the loop safely.

### Green gates (all must pass on the branch; MSVC clippy+doc via Phase-0 job)
- `cargo build --workspace` (default + new features) on ubuntu AND windows-latest.
- `cargo test --workspace` incl. NEW tests:
  - (a) loop stops on no-tool-calls; (b) safe tool dispatched + result fed back; (c) dangerous tool
    → turn SUSPENDS with pending `ApprovalId`, does NOT block; (d) `approve()` resumes + completes;
  - (e) **RED-TEAM crash-replay:** kill between two tool dispatches; replay does NOT re-execute the
    first side effect AND the replayed `SubmitOutcome` (incl. `ApprovalId`/TTL) is VALUE-IDENTICAL
    (proves journaled-clock determinism);
  - (f) `max_iterations` bound enforced; (g) persisted approval survives simulated restart
    (`PendingApprovalStore` reload) + still approvable; (h) unknown-tool/bad-args feed back as error
    tool_result without abort; (i) operator route returns redacted proof + approve resumes.
- `cargo clippy --workspace --all-targets -- -D warnings` (pedantic) on ubuntu AND windows.
- `cargo doc --no-deps -D warnings` on ubuntu AND windows.
- `scripts/audit-layer-b.sh` (flagship skill generic) + NEW CI guard against binary commits.
- `cargo deny check` (no new deps in default build; the loop adds no native dep).
- **Manual:** serve mode — message triggers safe skill → observe tool call + final answer; message
  triggers dangerous skill → observe suspend, see it in `GET /approvals/pending` with redacted
  proof, POST approve → turn completes; inspect turn-audit sink for start/dispatch/suspend/
  resume/stop_reason.

**Effort:** L+ (4-6 weeks realistic) — keystone with durable-replay hardening, persisted approval
store, operator surface, request/failover tool plumbing, SPI rename, manifest schema, docs/example.
**Build as checkpoints 1A→1D** (amendment 1) with distinct commits/milestones, not one monolith:
- **1A** — LLM tool plumbing (`ChatCompletionsRequest.tools` + `complete_with_tools` failover);
  compiles + unit tests. (files 1–2)
- **1B** — agent tool loop + safe `fs_read_allowlisted` flagship, end-to-end auto-run; loop bounds
  + unknown-tool feedback tests. (files 3–4, 6 partial)
- **1C** — persisted `PendingApprovalStore` + `ThinkOutcome::Suspended` + suspend/resume; survives
  simulated restart; resumable-turn-state defined as above. (files 5–6)
- **1D** — gateway approval routes + chat affordance + minimal turn-audit sink + crash-replay
  red-team proof (value-identical `SubmitOutcome`). (files 8–9)

---

## 6.5 Phase 4.5 — Growth loop (amendment 4; after scheduler, before multi-agent)

**Branch:** `feat/familyclaw-v4_5-growth-loop`. **Why here:** needs the Phase-1 approval gate
(every change is approval-gated), Phase-3 memory (safe summaries to learn from), and Phase-2 audit
(every proposal is traced). NOT before the tool loop; NOT after multi-agent.

**Pipeline:** `proof bundle → safe memory → pattern proposal → eval proposal → approval-gated
skill/policy update`.
1. **Proof → safe memory:** completed proof bundles (already redacted) become memory entries — only
   the safe `output_summary`, never raw inputs/secrets (reuses Phase-1 redaction + Phase-3 store).
2. **Pattern proposal:** an offline analysis proposes a candidate pattern ("skill X repeatedly
   needed argument shape Y" / "policy Z denied N times for the same safe case"). A PROPOSAL only.
3. **Eval proposal:** the candidate must come with a proposed eval (how we'd verify it helps) — no
   change without a test that proves value, mirroring the Phase-3 recall-benchmark discipline.
4. **Approval-gated update:** a human operator must approve before any skill is added or
   any policy/permission is changed. Reuses the Phase-1 operator approval surface.

**Hard invariants (non-negotiable):**
- ❌ **No silent self-modification.** A skill/policy change NEVER lands without explicit human approval.
- ❌ **No silent permission expansion.** Risk levels / approval policies cannot be auto-relaxed.
- ✅ Every proposal is audited (Phase-2) and carries its eval + the proof bundles that motivated it.
- ✅ Family-agency: an agent may *propose* its own growth, but the gate is the same as any operator action.

**Effort:** M (1.5-2 wk). **Proof-of-done:** a proposal flows end-to-end to the approval surface;
an UNAPPROVED proposal NEVER mutates a skill/policy (test); an approved one applies + is audited.

---

## 7. What NOT to do
- ❌ `ort`/ONNX as the default embedding path (unverified MSVC, trips deny) — pure-Rust default,
  `ort` opt-in only after Phase-0 spike.
- ❌ In-memory approval state (dies in the crash the durable layer exists to survive).
- ❌ Inline-block awaiting approval (hangs the ractor actor) — cooperative suspend (return) only.
- ❌ Auto-run an LLM-chosen network target as ReadOnly (SSRF). `fs-read` (allowlist) is the flagship,
  not http-get; http-get deferred to ≥1D/P2 as require-approval + egress allowlist + redirect/IP checks.
- ❌ Let `SUSPENDED_MARKER` (or any string sentinel) flow through the normal-reply pipe — suspend is a
  first-class `ThinkOutcome::Suspended` state by 1C.
- ❌ Ship Phase 1 as one monolithic 4-6 week commit — build checkpoints 1A→1D.
- ❌ Ship the SPI named `MockSkill` or claim "tools for free" without a schema + QUICKSTART.
- ❌ Claim "green on MSVC" before Phase 0's MSVC clippy+doc job exists.
- ❌ **Silent self-modification or silent permission expansion** (Growth loop, §6.5) — every skill/
  policy change is human-approval-gated and audited; no risk level auto-relaxes.
- ❌ SurrealDB revival; eternal-thread crate integration; cron-expression parsing in v1.0.

---

*Panel: Designer → Skeptic → Constraint Guardian → User Advocate → Arbiter. Disposition REVISE,
folded into this spec. Ready to build Phase 0 + Phase 1. Layer A (no private data — verify with
`scripts/audit-layer-b.sh` before commit).*
