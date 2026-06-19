"""Cross-process SIGKILL crash harness for the LangGraph competitor.

Mirrors FamilyClaw's `dispatch_redteam` lifecycle (prepare -> crash_run -> restart):

  1. RUN child process: execute the durable graph for `thread_id`. Inside the armed
     step's node, AFTER the external side effect fires, the process calls
     os._exit(137) -- a SIGKILL-equivalent across a real process boundary. The
     post-node checkpoint for that step is therefore never written.
  2. RESTART: a FRESH process opens the SAME SqliteSaver db + SAME thread_id and
     resumes the graph (invoke with input=None continues from the last checkpoint).
  3. COUNT: read the on-disk side-effect counter raw. overcount = total - NUM_STEPS.

Crash points (selectable, mapped onto FamilyClaw's CrashPoint):
  --crash-point before_write : crash inside step-1 (after its side effect, before its
       checkpoint). This is FamilyClaw's BeforeWrite / intent-only window -- the exact
       gap idempotency-keyed dispatch closes.
  --crash-point mid_replay  : crash again DURING the resume (inside a later step while
       replaying forward) to prove the resume itself is also crash-safe (FamilyClaw's
       MidReplay).
  --crash-point clean       : no crash -- the baseline run for comparison.

Usage
-----
  # full crash+resume cycle in one driver process (spawns child, then resumes):
  python crash_harness.py cycle --crash-point before_write --workdir <dir>

  # low-level single phase (used internally / for manual inspection):
  python crash_harness.py run     --thread-id T --db <db> --counter <file> [--crash-at N]
  python crash_harness.py restart --thread-id T --db <db> --counter <file> [--crash-at N]

Determinism: no system clock is read; the workload is a fixed scripted graph. Same
inputs -> identical result every run.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

# Import the agent module (same directory).
sys.path.insert(0, str(Path(__file__).resolve().parent))
import langgraph_agent as agent  # noqa: E402


THIS = str(Path(__file__).resolve())


# --- Mapping competitor crash points onto FamilyClaw's CrashPoint vocabulary ---
# We crash inside step-1 (a middle step) so "resume from the next step" is meaningful.
CRASH_POINTS = {
    # FamilyClaw BeforeWrite / intent-only: side effect fired, durable record not yet.
    "before_write": {"run_crash_at": 1, "restart_crash_at": -1},
    # FamilyClaw MidReplay: crash again during the resume itself (inside a later step).
    "mid_replay": {"run_crash_at": 1, "restart_crash_at": 2},
    # FamilyClaw Clean: no crash; pure baseline.
    "clean": {"run_crash_at": -1, "restart_crash_at": -1},
}


def _read_counter(counter_path: str) -> int:
    try:
        return int(Path(counter_path).read_text().strip())
    except (FileNotFoundError, ValueError):
        return 0


def phase_run(thread_id: str, db: str, counter: str, crash_at: int) -> int:
    """Phase 1: start the graph fresh for thread_id. May os._exit(137) inside a node."""
    import os

    if crash_at >= 0:
        os.environ[agent.CRASH_AT_ENV] = str(crash_at)
    else:
        os.environ.pop(agent.CRASH_AT_ENV, None)

    saver = agent.open_saver(db)
    graph = agent.build_graph(saver)
    config = {"configurable": {"thread_id": thread_id}}
    initial = {"completed": [], "counter_path": counter}
    # durability="sync" == LangGraph's strongest: persist a node's writes before the
    # next step starts. If the process survives, state is fully checkpointed.
    final = graph.invoke(initial, config, durability="sync")
    print("RUN_COMPLETED " + json.dumps({"completed": final["completed"]}))
    return 0


def phase_restart(thread_id: str, db: str, counter: str, crash_at: int) -> int:
    """Phase 2: FRESH process resumes from the durable checkpoint for thread_id.

    invoke(None, config) continues the graph from its last persisted checkpoint.
    """
    import os

    if crash_at >= 0:
        os.environ[agent.CRASH_AT_ENV] = str(crash_at)
    else:
        os.environ.pop(agent.CRASH_AT_ENV, None)

    saver = agent.open_saver(db)
    graph = agent.build_graph(saver)
    config = {"configurable": {"thread_id": thread_id}}

    state = graph.get_state(config)
    next_nodes = list(state.next) if state else []
    resumed_completed = state.values.get("completed", []) if state and state.values else []

    final = graph.invoke(None, config, durability="sync")
    report = {
        "checkpoint_existed": state is not None and bool(state.values),
        "next_before_resume": next_nodes,
        "completed_before_resume": resumed_completed,
        "completed_after_resume": final["completed"],
        "side_effect_count": _read_counter(counter),
    }
    print("RESTART_REPORT " + json.dumps(report))
    return 0


def drive_cycle(crash_point: str, workdir: str) -> int:
    """Top-level driver: run (child, may crash) then restart (fresh child), then report.

    This is the apples-to-apples S1 cycle: fixed workload, crash at a point, fresh
    process resumes from durable checkpoint, count external side effects on disk.
    """
    if crash_point not in CRASH_POINTS:
        print(f"unknown crash-point {crash_point!r}; choose from {list(CRASH_POINTS)}",
              file=sys.stderr)
        return 2

    wd = Path(workdir)
    wd.mkdir(parents=True, exist_ok=True)
    db = str(wd / "checkpoints.sqlite")
    counter = str(wd / "side_effect_counter.txt")
    thread_id = f"s1-{crash_point}"

    # Clean slate so the count is unambiguous.
    for f in (db, counter, db + "-wal", db + "-shm"):
        Path(f).unlink(missing_ok=True)
    Path(counter).write_text("0")

    cp = CRASH_POINTS[crash_point]
    run_crash_at = cp["run_crash_at"]
    restart_crash_at = cp["restart_crash_at"]

    # --- Phase 1: RUN (separate process so the crash is a real process death) ---
    run_proc = subprocess.run(
        [sys.executable, THIS, "run",
         "--thread-id", thread_id, "--db", db, "--counter", counter,
         "--crash-at", str(run_crash_at)],
        capture_output=True, text=True,
    )
    counter_after_crash = _read_counter(counter)

    # --- Phase 2 (optional re-crash during replay): MidReplay re-crashes once ---
    restart_attempts = []
    if restart_crash_at >= 0:
        first_restart = subprocess.run(
            [sys.executable, THIS, "restart",
             "--thread-id", thread_id, "--db", db, "--counter", counter,
             "--crash-at", str(restart_crash_at)],
            capture_output=True, text=True,
        )
        restart_attempts.append({
            "phase": "restart-1 (re-crash during replay)",
            "exit_code": first_restart.returncode,
            "stdout": first_restart.stdout.strip(),
            "counter_after": _read_counter(counter),
        })

    # --- Phase 2/3: RESTART to completion (fresh process, no crash armed) ---
    final_restart = subprocess.run(
        [sys.executable, THIS, "restart",
         "--thread-id", thread_id, "--db", db, "--counter", counter,
         "--crash-at", "-1"],
        capture_output=True, text=True,
    )

    counter_final = _read_counter(counter)
    overcount = counter_final - agent.NUM_STEPS

    report = {
        "crash_point": crash_point,
        "num_steps": agent.NUM_STEPS,
        "run_exit_code": run_proc.returncode,
        "counter_after_crash": counter_after_crash,
        "restart_attempts": restart_attempts,
        "final_restart_exit_code": final_restart.returncode,
        "final_restart_stdout": final_restart.stdout.strip(),
        "side_effect_count_final": counter_final,
        "side_effect_overcount": overcount,
        "exactly_once": overcount == 0,
        "db": db,
        "counter_file": counter,
    }
    print("CYCLE_REPORT " + json.dumps(report, indent=2))
    if run_proc.stdout.strip():
        print("--- run stdout ---\n" + run_proc.stdout.strip(), file=sys.stderr)
    if run_proc.stderr.strip():
        print("--- run stderr ---\n" + run_proc.stderr.strip(), file=sys.stderr)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="LangGraph S1 crash harness")
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_cycle = sub.add_parser("cycle", help="full crash+resume cycle with report")
    p_cycle.add_argument("--crash-point", required=True, choices=list(CRASH_POINTS))
    p_cycle.add_argument("--workdir", required=True)

    for name in ("run", "restart"):
        p = sub.add_parser(name)
        p.add_argument("--thread-id", required=True)
        p.add_argument("--db", required=True)
        p.add_argument("--counter", required=True)
        p.add_argument("--crash-at", type=int, default=-1)

    args = parser.parse_args()
    if args.cmd == "cycle":
        return drive_cycle(args.crash_point, args.workdir)
    if args.cmd == "run":
        return phase_run(args.thread_id, args.db, args.counter, args.crash_at)
    if args.cmd == "restart":
        return phase_restart(args.thread_id, args.db, args.counter, args.crash_at)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
