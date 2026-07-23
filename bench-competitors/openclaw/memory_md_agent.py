"""OpenClaw-shaped crash harness agent — MEMORY.md + restart re-runs side effects.

This models the *documented* OpenClaw failure mode for the S1 continuity metric:
file-based memory with no idempotency-keyed external dispatch. On restart the
workload re-runs from scratch, so side effects that already fired re-fire.

When ``OPENCLAW_BIN`` is set, ``crash_harness.py`` shells out to that binary
instead of this in-process model (see README). Without it, this stub is the
reproducible, no-network competitor Subject used in CI and evaluator demos.

No LLM. No API key. Deterministic.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

NUM_STEPS = 4
CRASH_AT_ENV = "OCBENCH_CRASH_AT_STEP"
STATE_FILE = "task_state.json"
MEMORY_FILE = "MEMORY.md"
BOOTSTRAP_BUDGET = 8


def _bump(counter_path: str) -> int:
    p = Path(counter_path)
    try:
        current = int(p.read_text(encoding="utf-8").strip())
    except (FileNotFoundError, ValueError):
        current = 0
    new_val = current + 1
    p.write_text(str(new_val), encoding="utf-8")
    return new_val


def _crash_at() -> int:
    raw = os.environ.get(CRASH_AT_ENV, "").strip()
    if not raw:
        return -1
    try:
        return int(raw)
    except ValueError:
        return -1


def _load_state(workdir: Path) -> dict:
    path = workdir / STATE_FILE
    if not path.exists():
        return {"completed": [], "memory": []}
    return json.loads(path.read_text(encoding="utf-8"))


def _save_state(workdir: Path, state: dict) -> None:
    (workdir / STATE_FILE).write_text(json.dumps(state), encoding="utf-8")
    # MEMORY.md truncation model (oldest-first when over budget).
    lines = list(state.get("memory", []))
    while len(lines) > BOOTSTRAP_BUDGET:
        lines.pop(0)
    (workdir / MEMORY_FILE).write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")
    state["memory"] = lines


def run_workload(workdir: str, counter: str, resume: bool) -> dict:
    """Run or re-run the fixed 4-step workload.

    OpenClaw-shaped behavior: there is **no durable 'step done' record that
    suppresses re-dispatch**. Even on ``resume=True`` we re-execute every step
    that is not already listed — and after a crash mid-step the completed list
    never received that step, so the side effect re-fires.
    """
    wd = Path(workdir)
    wd.mkdir(parents=True, exist_ok=True)
    state = _load_state(wd) if resume else {"completed": [], "memory": []}
    if not resume:
        Path(counter).write_text("0", encoding="utf-8")

    crash_at = _crash_at()
    for step in range(NUM_STEPS):
        label = f"step-{step}"
        if label in state["completed"]:
            continue
        _bump(counter)
        state["memory"].append(f"did {label}")
        if crash_at == step:
            # Crash AFTER side effect, BEFORE durable "completed" write.
            _save_state(wd, state)  # memory may flush; completed does not include this step
            os._exit(137)
        state["completed"].append(label)
        _save_state(wd, state)

    return {
        "completed": state["completed"],
        "side_effect_count": int(Path(counter).read_text(encoding="utf-8").strip()),
        "mode": "openclaw-shaped-memory-md",
    }
