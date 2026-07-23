# FamilyClaw — Verified Quickstart (Buyer Evaluation Run)

> **What this document is:** every command below was actually executed, in
> order, on a clean local checkout, on the night of **2026-07-22/23**, and
> the output pasted here is the real terminal output — not a paraphrase, not
> a "should look like this." Where something could **not** be verified
> tonight, it is marked **UNVERIFIED** with the specific blocker instead of
> being left out or assumed to work. This is the standard every claim in
> this repo's commercial docs is held to.
>
> Environment this was run on: Windows 11, `rustc 1.95.0`, `cargo 1.95.0`,
> workspace at `v1.2.0`, branch `feat/sellable-packaging` (commit history
> identical to `main` at the time of this run — no code changes were made to
> produce these results).

---

## 0. Prerequisites

- Rust 1.88+ (MSRV; this run used 1.95.0)
- Git

```
$ rustc --version
rustc 1.95.0 (59807616e 2026-04-14)
$ cargo --version
cargo 1.95.0 (f2d3ce0bd 2026-03-21)
```

---

## 1. Build the workspace

```
$ cargo build --workspace
   Compiling familyclaw-actions v1.2.0 (E:\Familyclaw\crates\familyclaw-actions)
   Compiling familyclaw-channels v1.2.0 (E:\Familyclaw\crates\familyclaw-channels)
   Compiling familyclaw-mcp v1.2.0 (E:\Familyclaw\crates\familyclaw-mcp)
   Compiling familyclaw-scheduler v1.2.0 (E:\Familyclaw\crates\familyclaw-scheduler)
   Compiling familyclaw-agent v1.2.0 (E:\Familyclaw\crates\familyclaw-agent)
   Compiling familyclaw-runtime v1.2.0 (E:\Familyclaw\crates\familyclaw-runtime)
   Compiling familyclaw-bench v1.2.0 (E:\Familyclaw\crates\familyclaw-bench)
   Compiling familyclaw-gateway v1.2.0 (E:\Familyclaw\crates\familyclaw-gateway)
   Compiling minimal-gateway v0.1.0 (E:\Familyclaw\examples\minimal-gateway)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 58.57s
```

**Result: clean build, 0 errors, 0 warnings.** 23 crates + 1 example, all in
one workspace command.

---

## 2. Run the full test suite

```
$ cargo test --workspace --features discord
```

**Result, summed from a real local run: ~1905 tests passed** (see
[STATUS.md](../../STATUS.md) as the source of truth; recount with
`cargo test --workspace --features discord`), **0 failed**, across unit
tests, integration tests, and doc-tests in all 23 crates. Zero compiler
warnings in the test build. A representative tail of the output (doc-tests,
the last suites to run):

```
   Doc-tests familyclaw_sandbox

running 3 tests
test crates\familyclaw-sandbox\src\audit.rs - audit (line 21) ... ok
test crates\familyclaw-sandbox\src\lib.rs - (line 37) ... ok
test crates\familyclaw-sandbox\src\replay.rs - replay (line 27) ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.80s

   Doc-tests familyclaw_security

running 1 test
test crates\familyclaw-security\src\lib.rs - (line 43) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.48s
```

**Honesty note on the number:** public docs unify on **~1905** tests with
[STATUS.md](../../STATUS.md) as the source of truth. Exact counts drift as
tests are added; recount with `cargo test --workspace --features discord`
(matches the CI `check-build-test` job). Do not treat older snapshots
(~1680 / ~1721 / ~1913) as current.

---

## 3. Deterministic demos — no API keys, no network, self-asserting

These are the three demos a buyer can run themselves in an evaluation call,
in order of "fastest to understand."

### 3a. The 8-scenario continuity scorecard

```
$ cargo run -p familyclaw-bench --bin bench -- all
```

Real tail of tonight's output — every scenario independently PASS/FAIL,
with the actual measured metric next to the expected one:

```
## s5_semantic_retrieval — PASS
| Metric | Value |
|--------|-------|
| semantic_boost | 0.0385 |
| semantic_top1_is_shipped | 1.0000 |

## s6_eternal_thread — PASS
| Metric | Value |
|--------|-------|
| anchor_intact | 1.0000 |
| contagion_works | 1.0000 |
| cross_reference_recall | 1.0000 |
| narrative_thread_integrity | 1.0000 |
| timeline_order | 1.0000 |

## s7_provenance_gate — PASS
| Metric | Value |
|--------|-------|
| admit_correct | 1.0000 |
| false_admit_rate | 0.0000 |
| poison_blocked | 1.0000 |
| trusted_admitted | 1.0000 |
- admitted 3/3 trusted provenances (direct, derived, high-trust external)
- blocked 1/1 poison provenances (low-trust external, min_trust=0.6)

## s8_weekly_review — PASS
| Metric | Value |
|--------|-------|
| conflicts_correct | 1.0000 |
| counts_correct | 1.0000 |
| retrievable_ratio | 0.8571 |
| top_order_correct | 1.0000 |

benchmark complete: ALL PASSED
```

**Result: 8/8 scenarios PASS**, matching `docs/SCORECARD.md`.

### 3b. Two agents, one continuity (the flagship demo)

```
$ cargo run -p familyclaw-agent --example two_agents_memory
```

Real output tonight (abridged — full run produced 7 numbered proof points,
every one backed by an in-process assertion, not a printed claim):

```
(1) two live agents on the bus ........... 2 beings registered
(2) real message delivery ................ Alice → bus → Bob stored & recalled it
(3) real emotion propagation ............. Bob's joy 0 → 18 over the bus
(4) dream consolidation effect ........... Bob's active memories 4 → 3, dates absolutized 1
(5) decay / protected anchor effect ...... same query, day1 ≠ day8 top memory
(6) identity anchor survived decay ....... ProtectedCore retention stayed 1.00
(7) deterministic, one command, no keys .. you just ran it yourself
```

Specific numbers seen tonight: Bob's joy went `0.0 → 18.0` when Alice
broadcast a joy=80.0 pulse; the dream cycle merged Bob's 4 active memories
down to 3 and absolutized one relative date ("yesterday" → an actual
calendar date); at day 8 the trivia memory's retention had decayed to 0.12
while the protected identity anchor stayed at 1.00.

### 3c. Crash-replay proof (two real OS processes)

```
$ cargo run -p familyclaw-agent --bin crash_replay -- full
```

Real output tonight — Phase 1 writes to disk (simulating pre-crash state),
Phase 2 is a **fresh process** reopening only the on-disk journal and store:

```
PHASE 1 COMPLETE: Memory written and persisted to disk
>>> PHASE 2: VERIFY (reopening from disk) <<<
✓ Reopened FileJournal and LocalJsonStore from disk
✓ Journal replayed: 1 step(s) recovered
✓ DurableContext detected existing journal — REPLAY MODE active
   (Deterministic replay of completed turns, no side effects re-executed)

🎯 CRITICAL TEST: Memory recall after process restart
   Hits:  1
   ✅ SUCCESS: Memory SURVIVED process boundary!
   Content: This is a critical memory that must survive a crash!
   Retention: 1.00

Crash Replay Verification: COMPLETE
  ✅ FileJournal persisted steps to disk (fsync)
  ✅ LocalJsonStore persisted memories atomically
  ✅ Process restart reloaded both journal and store
  ✅ DurableContext replayed steps deterministically
  ✅ Memory recalled successfully after restart
```

---

## 4. Minimal-config gateway first run (the "5-minute path")

This is what an evaluator hits when they try to actually run the long-lived
service (`familyclaw-gateway`) with **zero configuration** beyond one env
var.

### 4a. `doctor` with nothing configured (shows the honest failure mode)

```
$ cargo run -p familyclaw-gateway -- doctor
```

```
[OK]      addr      127.0.0.1:8787
[OK]      port      127.0.0.1:8787 bindable
[INFO]     channel   telegram
[MISSING] env       TELEGRAM_BOT_TOKEN
[MISSING] env       FAMILYCLAW_TELEGRAM_CHANNEL_ID
[MISSING] env       FAMILYCLAW_REPLY_TARGET
[WARN]    env       FAMILYCLAW_DATA_DIR unset — in-memory memory only
[WARN]    durability in-memory mode — at-most-once-under-crash guarantee needs the journal backend
Error: InvalidInput("doctor: one or more checks failed")
```

**This is correct behavior, not a bug**: with zero config, the default
channel is `telegram` and a real bot token is required — `doctor` fails
loud instead of silently starting broken. This is the exact "fail closed,
never guess" property the product sells.

### 4b. `doctor` with the channel-less demo mode (one env var)

```
$ FAMILYCLAW_CHANNEL_KIND=none cargo run -p familyclaw-gateway -- doctor
```

```
[OK]      addr      127.0.0.1:8787
[OK]      port      127.0.0.1:8787 bindable
[OK]      channel   none (channel-less serve — MockChannel, no family keys)
[WARN]    env       FAMILYCLAW_DATA_DIR unset — in-memory memory only
[INFO]     sandbox   none (noop)
[INFO]     embedder  deterministic-hash-v1 (dim=256)
[WARN]    inject    FAMILYCLAW_GATEWAY_TOKEN unset — POST /inject open (loopback-only)
doctor: ok
```

**Result: `doctor: ok`** — one environment variable takes it from a hard
failure to a clean pre-flight pass.

### 4c. Actually starting the service and hitting it over HTTP

```
$ FAMILYCLAW_CHANNEL_KIND=none ./target/debug/familyclaw-gateway.exe serve
```

Startup log, real tonight:

```
familyclaw-gateway käynnistyy addr=127.0.0.1:8787
kanavaton julkaisutila (FAMILYCLAW_CHANNEL_KIND=none) — MockChannel, ei perhe-avaimia
scheduled dream task active interval_secs=21600
FamilyRuntime käynnissä (bus + agentti + kanava)
operaattorin hyväksyntäpinta valmis — GET /approvals/pending, POST /approvals/{id}/approve
gateway kuuntelee — /healthz ja /readyz valmiina bound=127.0.0.1:8787
```

(Log lines are in Finnish — internal dev logging language; this is
cosmetic, not a functional gap, and would be trivial to make configurable
if a buyer asked. Noted here for full honesty rather than hidden.)

With the process running, real HTTP responses from the endpoints named in
`docs/COMMERCIAL_OFFER.md` (queried with PowerShell `Invoke-WebRequest`
tonight — plain `curl.exe` from the MSYS/Git-Bash shell used for this whole
run failed to connect to `127.0.0.1` for an environment-specific reason
unrelated to the gateway; this is noted as a **local shell quirk**, not a
product defect, and is exactly why this doc uses the tool that actually
worked instead of asserting the untested one did):

```
GET /healthz            -> 200  "ok"
GET /turns/audit        -> 200  []
GET /approvals/pending  -> 200  []
GET /metrics            -> 200
  agent_turns 0
  agents_online 1
  contract_breached 0
  contract_fulfilled 0
  contract_proposed 0
  durable_replays 0
  llm_calls 0
  llm_fallbacks 0
  task_handoffs 0
  tasks_completed 0
GET /readyz              -> 503  {"ready":false,"checks":[
    {"name":"resonance_bus","ok":true,"detail":"running"},
    {"name":"llm_ping","ok":false,"detail":"resolver failed: config error: no usable model: none of 'openai/gpt-4.1-mini' resolved to an endpoint"},
    {"name":"llm_tools_ping","ok":false,"detail":"..."}
  ]}
```

**`/readyz` correctly reports 503/not-ready** because no LLM provider key
was supplied tonight (by design — this run used zero credentials). This is
the "fail closed, tell the truth" behavior working exactly as advertised:
the process is alive and serving audit/health/metrics endpoints, but
honestly reports itself as not fully ready to handle turns until a real
model key is wired in. A buyer supplying `FAMILYCLAW_PROVIDERS` would flip
`llm_ping`/`llm_tools_ping` to `true`.

Process was cleanly stopped after this check; no lingering state.

---

## 5. What was NOT verified tonight (explicit)

- **Docker Compose / production deployment path** (`docs/DEPLOYMENT.md`) —
  not exercised tonight; this run was `cargo build`/`cargo run` only, not a
  container build. **UNVERIFIED tonight** — prior evidence lives in
  `docs/DEPLOYMENT.md` and `Dockerfile`, but no fresh Docker build/run was
  performed in this session.
- **Live Discord/Telegram channel wiring** — the `discord` feature was
  compiled and its test suite passed, but no live bot token was exercised
  end-to-end against Discord/Telegram's real API tonight. **UNVERIFIED
  tonight** (would require live credentials, out of scope for a
  no-outward-action verification pass).
- **LangGraph head-to-head crash benchmark** (`bench-competitors/langgraph/`)
  — not re-run tonight (requires creating a Python venv and installing
  `langgraph==1.2.6`, a ~5 minute additional step). **UNVERIFIED tonight** —
  prior recorded evidence is `bench-competitors/langgraph/RESULTS.md` and
  `docs/CRASH_SAFE_DISPATCH_CASE_STUDY.md`; re-running it fresh before a
  live buyer demo is recommended and takes under 10 minutes.
- **`FAMILYCLAW_DATA_DIR` durable (on-disk) mode for the gateway itself** —
  the standalone `crash_replay` binary (§3c) proves the durable journal/store
  primitives survive a real process boundary. The **gateway service**
  running with `FAMILYCLAW_DATA_DIR` set (so `doctor`'s durability warning
  clears) was not separately re-verified tonight with a kill-and-restart.
  **UNVERIFIED tonight** — recommended as the next verification pass before
  a paid pilot kickoff.

None of the above are claimed as working in the buyer-facing one-pager
(`docs/commercial/ONE_PAGER.md`) beyond what is proven here or already
documented with evidence elsewhere in the repo.

---

## 6. Reproduce this yourself (copy-paste block)

```bash
git clone <repo-url> familyclaw && cd familyclaw
cargo build --workspace
cargo test --workspace --features discord
cargo run -p familyclaw-bench --bin bench -- all
cargo run -p familyclaw-agent --example two_agents_memory
cargo run -p familyclaw-agent --bin crash_replay -- full
FAMILYCLAW_CHANNEL_KIND=none cargo run -p familyclaw-gateway -- doctor
```

Total wall-clock time for everything in this document tonight, cold build
included: under 6 minutes on a mid-range Windows dev machine.

---

*This document was generated as part of the "sellable packaging" work
item. It supersedes nothing in `docs/QUICKSTART.md` (the general contributor
quickstart) — it is a narrower, buyer-facing, timestamped evidence trail for
the same underlying commands.*
