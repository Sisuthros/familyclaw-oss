# Changelog

All notable changes to FamilyClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Covers everything landed since `v1.2.0` (2026-07-02). No version has been
tagged for these changes yet.

### Added
- **Reliability Console** (`GET /console`) — live operator surface: a "Now"
  strip, an SSE audit feed, and one-click approvals. `docs/CONSOLE.md`.
- **Time Machine** — `familyclaw_durable::TimeMachine` (inspect / fork / diff)
  with a `DryRunRecorder` that has no dispatch path by construction, a
  `FORK_MARKER` audit trail, and a fail-closed `InvalidFork` error; CLI
  `familyclaw replay inspect|fork|diff|demo`. Companion
  `familyclaw-growth::evidence` (`ReplayEvidence`, `EvidenceLedger`,
  `evaluate_for_approval`) is fail-closed and still has **no** apply path.
- **`familyclaw import --from openclaw|hermes`** — tolerant adapters parse
  observed OpenClaw / Hermes export shapes into a versioned bundle and write an
  import report plus a quarantine manifest. Imported skills are **quarantined**
  (never registered, never executed); imported memories carry
  `Provenance::External` at trust 0.2, below the provenance gate default, so
  nothing is auto-admitted. `docs/MIGRATION.md`.
- **Security bench suite** (`cargo run -p familyclaw-bench --bin bench --
  security`) — SEC1 fuel exhaustion, SEC2 capability denial, SEC3
  SSRF / prompt injection, SEC4 unapproved side effect, driving the real
  sandbox and actions APIs; deterministic committed artifact +
  `docs/SECURITY_BENCH.md`.
- **Three executor skills** — `web_search` (keyless read-only GET,
  SSRF-guarded, capped results and bytes), `file_write` (real disk write behind
  a canonicalized allowlist + `AlwaysRequireApproval`; proof records a path hash
  and byte count, never content), and `research` (multi-source fetch, host
  dedup, injection-escaped markdown report). `FAMILYCLAW_FILE_WRITE_ALLOW`
  supplies the write allowlist; absent or empty stays fail-closed.
- **`file_patch` and `github_issue_draft` promoted from stubs** to real bodies
  behind the same write-safety machinery (canonicalized allowlist, approval
  gate, hash-only proof).
- **`familyclaw-mcp` crate** (MCP client, stdio + HTTP transport) plus the
  `shell_exec`, `schedule_task`, `spawn_subagent`, `github_issue` and
  `file_patch_apply` skills, and multi-agent subagent support.
- **Grounding module** (`familyclaw-agent/src/grounding.rs`) — flags a response
  that claims completed work while dispatching zero tools in that turn, and
  filters memories carrying unverified prior tool claims out of recall.
- **Soft/hard turn watchdog** — past `FAMILYCLAW_TURN_WATCHDOG_SECS` the user
  gets an interim "still working" notice and the turn keeps running; only past
  `FAMILYCLAW_TURN_WATCHDOG_HARD_MULTIPLIER` × soft is the turn abandoned.
  `FAMILYCLAW_HISTORY_MAX_CHARS` makes per-message history truncation
  configurable.
- **Auto-continuation for replies cut at the token limit** — a completion still
  ending on `finish_reason == "length"` is continued and concatenated, bounded
  by `FAMILYCLAW_MAX_CONTINUATIONS` and a total output cap; a failed
  continuation returns the accumulated partial instead of discarding it.
  `FAMILYCLAW_MAX_TOKENS` lifts the output ceiling.
- **Deep `/readyz` + `POST /canary`** — provider ping, channel state and
  journal writability, plus a 5-minute canary script.
- **Content-hash-bound growth approvals** — approval binds to a SHA-256 of the
  proposal content (status field excluded), so a record→approve content swap
  fails with `HashMismatch` instead of silently approving; persistent
  `ApprovalRecord` decision trail (`approval_history` / `approvals`). The hard
  invariant holds: there is still no `apply()` path.
- **Env-driven semantic recall** — `FAMILYCLAW_EMBED_PROVIDER=ollama` selects
  `OllamaEmbedder` (feature `ollama`), and `FAMILYCLAW_SEMANTIC_WEIGHT`
  (default 0.6) actually engages vector search, which was previously gated off
  at weight 0.0.
- **Slack channel adapter** (feature `slack`) — `SlackChannel` implements the
  `Channel` trait and `FAMILYCLAW_CHANNEL_KIND=slack` selects it in the gateway
  (`SLACK_BOT_TOKEN` + `FAMILYCLAW_SLACK_CHANNEL_ID`). **Outbound only today:**
  `chat.postMessage` works and `format_approval_prompt` renders Approve/Deny
  instructions. Socket Mode / Events API is not implemented, `POST /inject` is
  still wired to the Discord channel only (it answers `503 discord channel not
  configured` in Slack mode), and `doctor` does not yet know the `slack` kind.
  `docs/slack-setup.md`.
- **Native OIDC / JWT operator auth** — protected gateway routes accept either
  the static `FAMILYCLAW_GATEWAY_TOKEN` **or** a Bearer JWT whose `iss` / `aud`
  / `exp` match the configured IdP (HS256 shared secret or JWKS). Half
  configuration fails closed at `serve` startup; fully unset preserves the
  previous loopback-open default exactly. `docs/ENTERPRISE_AUTH.md`.
- **`PostgresJournal`** (feature `familyclaw-durable/postgres`) — the same
  `Journal` contract on a single `familyclaw_journal` table (BIGSERIAL seq,
  JSONB payload), created on open. **Library-level only:** no runtime path
  selects it, so `serve` still always opens a `FileJournal` even when
  `DATABASE_URL` is set. Round-trip test self-skips without a live database.
- **OTLP span-envelope scaffolding** (feature `familyclaw-observability/otlp`)
  — turns a `TraceContext` into a JSON span envelope plus a traces-endpoint
  helper. Deliberately **not** an OpenTelemetry SDK: there is no network
  exporter, the feature is off by default and is not enabled by any other
  crate.
- **Workflow evaluation packs** — `packs/refund-guard`,
  `packs/infra-teardown`, `packs/migration-runner`: README + WORKFLOW + eval
  scenario + PowerShell/bash demo scripts, each driving *existing* crate tests
  and binaries (`redteam_dispatch_exactly_once`, `crash_replay`,
  `continuity_daemon`, `familyclaw-bench`) rather than new fixtures. A
  documentation and script layer only — nothing here is referenced from the
  gateway's default runtime path.
- **Operator ACL + trace correlation**, OpenClaw/Hermes-shaped crash matrices,
  fail-closed gateway auth off-loopback, and Docker compose / backup /
  compliance documentation.
- **Nightly crash matrix in CI** and pinned, live-capable competitor harnesses.
- **Buyer-facing material** — `docs/commercial/QUICKSTART.md` (every command
  actually run with real output; unverified items marked UNVERIFIED),
  `ONE_PAGER.md`, `EVALUATOR_DEMO_SCRIPT.md`, a sanitized
  `docs/COMMERCIAL_OFFER.md`, `docs/BLOG_CASE_STUDY.md`, the OSS launch
  playbook, and a FamilyClaw vs. NVIDIA NemoClaw comparison.
- **Deploy gate** `scripts/deploy-appliance.ps1` — refuses a dirty tree, builds
  `--release`, backs up the running binary, restarts, then asserts `/healthz`
  200 and `/readyz` `ready:true`; every destructive step is behind `-WhatIf`.

### Changed
- **Pre-release hygiene sweep** — translated remaining Finnish documentation
  (this changelog, `Dockerfile` comments) to English; scrubbed stale
  private-path and internal-planning references from public docs; corrected
  workspace metadata (crate counts, stale caveats); pinned MSRV to 1.88
  consistently across `Dockerfile` and `examples/minimal-gateway/Cargo.toml`.
- **Gateway logs go to stderr, not stdout.** `tracing_subscriber`'s default
  stdout writer is line-buffered, which could leave `serve` logs stuck and
  never reaching a redirected log file depending on how the process was
  launched. stderr is unbuffered and is the conventional destination for a
  service's own logs. No-op for setups already redirecting both streams.
- **An unrecognized `FAMILYCLAW_CHANNEL_KIND` is now an explicit error**
  (`expected none|discord|telegram|slack`) instead of silently falling through
  to the Telegram branch.
- **`fs_read` on a directory returns the entry listing** (up to 64 names with a
  truncation tail) instead of a size-capped summary, and allowlist denials are
  now three distinguishable messages (empty allowlist / uncanonicalizable path
  / outside every configured root) without leaking any raw path. Startup warns
  when `fs_read` has no allowlist or a configured root does not exist.
- **Allowlisted file writes run without manual approval** and suspended turns
  notify the user instead of going silent; typing/progress outbound signals
  during long tool loops.
- **Docs restructured for publication** — internal planning archive moved out
  of the public tree, remaining Finnish demo/log output and archive/roadmap
  documents translated (or glossed inline where the original is verbatim
  program output), dead links removed, crash-replay demo GIF added to the
  README, `docs/README.md` index extended.
- **26 `Cargo.toml` manifests** got English `description` / `keywords` /
  `categories` / `publish` metadata for a crates.io publication.

### Fixed
- **`/readyz` failover stall.** `check_llm_ping` had an 8 s per-attempt timeout
  but no overall deadline around the failover walk, so 4 models × 2 passes
  could take ~64 s and time the probe out. A 20 s total deadline now bounds the
  walk (measured after deploy: `/readyz` always 200, median ~3 s, worst case
  the 20 s deadline).
- **Retired / missing models are treated as provider-dead.** An upstream 404
  (and the OpenAI-compatible 400 "model not found") now rotates to the next
  provider with a long cooldown, and turn-provider logs surface the final error
  class.
- **Discord messages split on line count, not only character count** — a short
  but multi-line message could still be hidden behind "Show more"; chunks now
  break at 1900 chars *or* 15 newlines, whichever comes first.
- **Discord presence** — the bot advertises online instead of appearing
  offline.
- **Flaky `FAMILYCLAW_SKILL_REGISTRY` tests** — two tests mutated the same
  process-global env var in parallel; a static lock now serializes them.
- **`clippy::manual_assert_eq` in `familyclaw-bus`** which had been blocking CI
  on `main` since 2026-07-11.
- **Rustdoc intra-doc links** pointing at private items, which failed the
  `RUSTDOCFLAGS=-D warnings` doc gate.
- **`scripts/e2e-gateway.ps1` parse error** — an em dash broke Windows
  PowerShell 5.1.
- **Missing wasmtime is now visible.** `resolve_sandbox_skills` logged
  "sandbox wired" while handing back a `NoopSandbox`; it now warns loudly when
  `SANDBOX_SKILLS=1` but wasmtime is unavailable, and `Dockerfile` gained
  `--build-arg FAMILYCLAW_FEATURES=wasmtime` to build the real sandbox.
- **Cron expressions are validated at write time** by the scheduler.
- **Expo/demo scripts made truthful and PowerShell-native**, with valid
  LangGraph reproduction commands and a booth fallback export.

### Security
- **`web_fetch` SSRF.** `validate_url` only checked non-public IPs for a
  literal IP host, so a domain resolving to `169.254.169.254` (cloud metadata)
  or `127.0.0.1` bypassed it entirely. The host is now resolved before the
  fetch and internal addresses are rejected.
- **`shell_exec` smart mode** now confines file arguments to a cwd allowlist.
- **Catastrophic `rm` targets** — `is_catastrophic_rm_target` covered only a
  literal `/home`, not a *named* user home (`/home/<user>`, `/Users/<user>`).
  The home root and its top-level sweep are now hard-blocked in every mode;
  deeper project paths still go to the approval gate.
- **Dependency advisories** — `crossbeam-epoch` 0.9.18 → 0.9.20
  (RUSTSEC-2026-0204), plus `surrealdb` 3.1.3 → 3.2.0 and `openssl` 0.10.78 →
  0.10.81 (8 advisories).
- **Layer B leak scrubbing** — private agent/operator names and paths removed
  from publishable code, comments, test fixtures and deploy scripts; local
  debug/probe scripts quarantined.

## [1.2.0] - 2026-07-02

### Fixed
- **Hearth SurrealDB persistence.** `emotional_state` and `narrative_thread`
  (`set_thread`) no longer silently fail to persist — a bare `UPSERT` was
  creating rows under random ids, and an RFC3339 string written into a
  datetime field was dropped. Fixed via `type::record` + `type::datetime` /
  `time::now()` and a batch `UPSERT`; round-trip tests added
  (`familyclaw-hearth`).

### Added
- **`--all-features` CI gate.** Build, test, clippy, and doc all pass under
  `--all-features`; a dedicated CI job runs the full feature matrix to catch
  feature-gated regressions (like the surreal one above) before they ship.
- **Provider failover taxonomy.** Failover classifies errors by retryability —
  transient (timeout, 5xx, 429) vs. terminal (401 auth) — so a dead API key
  fails fast instead of looping.
- **Cooldown state machine + exponential backoff** for rate-limited providers.
- **Key-pool rotation** across multiple API keys per provider.
- **Channel-less serve mode** (`FAMILYCLAW_CHANNEL_KIND=none`) — `serve` and
  `status` run with no family keys configured.
- **Windows installer** (`install.ps1`) — cold start under five minutes,
  registers a Scheduled Task, binds to localhost by default.
- **`VectorStore` interface + embeddings infrastructure**, semantic retrieval
  off by default until a labeled fixture proves a Hit@k gain over keyword
  retrieval.
- **Real `fs_read` and `web_fetch` skills** wired to the runtime (allowlisted
  local file read; read-only HTTP GET with SSRF guards).
- **LangGraph crash-safety benchmark** (`bench-competitors/langgraph`) — a
  reproducible metric for external side effects re-executed after a crash.
  FamilyClaw: 0 at every crash point; LangGraph: 0/1/2.
- **Pre-publish history leak gate** (`scripts/pre-publish-scan.sh`).
- **Flagship continuity demo** (`two_agents_memory` example) proving real
  message delivery, emotion contagion, dream consolidation, and time-based
  decay end to end.

## [1.0.1] - 2026-06-26

Patch release hardening v1.0.0 in response to an adversarial post-release
audit. No new features; no public API breaks.

### Fixed
- **Security — Layer B audit blind spot (#34).** The leak audit scanned files
  by extension allowlist, silently skipping tracked text files in other
  formats. It now scans every git-tracked text file, classified by content,
  so a leaked private name in any text format fails the audit. Added 14
  regression tests.
- **Concurrency — scheduler held its control-plane lock across `await` (#35).**
  `SchedulerRunner::run_shared` blocked operator-plane mutations
  (pause/resume/kill-switch) behind slow dispatch I/O. The tick now collects
  due dispatches under a brief lock, dispatches without the lock, then
  re-locks briefly to record firings — control commands no longer queue
  behind a long tick. At-most-once dispatch is preserved. Added 5 tests.
- **Docs — hosted CI status (#36).** Documented that hosted GitHub Actions was
  intentionally not relied upon at the time; the local, reproducible
  verification was the authoritative gate.

## [1.0.0] - 2026-06-25

First stable release — a Rust-native multi-agent runtime. All six phases of
the v1.0 roadmap delivered:

- **Phase 0 — CI + spike.** MSVC clippy+doc gate; embedder backend spike.
- **Phase 1 — tool loop.** Replay-correct bounded tool loop, persisted
  approval store, gateway operator approval routes, crash-replay red-team
  proof, flagship `fs_read` allowlisted skill.
- **Phase 2 — observability.** Turn + tool-call Prometheus metrics wired
  end-to-end; bounded metric sink.
- **Phase 3 — embeddings.** `familyclaw-embeddings` crate, auto-embed memory
  decorator, runtime wiring, recall benchmark gate.
- **Phase 4 — scheduler + family-agency.** `familyclaw-scheduler` (interval);
  `DreamCycle` as a scheduled task; kill-switch end to end; expire-on-no-human.
- **Phase 4.5 — growth loop (safe core).** `familyclaw-growth` records
  proposals and human decisions but never applies anything.
- **Phase 5 — multi-agent.** Orchestrator coordinates two or more agents by
  capability through the live `TurnExecutor` seam.

Known scope boundaries at release: semantic embeddings deferred to 1.1
(deterministic bag-of-words default only); growth-loop apply path deferred for
safety; multi-agent serve is sequential (parallel orchestration next).

## 0.x pre-release history

Pre-1.0.0 development (`0.1.0` through `0.1.0-alpha.8`, 2026-06-04 to
2026-06-16) built the platform up crate by crate: the core substrate
(`familyclaw-core`, `familyclaw-durable`, `familyclaw-bus`, `familyclaw-memory`,
`familyclaw-dream`, `familyclaw-emotion`, `familyclaw-latent`,
`familyclaw-sandbox`, `familyclaw-security`, `familyclaw-bridge`,
`familyclaw-agent`, `familyclaw-channels`) and later the action/skill runtime
(`familyclaw-actions`) with its approval gate, redacting proof bundles, and
idempotent dispatch outbox.

- Landed durable crash-replay, Eternal Thread memory with Ebbinghaus decay,
  dream consolidation, affective contagion over the Resonance Bus, and a WASM
  sandbox — the continuity substrate the rest of the platform builds on.
- Added the gateway's operator approval surface (suspend/resume) and a
  redacted turn-audit route, both bearer-token protected.
- Hardened CI: fixed a dead feature flag that had been silently breaking the
  `--all-features` build, added `cargo-audit`/`cargo-deny` gates, upgraded
  `wasmtime` to clear 16 advisories.
- Grew test coverage from roughly 120 tests at the first tag to over 1200 by
  the last alpha, with a deterministic 6/8-scenario continuity scorecard.
- Completed the OSS sanitization pass (generic `agent_a`/`agent_b` naming,
  Layer A/B boundary enforcement via `.gitignore` + `scripts/audit-layer-b.sh`).
