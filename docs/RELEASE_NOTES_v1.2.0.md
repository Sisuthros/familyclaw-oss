# FamilyClaw v1.2.0 — Release Notes

A Rust agent runtime where in-flight work survives a crash — at-most-once
external side effects, durable memory, contract-checked coordination.

## Highlights

- **Hearth SurrealDB persistence fixes.** The `emotional_state` and
  `narrative_thread` (`set_thread`) persistence bugs are fixed. Previously a bare
  `UPSERT` created rows under random ids, and an RFC3339 string written into a
  datetime field silently failed to persist. The fix uses `type::record` +
  `type::datetime` / `time::now()` and a batch `UPSERT`, so records land under
  stable ids with real datetime values. Round-trip tests were added to lock the
  behaviour in (`familyclaw-hearth`).

- **`--all-features` made green + a dedicated CI gate.** Build, test, clippy, and
  doc all pass under `--all-features`, and a dedicated `all-features` CI job now
  runs `test` / `doc` / `clippy -D warnings` across the full feature matrix. This
  job exists specifically to catch feature-gated regressions like the surreal one
  before they ship.

- **Provider failover taxonomy.** Failover now classifies errors by retryability,
  distinguishing *transient* failures (timeout, 5xx, 429) from *terminal* ones
  (401 auth). A dead API key is treated as terminal and kills the chain fast
  instead of looping through it repeatedly.

- **Cooldown state machine + exponential backoff.** Rate-limited providers move
  through an explicit cooldown state machine with exponential backoff rather than
  being retried immediately.

- **Key-pool rotation.** Multiple API keys for the same provider are rotated
  across a key pool, so a single throttled or exhausted key does not stall the
  provider.

- **Channel-less serve mode.** `FAMILYCLAW_CHANNEL_KIND=none` lets `serve` and
  `status` run with no family keys configured, making the OSS build runnable out
  of the box.

- **Windows installer (`install.ps1`).** A cold start completes in under five
  minutes, registers a Scheduled Task, and binds to localhost by default.

- **`VectorStore` interface + embeddings infrastructure.** The embeddings infra
  and a `VectorStore` interface ship and are tested. Semantic retrieval is **off
  by default** — the semantic weight is only turned on once a labeled fixture
  proves a Hit@k gain over keyword retrieval. Shipping it off until then is an
  honest default, not a regression.

- **Real `fs_read` and `web_fetch` runtime wiring.** Two genuinely functional
  reference skills are wired to the runtime: `fs_read` (allowlisted local file
  read) and `web_fetch` (read-only HTTP GET with SSRF guards).

- **LangGraph crash-safety benchmark** (`bench-competitors/langgraph`). A single,
  reproducible metric: after a crash, how many external side effects re-execute?
  FamilyClaw records **0** at every crash point (`clean`, `before_write`,
  `mid_replay`); LangGraph records **0 / 1 / 2** at those same points. One metric,
  reproducible from the bench directory.

- **Pre-publish history leak gate** (`scripts/pre-publish-scan.sh`). Scans git
  history *and* commit messages for private names before any public push, so a
  leaked name cannot slip out through history even if the working tree is clean.

- **Flagship continuity demo.**
  `cargo run -p familyclaw-agent --example two_agents_memory` runs two live agents
  on the bus and proves — with assertions — real message delivery, emotion
  contagion, dream consolidation, and time-based decay end to end.

## Known limitations

Honest list of what is **not** a shipped capability in v1.2.0:

- **PostgresJournal / multi-node HA** — not built; on the roadmap.
- **Send-side latent translation** — a fenced research track that **always falls
  back to text**; not production behaviour.
- **Semantic retrieval weight** — off by default until a labeled fixture proves a
  Hit@k gain (see above).
- **Provider skill bodies** (`email`, `github`, `discord`, `file_patch`) —
  complete implementations of the skill *contract* using placeholder data, not
  yet wired to live providers. Wiring a real provider is a swap of the execution
  body.
- **Claw language compiler** — an experimental spike, excluded from the workspace
  and from CI.
- **Growth-loop apply path** — deferred for safety; the proposal core cannot
  mutate any skill, policy, or permission.

## Verification

Hosted GitHub Actions runs on a zero-spend account and is **not** relied upon as
the authoritative gate. The repo defines the CI gates — `fmt`, `clippy -D
warnings`, `test`, `doc`, `--all-features`, the Layer B audit, MSRV, and a
Windows build+test job — but the authoritative proof is the local, reproducible
verification documented in [docs/EXPO_VALIDATION_PROOF.md](EXPO_VALIDATION_PROOF.md).
That proof covers the full suite (~1680 tests) and the 8/8 continuity scorecard.
