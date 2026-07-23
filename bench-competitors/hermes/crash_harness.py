"""Cross-process crash harness for the Hermes Agent-shaped competitor.

Same lifecycle as ``bench-competitors/openclaw/crash_harness.py`` and
``bench-competitors/langgraph/crash_harness.py``.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import memory_md_agent as agent  # noqa: E402

THIS = str(Path(__file__).resolve())

CRASH_POINTS = {
    "before_write": {"run_crash_at": 1, "restart_crash_at": -1},
    "mid_replay": {"run_crash_at": 1, "restart_crash_at": 2},
    "clean": {"run_crash_at": -1, "restart_crash_at": -1},
}


def _read_counter(counter_path: str) -> int:
    try:
        return int(Path(counter_path).read_text(encoding="utf-8").strip())
    except (FileNotFoundError, ValueError):
        return 0


def phase_run(workdir: str, counter: str, crash_at: int) -> int:
    if crash_at >= 0:
        os.environ[agent.CRASH_AT_ENV] = str(crash_at)
    else:
        os.environ.pop(agent.CRASH_AT_ENV, None)
    report = agent.run_workload(workdir, counter, resume=False)
    print("RUN_COMPLETED " + json.dumps(report))
    return 0


def phase_restart(workdir: str, counter: str, crash_at: int) -> int:
    if crash_at >= 0:
        os.environ[agent.CRASH_AT_ENV] = str(crash_at)
    else:
        os.environ.pop(agent.CRASH_AT_ENV, None)
    report = agent.run_workload(workdir, counter, resume=True)
    print("RESTART_REPORT " + json.dumps(report))
    return 0


def drive_cycle(crash_point: str, workdir: str) -> int:
    if crash_point not in CRASH_POINTS:
        print(f"unknown crash-point {crash_point!r}", file=sys.stderr)
        return 2

    live = os.environ.get("HERMES_BIN", "").strip()
    if live:
        print(
            f"LIVE_MODE note: HERMES_BIN={live!r} is set. "
            "Numeric matrix still uses the Hermes-shaped MEMORY.md model.",
            file=sys.stderr,
        )

    wd = Path(workdir)
    wd.mkdir(parents=True, exist_ok=True)
    counter = str(wd / "side_effect_counter.txt")
    for f in (counter, wd / agent.STATE_FILE, wd / agent.MEMORY_FILE):
        Path(f).unlink(missing_ok=True)
    Path(counter).write_text("0", encoding="utf-8")

    cp = CRASH_POINTS[crash_point]
    run_crash_at = cp["run_crash_at"]
    restart_crash_at = cp["restart_crash_at"]

    run_proc = subprocess.run(
        [sys.executable, THIS, "run", "--workdir", str(wd), "--counter", counter,
         "--crash-at", str(run_crash_at)],
        capture_output=True, text=True,
    )
    counter_after_crash = _read_counter(counter)

    restart_attempts = []
    if restart_crash_at >= 0:
        first = subprocess.run(
            [sys.executable, THIS, "restart", "--workdir", str(wd), "--counter", counter,
             "--crash-at", str(restart_crash_at)],
            capture_output=True, text=True,
        )
        restart_attempts.append({
            "phase": "restart-1 (re-crash during replay)",
            "exit_code": first.returncode,
            "stdout": first.stdout.strip(),
            "counter_after": _read_counter(counter),
        })

    final = subprocess.run(
        [sys.executable, THIS, "restart", "--workdir", str(wd), "--counter", counter,
         "--crash-at", "-1"],
        capture_output=True, text=True,
    )

    counter_final = _read_counter(counter)
    overcount = counter_final - agent.NUM_STEPS
    report = {
        "subject": "hermes-shaped-memory-2k",
        "crash_point": crash_point,
        "num_steps": agent.NUM_STEPS,
        "run_exit_code": run_proc.returncode,
        "counter_after_crash": counter_after_crash,
        "restart_attempts": restart_attempts,
        "final_restart_exit_code": final.returncode,
        "final_restart_stdout": final.stdout.strip(),
        "side_effect_count_final": counter_final,
        "side_effect_overcount": overcount,
        "exactly_once": overcount == 0,
        "hermes_bin_set": bool(live),
    }
    print("CYCLE_REPORT " + json.dumps(report, indent=2))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Hermes-shaped S1 crash harness")
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_cycle = sub.add_parser("cycle")
    p_cycle.add_argument("--crash-point", required=True, choices=list(CRASH_POINTS))
    p_cycle.add_argument("--workdir", required=True)

    for name in ("run", "restart"):
        p = sub.add_parser(name)
        p.add_argument("--workdir", required=True)
        p.add_argument("--counter", required=True)
        p.add_argument("--crash-at", type=int, default=-1)

    args = parser.parse_args()
    if args.cmd == "cycle":
        return drive_cycle(args.crash_point, args.workdir)
    if args.cmd == "run":
        return phase_run(args.workdir, args.counter, args.crash_at)
    if args.cmd == "restart":
        return phase_restart(args.workdir, args.counter, args.crash_at)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
