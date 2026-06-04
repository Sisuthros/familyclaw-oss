# Changelog

All notable changes to FamilyClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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