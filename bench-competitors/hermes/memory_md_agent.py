"""Hermes Agent-shaped crash harness agent — MEMORY.md ~2.2k char budget + re-run.

Models the documented Hermes-style hard character limit on file memory and the
absence of idempotency-keyed external dispatch. On restart, incomplete steps
re-execute (side effects re-fire).

When ``HERMES_BIN`` is set, the harness notes live mode (see README) but the
numeric matrix remains this shaped model until a pinned live Subject adapter
exists.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

NUM_STEPS = 4
CRASH_AT_ENV = "HMBENCH_CRASH_AT_STEP"
STATE_FILE = "task_state.json"
MEMORY_FILE = "MEMORY.md"
CHAR_BUDGET = 2200


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


def _trim_memory(lines: list[str]) -> list[str]:
    while lines and sum(len(x) + 1 for x in lines) > CHAR_BUDGET:
        lines.pop(0)
    return lines


def _load_state(workdir: Path) -> dict:
    path = workdir / STATE_FILE
    if not path.exists():
        return {"completed": [], "memory": []}
    return json.loads(path.read_text(encoding="utf-8"))


def _save_state(workdir: Path, state: dict) -> None:
    lines = _trim_memory(list(state.get("memory", [])))
    state["memory"] = lines
    (workdir / STATE_FILE).write_text(json.dumps(state), encoding="utf-8")
    (workdir / MEMORY_FILE).write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")


def run_workload(workdir: str, counter: str, resume: bool) -> dict:
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
            _save_state(wd, state)
            os._exit(137)
        state["completed"].append(label)
        _save_state(wd, state)

    return {
        "completed": state["completed"],
        "side_effect_count": int(Path(counter).read_text(encoding="utf-8").strip()),
        "mode": "hermes-shaped-memory-2k",
    }
