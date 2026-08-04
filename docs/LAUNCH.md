# FamilyClaw — OSS Launch Playbook (Horisontti 2)

> **Goal:** a skeptic clones the repo, runs one command, and reproduces the
> continuity proof. **Success metric:** at least one external person runs
> `familyclaw serve` in their own environment and reports back (see
> [USERS.md](USERS.md)).

**Technical truth:** [STATUS.md](../STATUS.md)

---

## Pre-launch checklist

Run locally before any public post:

```bash
bash scripts/audit-layer-b.sh
cargo test --workspace --features discord
cargo build -p familyclaw-agent --bin continuity_daemon
cargo run -p familyclaw-bench --bin bench -- all          # must print ALL PASSED
cargo run -p familyclaw-bench --bin bench -- compare      # regenerates COMPARISON.md
```

Windows booth path (optional):

```powershell
powershell -File scripts/expo-preflight.ps1
powershell -File scripts/expo-demo.ps1
```

- [ ] Layer B audit green
- [ ] Scorecard 8/8 PASS
- [ ] LangGraph repro steps pinned in `bench-competitors/langgraph/README.md`
- [ ] README "Should you use this?" section accurate
- [ ] No Layer B names in tracked publishable files
- [ ] `main` pushed and CI green on GitHub

---

## One-command proof (put this in every post)

```bash
git clone https://github.com/Sisuthros/familyclaw-oss
cd familyclaw
cargo build -p familyclaw-agent --bin continuity_daemon
cargo run -p familyclaw-bench --bin bench -- all
```

Expected: `# FamilyClaw Continuity Scorecard` with **Overall: PASS** and
`side_effect_overcount = 0` in `s1_crash_matrix`.

**LangGraph head-to-head** (optional second command, requires Python 3.13+):

```bash
cd bench-competitors/langgraph
python -m venv .venv
# Windows: .venv\Scripts\python.exe -m pip install langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0
.venv/bin/python crash_harness.py cycle --crash-point before_write --workdir _runs/bw
```

See [RESULTS.md](../bench-competitors/langgraph/RESULTS.md) for honest caveats.

---

## Show HN — draft

**Title:** Show HN: Rust agent runtime where external side effects survive a crash at most once (benchmark vs LangGraph)

**Body:**

Long-running agents crash. Checkpointers restore *state*; they do not always
prevent a *duplicate external side effect* when a node re-runs after a kill
between "effect fired" and "durable record written."

FamilyClaw is a Rust workspace (MIT, `unsafe` forbidden) built around one narrow,
falsifiable claim: **at-most-once dispatch of external side effects under crash**
(idempotency-keyed outbox, fail-closed in the intent-only window).

We benchmarked against LangGraph 1.2.6 with `durability="sync"` on one metric:
after SIGKILL, how many money-touching side effects re-execute? FamilyClaw: 0 at
every crash point we test; LangGraph re-fired in the `before_write` and
`mid_replay` windows in our harness.

Reproduce:

```bash
git clone https://github.com/Sisuthros/familyclaw-oss
cd familyclaw
cargo build -p familyclaw-agent --bin continuity_daemon
cargo run -p familyclaw-bench --bin bench -- all
```

Honest limits (read before trusting us):

- Not "exactly-once completion" — duplicate-prevention under crash, not universal completion.
- Not a breadth play vs larger agent frameworks — no channel matrix arms race.
- Semantic recall is gated; keyword + provenance + temporal is the supported path unless recall fixtures prove otherwise.

**Should you use this?** If your agent only reads/summarizes, stay in Python. If
it mutates the world and a crash costs money, run the bench.

Looking for one external adopter: `familyclaw serve` in someone else's repo —
that's the adoption gate we care about more than stars.

---

## r/rust — draft

**Title:** FamilyClaw v1.2 — durable agent runtime, `unsafe` forbidden, continuity scorecard 8/8

**Body:**

We shipped a Rust agent runtime focused on continuity under crash:

- Journal-based deterministic replay (`familyclaw-durable`)
- At-most-once external dispatch via idempotency-keyed outbox
- Approval-gated action runtime (`familyclaw-actions`)
- 23 crates, ~1905 tests (see [STATUS.md](../STATUS.md)), MSRV 1.88

The wedge is a reproducible benchmark suite (`cargo run -p familyclaw-bench --bin bench -- all`) including a subprocess crash matrix. We also pinned a comparison vs LangGraph's durable checkpointing on duplicate side effects after kill.

Quick start (no keys):

```bash
cargo run -p familyclaw-agent --example two_agents_memory
FAMILYCLAW_CHANNEL_KIND=none cargo install --path crates/familyclaw-gateway && familyclaw-gateway serve
```

Feedback welcome — especially from anyone running world-mutating agents who has been burned by double-fire on resume.

---

## Adoption gate (track manually)

| # | Who | Date | `familyclaw serve` in their repo? | Notes |
|---|-----|------|-----------------------------------|-------|
| 1 | | | | |

When the first row is filled, Horisontti 2 is won.

---

## Commercial follow-up (Horisontti 3)

After technical credibility: [COMMERCIAL_OFFER.md](COMMERCIAL_OFFER.md) (Reliability Review / Sprint).
See [`commercial/EVALUATOR_DEMO_SCRIPT.md`](commercial/EVALUATOR_DEMO_SCRIPT.md)
for the evaluation-call script.

---

*Last updated: 2026-07-06*
