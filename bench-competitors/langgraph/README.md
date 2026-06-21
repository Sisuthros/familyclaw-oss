# LangGraph competitor harness (S1 continuity benchmark)

Standalone, reproducible benchmark pitting LangGraph's durable checkpointing against
FamilyClaw's S1 crash-matrix contract on the metric that matters for money-touching
work: **how many external side effects re-execute after a crash** (target 0).

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
- `requirements.lock.txt` — full `pip freeze` of the throwaway venv.

## Quick start

```bash
VENV=$VENV_DIR/Scripts/python.exe
cd E:/Familyclaw/bench-competitors/langgraph
"$VENV" crash_harness.py cycle --crash-point before_write --workdir _runs/before_write
cat _runs/before_write/side_effect_counter.txt   # raw side-effect count
```

If recreating the venv from scratch:

```bash
PY=python   # or the full path to a Python 3.13 interpreter (3.13.5, pip 25.2)
"$PY" -m venv $VENV_DIR
$VENV_DIR/Scripts/python.exe -m pip install \
  langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0
```

## Result in one line

Under a SIGKILL after a node's external side effect but before its checkpoint,
LangGraph (`SqliteSaver`, `durability="sync"`) **re-executes the side effect**:
`before_write` overcount = **1**, `mid_replay` overcount = **2**, `clean` overcount = 0.
FamilyClaw's idempotency-keyed dispatch keeps the same metric at **0**. See `RESULTS.md`.
