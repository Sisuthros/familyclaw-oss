"""LangGraph durable-checkpointing agent — the real competitor for the S1 continuity benchmark.

This mirrors FamilyClaw's S1 crash-matrix workload apples-to-apples:

  * A FIXED multi-step workload. Each step performs an EXTERNAL, money-touching-like
    side effect: it increments a counter held in an on-disk file. The side effect MUST
    happen exactly once across a crash + resume cycle (overcount target = 0).
  * Durable state via LangGraph's strongest durability story: a `SqliteSaver`
    checkpointer persisting graph state to a sqlite file, keyed by `thread_id`.
  * A DETERMINISTIC scripted graph. NO LLM call, NO API key — fully reproducible.

The benchmark question (identical to what FamilyClaw's `CountingExecutor` measures):

  When the process crashes AFTER a node's external side effect has fired but BEFORE
  the checkpoint that records "this node is done" is durably written, does LangGraph
  RE-EXECUTE that node's side effect on resume (overcount > 0)?

FamilyClaw's idempotency-keyed dispatch (`submit_task_idempotent`: intent ->
side effect -> committed) keeps that count at exactly 1 per step. This harness reports
LangGraph's ACTUAL behavior under the same crash window.

Honesty notes
-------------
* The side effect lives INSIDE the node body, exactly like FamilyClaw's executor.
* LangGraph's checkpoint barrier sits BETWEEN nodes (`durability="sync"` persists a
  node's writes AFTER it returns, before the next node starts). The side effect and
  that checkpoint write are therefore NOT atomic. The crash is injected at the one
  instant that exposes this gap: right after the counter bump, before the node returns
  -- so LangGraph has not yet written the "node done" checkpoint. This is the SAME
  intent-only / committed-window gap FamilyClaw's outbox closes. It is NOT rigging:
  it is the realistic failure window any external-side-effect node faces.
* `durability="sync"` is LangGraph's MOST durable mode (persist before next step).
  We use it so the competitor is shown at its strongest, not weakest.
"""

from __future__ import annotations

import os
import sqlite3
from pathlib import Path
from typing import Annotated, TypedDict

from langgraph.checkpoint.sqlite import SqliteSaver
from langgraph.graph import END, START, StateGraph


# Number of steps in the fixed workload. Kept small but > 1 so "resume from the next
# step" is a meaningful claim (FamilyClaw uses 5; we use 4 distinct side-effect nodes).
NUM_STEPS = 4

# Env var that arms the crash: "<step_index>" => os._exit(137) inside that step's node,
# AFTER the side effect fires, BEFORE the node returns (so before the post-node
# checkpoint is written). Unset / empty => no crash (clean baseline run).
CRASH_AT_ENV = "LGBENCH_CRASH_AT_STEP"


class WorkState(TypedDict):
    """Graph state. `completed` accumulates step labels via an additive reducer so the
    checkpoint records progress; `counter_path` carries the on-disk side-effect target."""

    completed: Annotated[list[str], lambda a, b: a + b]
    counter_path: str


def _bump_disk_counter(counter_path: str) -> int:
    """The EXTERNAL side effect: read -> +1 -> write an on-disk counter.

    Identical in spirit to FamilyClaw's CountingExecutor.bump_disk_counter: this is the
    "money-touching" external mutation that must happen exactly once across a crash.
    Returns the new value (diagnostic only; the disk file is the real proof).
    """
    p = Path(counter_path)
    try:
        current = int(p.read_text().strip())
    except (FileNotFoundError, ValueError):
        current = 0
    new_val = current + 1
    p.write_text(str(new_val))
    return new_val


def _make_step_node(step_index: int):
    """Build node `step_index`: fire the external side effect, then maybe crash.

    The crash (`os._exit(137)`) happens AFTER `_bump_disk_counter` but BEFORE this
    function returns its state update -- i.e. before LangGraph persists the post-node
    checkpoint. That is the exact 'side effect committed, durable record not yet
    written' window FamilyClaw's S1 BeforeWrite/intent-only point models.
    """

    label = f"step-{step_index}"

    def node(state: WorkState) -> dict:
        # --- EXTERNAL SIDE EFFECT (must be exactly-once across crash+resume) ---
        _bump_disk_counter(state["counter_path"])

        # --- CRASH INJECTION: SIGKILL-equivalent, after side effect, before return ---
        crash_at = os_environ_get_crash_step()
        if crash_at == step_index:
            # os._exit bypasses cleanup/atexit/buffer flush == hard kill semantics.
            # At this point the side effect is on disk but the node has NOT returned,
            # so LangGraph has NOT written this step's checkpoint.
            os._exit(137)

        return {"completed": [label]}

    node.__name__ = f"node_{label}"
    return node


def os_environ_get_crash_step() -> int:
    """Parse the armed crash step from the environment (-1 == disarmed)."""
    raw = os.environ.get(CRASH_AT_ENV, "").strip()
    if raw == "":
        return -1
    try:
        return int(raw)
    except ValueError:
        return -1


def build_graph(checkpointer: SqliteSaver):
    """Assemble the deterministic linear graph: START -> step-0 -> ... -> step-N -> END.

    Each step node is a distinct graph node, so LangGraph checkpoints BETWEEN them.
    Compiled with the supplied SqliteSaver so state is durable per `thread_id`.
    """
    graph = StateGraph(WorkState)
    prev = START
    for i in range(NUM_STEPS):
        name = f"step-{i}"
        graph.add_node(name, _make_step_node(i))
        graph.add_edge(prev, name)
        prev = name
    graph.add_edge(prev, END)
    return graph.compile(checkpointer=checkpointer)


def open_saver(db_path: str) -> SqliteSaver:
    """Open a durable SqliteSaver on `db_path` (its own connection, WAL-friendly)."""
    conn = sqlite3.connect(db_path, check_same_thread=False)
    saver = SqliteSaver(conn)
    saver.setup()  # create checkpoint tables if missing
    return saver
