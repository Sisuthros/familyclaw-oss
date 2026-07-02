# LangGraph competitor harness — crash-safety benchmark (S1 continuity)

Standalone, reproducible benchmark pitting LangGraph's durable checkpointing against
FamilyClaw's S1 crash-matrix contract on the metric that matters for money-touching
work: **how many external side effects re-execute after a process crash?** (target: 0).

LangGraph is a genuine, widely-deployed agent-orchestration framework with durable
checkpointing — not a strawman. We give it its **strongest** durability config
(`durability="sync"`) and measure the one narrow window where the two systems differ:
a SIGKILL *after* a node's external side effect fires but *before* its checkpoint is
written. See [`RESULTS.md`](RESULTS.md) for the full design, mapping, and honesty caveats.

## Files

- `langgraph_agent.py` — deterministic LangGraph `StateGraph` (4 steps, NO LLM). Each
  step node performs an external side effect (increment an on-disk counter), compiled
  with a `SqliteSaver` checkpointer. A node can `os._exit(137)` right after its side
  effect (before its checkpoint) when armed via `LGBENCH_CRASH_AT_STEP`.
- `crash_harness.py` — cross-process SIGKILL driver: spawns a child to run the graph
  (crashes inside a node), then a FRESH process resumes from the checkpoint for the
  same `thread_id`, then counts on-disk side effects. Crash points: `clean`,
  `before_write`, `mid_replay`.
- `RESULTS.md` — design, apples-to-apples mapping, raw results, and the honest verdict.
- `requirements.lock.txt` — full `pip freeze` of the pinned venv (byte-reproducible).

## Reproduce it (one clone, one venv, one command)

Requires Python 3.13 (tested on 3.13.5). No API key, no LLM, no network at run time —
the graph is a fixed deterministic script, so every run is byte-reproducible.

```bash
git clone https://github.com/<your-org>/familyclaw.git
cd familyclaw/bench-competitors/langgraph

# 1. Create a throwaway venv and install the two pinned packages.
python -m venv .venv          # use a Python 3.13 interpreter
.venv/Scripts/python.exe -m pip install \
  langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0
# (Linux/macOS: .venv/bin/python instead of .venv/Scripts/python.exe)

# 2. Run all three crash points.
VENV=.venv/Scripts/python.exe
"$VENV" crash_harness.py cycle --crash-point clean        --workdir _runs/clean
"$VENV" crash_harness.py cycle --crash-point before_write --workdir _runs/before_write
"$VENV" crash_harness.py cycle --crash-point mid_replay   --workdir _runs/mid_replay

# 3. Read the raw on-disk side-effect counters (independent of the harness JSON).
cat _runs/clean/side_effect_counter.txt          # -> 4  (overcount 0, correct)
cat _runs/before_write/side_effect_counter.txt   # -> 5  (overcount 1, re-fired)
cat _runs/mid_replay/side_effect_counter.txt     # -> 6  (overcount 2, re-fired twice)
```

The `_runs/` directory is git-ignored — it is regenerated on every run.

### FamilyClaw side of the same metric (Rust, from the repo root)

```bash
cargo test -p familyclaw-actions --test redteam_dispatch_exactly_once   # 6 passed
cargo run  -p familyclaw-bench -- s1                                     # overcount 0, PASS
cargo run  -p familyclaw-bench -- compare                               # FamilyClaw vs markdown baseline
```

## Result in one line

Under a SIGKILL after a node's external side effect but before its checkpoint,
LangGraph (`SqliteSaver`, `durability="sync"`, its strongest mode) **re-executes the
side effect**: `before_write` overcount = **1**, `mid_replay` overcount = **2**,
`clean` overcount = 0. FamilyClaw's idempotency-keyed dispatch keeps the same metric at
**0** at every crash point.

The narrow, honest claim: **at-most-once / duplicate-prevented dispatch of a
money-touching external side effect across a process crash** — *not* "LangGraph is
broken" and *not* "magical exactly-once completion". See `RESULTS.md` §3 and §6 for
exactly where FamilyClaw wins and where the two systems tie.
