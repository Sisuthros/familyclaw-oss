# Changelog

All notable changes to FamilyClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-alpha.6] - 2026-06-14

### Added
- **familyclaw-bench S7/S8** — uudet jatkuvuus-skenaariot: `provenance_gate`
  (todistaa Sleeper-Memory-Poisoning-suojan) ja `weekly_review` (viikkokatsaus-
  konsolidoinnin determinismi)
- **familyclaw-dream `detect_conflicts`** — puhdas, mutatoimaton lähikaksois-
  parien tunnistus (ehdokkaat tutkittavaksi, ei automaattista tägäystä)
- **familyclaw-bus translate-on-send** — `BusLatentChannel::with_translator`:
  `VectorTranslator` lähetyspolulla; häviöllinen projektio → text-fallback
  (vastaanottopuolen decode pysyy tietoisesti perheen rajan takana)
- **familyclaw-acp** — testikattavuus nollasta (config/message/error puhdas kerros)
- **familyclaw-observability / -security** — rinnakkaisuus- ja reuna-testit
  (lukkovapaa atomic-laskuri kontentiossa, vakioaikainen vertailu)

### Changed
- **familyclaw-gemu** — kuolleen temp-prompt-polun poisto + käyttämättömien
  riippuvuuksien karsinta

## [0.1.0-alpha.5] - 2026-06-14

### Added
- **familyclaw-bus affektiivinen tartunta (vastaanotto)** — `AffectiveBeing`
  absorboi `EmotionPulse`-pulssin omaan tilaansa `EmotionTransition::blend`-
  kautta (kuljetus oli jo; reaktiopuoli kytketty)
- **familyclaw-gateway `orchestrate`-alikomento** — Orchestrator +
  LiveTurnExecutor elävä sisäänkäynti: multi-agent DAG ajettavissa oikeilla
  LLM-kutsuilla (`FAMILYCLAW_PLAN` JSON tai sisäänrakennettu savutesti)
- **familyclaw-memory tunne→tärkeys-silta** — `ImportanceFactors::from_emotion_state`
  + `MemoryBuilder::emotion_state` (`emotional_salience` painottaa tärkeyttä)
- **familyclaw-memory `GatedMemoryStore`** — `ProvenanceGate::admit` pakotettu
  write-polulla (myrkytyssuoja ingestionissa)

### Changed
- **wasmtime 35 → 45** — poistaa 16 RUSTSEC-advisorya (audit 21 → 0)
- **deny.toml** — perustellut audit-ignoret vain korjaamattomille/ei-FamilyClaw-
  poluille (rsa Marvin, rustls-webpki discord-ketju, 2 unmaintained)
- **README** — kuolleet docs.rs-badget poistettu, clippy-komento CI:hin kohdistettu

### Verified
- `cargo test --workspace` — 1092 PASS
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo audit` — 0 vulnerabilities (perustellut ignoret)

## [0.1.0-alpha.4] - 2026-06-13

### Added
- **familyclaw-memory provenance** — `Provenance` (DirectExperience/Derived/
  External) + `ProvenanceGate` (Sleeper-Memory-Poisoning-suoja)
- **familyclaw-emotion** — EmoMAS-inspiroitu strateginen arviointi +
  `emotional_salience` + HMM-tyylinen tunneinertia
- **familyclaw-bus** — μACP-nelivervi (PING/TELL/ASK/OBSERVE) + MESI-
  koherenssitilakone
- **familyclaw-dream** — SleepGate conflict-aware tägäys + viikkokatsaus
- **familyclaw-sandbox** — LOOP deterministinen replay + append-only audit-loki
- **familyclaw-latent** — cross-model `VectorTranslator` + cosine/blend-apurit

### Verified
- `cargo test --workspace` — 1063 PASS, `cargo clippy --all-targets` clean

## [0.1.0-alpha.3] - 2026-06-13

### Added
- **familyclaw-bridge `TurnExecutor`-sauma** — trait + `MockTurnExecutor` +
  tuottajapuolen `LiveTurnExecutor`-rajapinta (orkesterointi ↔ agentti)
- **CI-portit** — MSRV 1.85, all-features, Windows, cargo-audit, cargo-deny
- **gateway** — provider-prefixed default-malli + kanoninen `FAMILYCLAW_REPLY_TARGET`

### Changed
- **familyclaw-runtime `build_family`** — LLM-ketjun resolve epäonnistuu siististi
  (mute-varoitus, ei paniikkia); dream-silmukka omistettu sammutuksessa

### Verified
- `cargo test --workspace` — 935 PASS

> **Huom:** alempi `0.1.0-alpha.2`-merkintä jäi ilman git-tagia (vain
> `alpha`, `alpha.3`–`alpha.6` ovat tageja). Säilytetty historian vuoksi.

## [0.1.0-alpha.2] - 2026-06-11

### Added
- **dream-cron** — `familyclaw-dream` päiväkohtainen idempotentti cron-binääri
- **Discord inbound** — `POST /discord/interactions` gatewayssä (Ed25519)
- **scripts/public-demo.ps1** — geneerinen julkinen demo (minimal-gateway + test + bench)
- **.env.example** — Kerros B -pohja repossa (tyhjät salaisuudet)
- **LiveTurnExecutor handoff** — producer-tiimi omistaa agent/memory-live-executorin (sisäinen handoff Kerros B:ssä)

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