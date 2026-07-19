# Changelog

All notable changes to FamilyClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- **Pre-release hygiene sweep** — translated remaining Finnish documentation
  (this changelog, `Dockerfile` comments) to English; scrubbed stale
  private-path and internal-planning references from public docs; corrected
  workspace metadata (crate counts, stale caveats); pinned MSRV to 1.88
  consistently across `Dockerfile` and `examples/minimal-gateway/Cargo.toml`.

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
