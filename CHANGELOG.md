# Changelog

All notable changes to FamilyClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-alpha.2] - 2026-06-11

### Added
- **dream-cron** — `familyclaw-dream` päiväkohtainen idempotentti cron-binääri
- **Discord inbound** — `POST /discord/interactions` gatewayssä (Ed25519)
- **scripts/public-demo.ps1** — geneerinen julkinen demo (minimal-gateway + test + bench)
- **.env.example** — Kerros B -pohja repossa (tyhjät salaisuudet)
- **docs/handoff/agent_gamma_LIVE_TURN_EXECUTOR.md** — agent_gamma omistaa agent/memory-live-executorin

### Changed
- **OSS sanitization** — testit ja kommentit käyttävät `agent_a` / `agent_alpha` eikä perhenimiä
- **README / QUICKSTART / RUNBOOK** — julkinen polku ensin, perheversio Layer B:n ulkopuolella

### Verified
- `cargo test --workspace` — PASS
- `cargo run -p familyclaw-bench --bin bench -- all` — 6/6 PASS
- `scripts/public-demo.ps1` — PASS

---

## [0.1.0-alpha] - 2026-06-09

### Added
- **examples/minimal-gateway** — "FamilyClaw in 60 seconds" demo: 1 agent + Resonance Bus + MockChannel, zero external deps
- **CONTRIBUTING.md** — Contribution guidelines, PR checklist, commit conventions, benchmark requirements
- **GOVERNANCE.md** — Maintainer roles, bus factor ≥ 2, RFC process, KERROS A/B boundary (non-negotiable), release process
- **GitHub Actions CI** — check, test, clippy, fmt, bench (scorecard artifact), layer-b-audit, release pipeline with binary artifacts
- **README.md** — "60 seconds" quickstart, benchmark command, verification commands

### Changed
- **familyclaw-gateway** — Removed 9 dead-code constants; config now flows through FamilyConfig (KERROS B boundary respected)
- **familyclaw-hearth** — Fixed `unused_mut` warning in test
- **familyclaw-gateway src/config.rs** — Removed unused `is_yolo()` accessor

### Verified
- `cargo check --workspace` — 0 warnings
- `cargo test --workspace` — 120+ tests PASS
- `cargo run -p familyclaw-bench --bin bench -- all` — **6/6 scenarios PASS** (crash_matrix, retention_curve, dream_quality, emotional_contagion, semantic_retrieval, eternal_thread)
- Deterministic Scorecard generated at `crates/familyclaw-bench/out/SCORECARD.md`

### Security
- Layer B contamination audit in CI (enforces KERROS A/B boundary)
- SHA-256 tamper detection on durable log
- Input sanitization for all external-facing channels
- WASM sandbox with fuel limiting for untrusted code

---

## [0.1.0] - 2026-06-04

### Added
- **familyclaw-core** — Foundation types, error hierarchy, timestamp utilities, agent identity
- **familyclaw-bus** — Ractor actor mesh with affective contagion (sibling emotional state leaking)
- **familyclaw-durable** — Deterministic replay engine; side effects not re-run on recovery
- **familyclaw-memory** — Eternal Thread memory with Ebbinghaus decay, protected identity anchors, dual-write safety
- **familyclaw-dream** — Nightly consolidation cycle: duplicate merge, contradiction drop, date absolutization (hippocampal model)
- **familyclaw-emotion** — Valence-arousal affective nervous system with homeostasis regulation
- **familyclaw-latent** — Hidden-state vector exchange between siblings with dimension bridging and text fallback
- **familyclaw-sandbox** — Wasmtime WASM sandbox for safe untrusted code execution
- **familyclaw-security** — SHA-256 tamper detection, input sanitization, Layer A/B boundary enforcement
- **familyclaw-bridge** — HTTP organ server bridge (Axum) for external communication
- **familyclaw-agent** — Agent lifecycle management, heartbeat, configuration loading
- **familyclaw-channels** — Multi-channel communication layer (Discord, terminal, HTTP)
- 534 tests across 12 crates
- `unsafe_code = "forbid"` enforced at workspace level
- Clippy pedantic with warnings-as-errors
- Comprehensive `.gitignore` enforcing Layer A/B boundary
- MIT License
- ARCHITECTURE.md and CODE_REVIEW documentation

### Security
- Layer B contamination audit in CI
- SHA-256 tamper detection on durable log
- Input sanitization for all external-facing channels
- WASM sandbox with fuel limiting for untrusted code