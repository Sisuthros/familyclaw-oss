# RESULTS — FamilyClaw vs LangGraph (real) vs markdown baseline (control)

> **Status: INTERNAL.** This is a results doc for the operator + GPT-5.5 review, NOT a public
> claim surface. Do not copy these numbers into the public README/COMPARISON.md until
> reviewed. The honesty caveats in §6 are part of the artifact, not a footnote.

**The launch artifact the master-plan calls for (#1):** a real-competitor continuity
benchmark. LangGraph is a genuine, widely-deployed agent-orchestration framework with
durable checkpointing — not a strawman. We give it its *strongest* durability config and
ask the one question that matters for money-touching work: **after a crash, how many
external side effects re-execute?** (target: 0). FamilyClaw is measured on the same
metric via its own S1 crash-matrix and cross-process dispatch red-team. A truncating
markdown-file agent is the control (worst case).

---

## 1. Exact versions + harness config (reproducibility)

A skeptic must be able to re-run this. Everything below is pinned and deterministic.

### LangGraph stack (throwaway venv)

```
langgraph==1.2.6
langgraph-checkpoint==4.1.1
langgraph-checkpoint-sqlite==3.1.0
aiosqlite==0.22.1
langchain-core==1.4.8
Python 3.13.5
```

Full `pip freeze` is in `requirements.lock.txt`. The version **pins are unchanged** since
install; the only drift is `pip` itself (now `26.1.2` in the venv; the build summary
recorded `25.2` "at install"). **pip's version does not affect the deterministic graph** —
the package pins are identical and the graph runs no LLM, so every run is byte-reproducible.

### Agent / harness config

- **Graph**: a 4-node *linear* `StateGraph` (`step-0 → step-1 → step-2 → step-3`),
  compiled with a `SqliteSaver` checkpointer.
- **Durability**: `graph.invoke(..., durability="sync")` — **LangGraph's STRONGEST
  durability mode**: a node's writes are persisted synchronously *after it returns* and
  *before the next step starts*. (`"async"` and `"exit"` persist later, so they can only
  match or worsen this result. We did not cripple the checkpointer.)
- **No LLM, no API key**: a deterministic scripted graph → identical result every run.
- **Side effect**: each node bumps an on-disk counter (`side_effect_counter.txt`) — a
  stand-in for a "money-touching" external mutation. The bump lives **inside the node
  body**, exactly like FamilyClaw's `CountingExecutor.execute`.
- **Crash**: a cross-process SIGKILL — `os._exit(137)` fires inside the armed node
  **AFTER** the external side effect (counter bump) but **BEFORE** the node returns
  (i.e. before its post-node checkpoint). A **fresh child process** then resumes via
  `graph.invoke(None, config)` from the same `thread_id`.

### FamilyClaw counterparts (the apples-to-apples mapping)

| FamilyClaw S1 / dispatch red-team | LangGraph competitor |
|---|---|
| Fixed multi-step workload (`TASK_STEPS = 5`) | Fixed linear graph (4 nodes) |
| `CountingExecutor` bumps on-disk counter per `execute` | step node bumps on-disk counter per node run |
| `side_effect_overcount` (target 0) | `counter_final - num_nodes` |
| Durable journal + idempotency-keyed outbox (`submit_task_idempotent`) | `SqliteSaver`, `durability="sync"` |
| Cross-process SIGKILL: child `process::exit(137)` | child `os._exit(137)` inside the node |
| `CrashPoint::BeforeWrite` (effect done, journal not yet) | `before_write` (effect done, checkpoint not yet) |
| `CrashPoint::MidReplay` (re-crash during resume) | `mid_replay` |
| `CrashPoint::Clean` (baseline) | `clean` |

---

## 2. Comparison table — side-effect OVERCOUNT (raw numbers)

Overcount = (external side effects that actually fired) − (side effects that *should*
have fired exactly once). **Lower is better; 0 is correct.**

| Crash point | FamilyClaw | LangGraph (`durability="sync"`) | Markdown baseline (control) |
|---|:--:|:--:|:--:|
| `clean` — no crash (baseline) | **0** | **0** | **0** |
| `before_write` — effect committed externally, durable record not yet written | **0** | **1** | **4** |
| `mid_replay` — re-crash *during* the resume/replay itself | **0** | **2** | **5** |

**FamilyClaw wins** (0 overcount at every crash point). LangGraph is correct only on the
clean path and re-fires on both adversarial crash points. The markdown baseline is worst.

### Raw evidence behind the table (verified live, fresh re-run 2026-06-19 — not copied from the build summary)

**LangGraph crash harness** (venv `<VENV_DIR>`,
dir `E:\Familyclaw\bench-competitors\langgraph`):

```
clean        : run_exit_code=0,   side_effect_count_final=4, side_effect_overcount=0, exactly_once=true
before_write : run_exit_code=137, side_effect_count_final=5, side_effect_overcount=1, exactly_once=false
mid_replay   : run_exit_code=137, side_effect_count_final=6, side_effect_overcount=2, exactly_once=false
```

Raw on-disk counters (independent of the harness's own JSON):
`clean=4`, `before_write=5`, `mid_replay=6`.

`before_write` RESTART_REPORT (verbatim — proves the resume genuinely read state from disk):

```json
{"checkpoint_existed": true, "next_before_resume": ["step-1"],
 "completed_before_resume": ["step-0"],
 "completed_after_resume": ["step-0","step-1","step-2","step-3"],
 "side_effect_count": 5}
```

**Reproducibility:** a 2nd independent `before_write` cycle gave the identical
`run_exit_code=137` / overcount=1 / raw counter=5 (`_runs/before_write_rerun`,
`_runs/repro_bw` both show counter=5).

**FamilyClaw exactly-once cross-process dispatch proof**
(`cargo test -p familyclaw-actions --test redteam_dispatch_exactly_once` → 6 passed, 0 failed):

```
OLD path  (submit_task_as, no outbox)             -> side_effect_count = 2   (THE BUG: double-fire)
NEW path  (submit_task_idempotent, committed win) -> side_effect_count = 1, value_identical = true
NEW path  intent-only window (turn-* key)         -> side_effect_count = 1, policy_denied = true (fail-closed)
approval path intent-only (approval-{id} key)     -> side_effect_count = 1, policy_denied = true
approval path committed window                    -> side_effect_count = 1, value_identical = true
```

All armed crashes exited `137` (SIGKILL-style) across a real process boundary.

**FamilyClaw S1 crash matrix** (`cargo run -p familyclaw-bench -- s1` / `-- compare`,
5-step workload, injected clock `2026-06-04T12:00:00Z`):

```
FamilyClaw   : side_effect_overcount = 0.0, resume_correctness = 1.0, result PASS
  BeforeWrite reexec=0; MidWrite reexec=0; MidReplay reexec=0;
  CorruptedJournal = loud refusal (correct — refuses rather than silently re-running)

Markdown baseline : side_effect_overcount = 17.0 total, resume_correctness = 0.0, result FAIL
  (BeforeWrite/MidWrite/CorruptedJournal each re-run 4 of 5 completed steps,
   MidReplay re-runs all 5  ->  4+4+5+4 = 17)
```

> Note on the two baseline columns: the §2 table reports the markdown baseline at the
> *per-crash-point* granularity used for the LangGraph head-to-head (4 and 5 for
> `before_write`/`mid_replay`). The S1 figure of **17 total** is the sum over all four
> S1 adversarial crash points — a different aggregation, both honest, both shown so a
> reviewer can see exactly where each number comes from.

---

## 3. The honest verdict — where FamilyClaw actually wins, and where it ties

**FamilyClaw wins, honestly and reproducibly — and the headline is the plausible,
confirmed one, not an inflated one.**

### Where they TIE (state durably, in LangGraph's favor)

LangGraph's `SqliteSaver` gives **durable STATE replay**, and it genuinely works. After a
crash it correctly resumes from the last checkpoint: `next=["step-1"]`,
`completed=["step-0"]`, then runs the rest. This is a real, valuable guarantee and we do
not dispute it. A crash placed **strictly BETWEEN nodes** would *not* re-fire the side
effect in LangGraph either. On that ground the two systems tie.

### Where FamilyClaw WINS (the wedge — and it is a real one)

FamilyClaw wins precisely at the **after-side-effect / before-durable-record** crash
window: exactly-once / at-most-once **EXTERNAL side-effect dispatch** across a process
crash.

LangGraph's checkpoint barrier sits **BETWEEN** nodes, so the external side effect and the
post-node "node done" checkpoint are **NOT atomic**. A SIGKILL after the side effect but
before that checkpoint leaves the node still marked `next`, so resume **RE-RUNS** the node
and **RE-FIRES** the side effect (overcount 1; compounds to 2 under `mid_replay`). With its
**strongest** durability mode, LangGraph showed `exactly_once = false`.

FamilyClaw closes exactly this gap with an **idempotency-keyed intent→effect→committed
outbox**:

- A re-drive after a **committed-window** crash returns the **value-identical committed
  outcome without re-running** (counter stays at exactly 1).
- A crash in the narrow **intent-only window** fails **CLOSED** with `PolicyDenied`
  (at-most-once) rather than re-firing.

So: **durable STATE replay (LangGraph has it, it works) is a DIFFERENT guarantee from
exactly-once EXTERNAL SIDE EFFECTS (which requires the outbox FamilyClaw implements).**
The wedge is not a strawman — durable state replay is real and we credit it; the
differentiation is the side-effect-atomicity layer on top of it.

### The master-plan-honest phrasing of the claim

> **FamilyClaw provides at-most-once / duplicate-prevented DISPATCH of money-touching
> external side effects across a process crash — not magical exactly-once completion.**

It prevents a *duplicate fire* in the dangerous window. It does **not** claim to magically
complete a half-finished effect, nor to guarantee universal exactly-once *completion* of
arbitrary work. The guarantee is duplicate-prevention-under-crash for the
effect→durable-record window, which is exactly where money-touching idempotency must hold.

---

## 4. How to reproduce

### LangGraph competitor (Python)

```bash
VENV=<VENV_DIR>/Scripts/python.exe
cd E:/Familyclaw/bench-competitors/langgraph

"$VENV" crash_harness.py cycle --crash-point clean        --workdir _runs/clean
"$VENV" crash_harness.py cycle --crash-point before_write --workdir _runs/before_write
"$VENV" crash_harness.py cycle --crash-point mid_replay   --workdir _runs/mid_replay

# raw proof, independent of the harness JSON:
cat _runs/clean/side_effect_counter.txt          # -> 4
cat _runs/before_write/side_effect_counter.txt   # -> 5  (overcount 1)
cat _runs/mid_replay/side_effect_counter.txt     # -> 6  (overcount 2)
```

Recreate the venv from scratch if needed:

```bash
PY=python   # or the full path to a Python 3.13 interpreter (3.13.5)
"$PY" -m venv <VENV_DIR>
<VENV_DIR>/Scripts/python.exe -m pip install \
  langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0
```

### FamilyClaw side (Rust, from `E:\Familyclaw`)

```bash
cargo test -p familyclaw-actions --test redteam_dispatch_exactly_once   # 6 passed
cargo run  -p familyclaw-bench -- s1                                     # FamilyClaw overcount 0, PASS
cargo run  -p familyclaw-bench -- compare                               # FamilyClaw vs markdown baseline
```

---

## 5. Isolation (hard rule satisfied)

- `cargo metadata`: 21 workspace packages, **none competitor-related**. There is **no
  `Cargo.toml` anywhere under `bench-competitors/`** — the competitor is a standalone
  Python harness, fully outside the Rust tree.
- `cargo build --workspace` → **EXIT 0** (green, untouched).
- Private-identity scan (the Layer-B forbidden-name set, case-insensitive) over the
  competitor dir: **CLEAN** (no matches). No secrets, no API keys.
- No Rust `LangGraphSubject` was wired into FamilyClaw's tree. **Follow-up:** if one is
  added later (shelling out to this harness), it MUST live behind a feature flag /
  separate test bin so the default `build`/`test`/`clippy` gates stay green.

---

## 6. What this DOESN'T claim (honesty-as-product-asset)

The honesty here is a feature, not a disclaimer. Overstating the win would make the wedge
easy to knock down; understating it precisely makes it durable.

1. **NOT "LangGraph is broken."** LangGraph's durable checkpointing genuinely works for
   STATE replay. It resumes from the correct checkpoint. We credit that.
2. **NOT "exactly-once completion of arbitrary work."** FamilyClaw guarantees
   duplicate-*prevention* of the external dispatch in the crash window — at-most-once /
   committed-replay — not that any half-done effect magically finishes.
3. **NOT a win in the between-nodes window.** A crash strictly between nodes would not
   re-fire in LangGraph either. The wedge is specifically the **intra-node** "effect done,
   durable record not yet" window. We say so explicitly rather than implying universal
   superiority.
4. **NOT a throughput / latency / cost / ergonomics benchmark.** This measures ONE thing:
   side-effect re-execution under crash. LangGraph wins on ecosystem, tooling, and breadth;
   that is not what this artifact contests.
5. **NOT an LLM-quality comparison.** No LLM runs in either harness — by design, so the
   result is deterministic and reproducible.
6. **The crash window is the realistic one, and LangGraph got its best config.** The side
   effect lives inside the node body in BOTH systems, and LangGraph ran with
   `durability="sync"` (its strongest). It was not crippled to manufacture a loss.

**Bottom line:** durable STATE replay (LangGraph has it) ≠ exactly-once EXTERNAL SIDE
EFFECTS (FamilyClaw's intent→effect→committed outbox). FamilyClaw wins at the
money-touching idempotency layer, at-most-once dispatch — stated as exactly that, no more.
