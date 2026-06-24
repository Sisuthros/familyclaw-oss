# Changelog

All notable changes to FamilyClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-alpha.8] - 2026-06-16

### Added
- **familyclaw-actions** (UUSI crate, KERROS A / OSS) — toiminta-/skill-ajoaika joka
  toteuttaa putken `observe → plan → approve → execute → verify → persist proof →
  remember → report`. Sisältää geneerisen skill-rekisterin, kyvykkyyspolitiikan,
  hyväksyntäportin, redaktoivat todistepaketit (proof bundles) ja audit-lokin.
  Vain mock-skillejä — ei oikeita Gmail/GitHub-verkkokutsuja, ei providereita,
  sieluja eikä avaimia. 161 yksikkötestiä, mukaan lukien redaktio-todiste
  (synteettinen avain-mallinen syöte korvautuu `[REDACTED]`-merkinnällä).
- Juuren `Cargo.toml`: `familyclaw-actions` lisätty `[workspace.dependencies]`-listaan
  (aakkosjärjestyksessä).
- **familyclaw-gateway operaattorin hyväksyntäpinta** (suspend/resume-silta,
  roadmap §6 D1/D2) — kaksi bearer-suojattua HTTP-reittiä (sama
  `FAMILYCLAW_GATEWAY_TOKEN` kuin `/inject`, vakioaikainen täsmäys):
  - `GET /approvals/pending` — listaa odottavat hyväksynnät **redaktoituina**
    (`approval_id`, `redacted_summary`, `created_at`); ei koskaan raakaa
    payloadia, työkaluargumentteja eikä salaisuuksia.
  - `POST /approvals/{approval_id}/approve` — myöntää hyväksynnän ja ajaa
    keskeytyneen toiminnon loppuun (payload-sidottu, kertakäyttöinen).

  Reitit rekisteröidään aina; kun toimintoajoympäristöä ei ole kytketty,
  handlerit vastaavat `503`. Bearer-tokenin puuttuessa `401`.
- **familyclaw-gateway turn-audit-reitti** (TURN-AUDIT, roadmap §6 D6) —
  `GET /turns/audit` palauttaa operaattorille havainnoitavan tool-loop-jäljen
  JSON-listana (`familyclaw_actions::ExecAuditEvent`): vuoron korrelaatiotunniste
  (`action_id`), tapahtumatyyppi (`turn_started` / `tool_dispatched` /
  `turn_suspended` / `turn_resumed` / `turn_answered` / `turn_max_iterations`),
  aikaleima (`at`) ja **redaktoitu** selite (`detail`, redaktoitu jo
  kirjaushetkellä). Sama bearer-suojaus kuin `/inject`; `503` jos turn-auditia
  ei ole kytketty.
- **familyclaw-runtime jaetut operaattorikahvat** — `FamilyRuntime::actions()`
  (`Arc<Mutex<ActionRuntime>>`) ja `FamilyRuntime::turn_audit()`
  (`Arc<AuditCollector>`) altistavat saman lukitun toimintoajoympäristön ja
  vain-lisäävän turn-audit-keräimen jotka agentin tool-loop omistaa — gateway
  lukee odottavat hyväksynnät ja tool-loop-jäljen jakamatta agentin sisuksia.
- **familyclaw-actions lähetyksen idempotenssi-outbox** (`DispatchOutboxStore` /
  `JournalDispatchOutbox`) + `ActionRuntime::submit_task_idempotent` — sulkee
  ikkunan sivuvaikutuksen suorituksen ja sen agenttikerroksen journaloinnin
  välissä. Jokainen lähetys kytketään kutsujan johtamaan vakaaseen
  idempotenssi-avaimeen ja kirjataan kaksivaiheisesti (intent ennen
  sivuvaikutusta, committed sen jälkeen). `familyclaw-runtime build_family`
  kytkee kaatumiskestävän `JournalDispatchOutbox`:n
  (`<FAMILYCLAW_DATA_DIR>/dispatch_outbox.jsonl`) tuotantopolulla.
  **Takuu (rehellinen raja):** sivuvaikutus **lähetetään korkeintaan kerran**
  prosessin kaatumisen / SIGKILL:n yli — se ei koskaan laukea kahdesti.
  Sitoutunut (committed) lähetys palautuu arvo-identtisenä ajamatta
  sivuvaikutusta uudelleen; kaatuminen kapeassa intent-only-ikkunassa
  **epäonnistuu suljettuna** (nolla tai yksi suoritusta, vaatii toipumisen) sen
  sijaan että ajaisi sokeasti uudelleen. Tämä on **kaksoislaukaisun esto
  (duplicate-prevention) kaatumisen yli, EI lupaus universaalista
  "exactly-once completion" -valmistumisesta.**

### Verified
- `bash scripts/audit-layer-b.sh` — PASS (ei sieluja/avaimia/nimivuotoja)
- `cargo build --workspace` ja `--features discord` — PASS
- `cargo clippy --workspace --all-targets --features discord -- -D warnings` (pedantic) — clean
- `cargo test --workspace --features discord` — kaikki PASS (familyclaw-actions: 161)
- `cargo doc -p familyclaw-actions --no-deps` (RUSTDOCFLAGS=-D warnings) — clean

## [0.1.0-alpha.7] - 2026-06-14

### Fixed
- **CI all-features-job** — ei enää aktivoi kuollutta `surreal`-featurea (kytkemätön,
  API-vanhentunut SurrealHearthStore kaatoi `--all-features`-buildin). Korvattu
  eksplisiittisellä elävien featureiden joukolla.
- **CI cargo-audit-job** — lisätty `--ignore`-liput (cargo-audit ei lue deny.tomlia)
- **familyclaw-durable** — `FileJournal` mutex-poison-recovery (`read_all_entries` +
  `append`): poistettu tuotantopolun `unwrap()` joka rikkoi craten omaa "ei panikia
  tuotannossa" -lupausta

### Added
- **familyclaw-channels** — `verify_signature` SUCCESS-path-testi (Ed25519, vain
  failure-haara oli aiemmin katettu) + `from_webhook`/interaction-reunaehtotestit (+22)

### Changed
- **familyclaw-acp** — poistettu turha `#[allow(dead_code)]` (kenttä on käytössä)

### Verified
- `cargo test --workspace` — 1209 PASS, 0 fail
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo audit` (perustellut ignoret) — 0 vulnerabilities; Layer-B — PASS

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

### Verified
- `cargo test --workspace` — 1186 PASS, 0 fail
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo run -p familyclaw-bench --bin bench -- all` — 8/8 skenaariota PASS
- Layer-B — PASS

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
- **familyclaw-durable** — Deterministic replay engine; on recovery a side effect is dispatched at most once (never re-run twice)
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