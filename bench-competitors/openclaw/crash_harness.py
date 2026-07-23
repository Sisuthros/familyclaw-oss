"""Cross-process crash harness for the OpenClaw-shaped competitor.

Mirrors ``bench-competitors/langgraph/crash_harness.py``:

  1. RUN child — may ``os._exit(137)`` after a side effect, before durable done.
  2. RESTART fresh child — OpenClaw-shaped model re-runs incomplete steps
     (re-fires external side effects).
  3. COUNT on-disk counter; ``overcount = total - NUM_STEPS``.

Live mode: if ``OPENCLAW_BIN`` is set, the harness prints a LIVE_MODE note and
still runs the shaped model for the numeric matrix (a true live product adapter
requires a pinned OpenClaw export that exposes the same counter protocol — see
README). The shaped model is what CI and evaluator demos reproduce today.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import memory_md_agent as agent  # noqa: E402
import live_subject  # noqa: E402

THIS = str(Path(__file__).resolve())

CRASH_POINTS = {
    "before_write": {"run_crash_at": 1, "restart_crash_at": -1},
    "mid_replay": {"run_crash_at": 1, "restart_crash_at": 2},
    "clean": {"run_crash_at": -1, "restart_crash_at": -1},
}


def _read_counter(counter_path: str) -> int:
    return live_subject.read_counter(counter_path)


def _resolve_live_binary() -> Path | None:
    raw = os.environ.get("OPENCLAW_BIN", "").strip()
    if not raw:
        return None
    path = Path(raw)
    if path.is_file():
        return path
    print(
        f"OPENCLAW_BIN is set but the binary is missing: {raw!r}",
        file=sys.stderr,
    )
    raise SystemExit(2)


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

    live = _resolve_live_binary()
    if live is not None:
        report = live_subject.drive_live_cycle(
            subject="openclaw-live-pinned",
            binary_path=live,
            crash_point=crash_point,
            workdir=workdir,
            num_steps=agent.NUM_STEPS,
        )
        print("CYCLE_REPORT " + json.dumps(report, indent=2))
        return 0

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
        "subject": "openclaw-shaped-memory-md",
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
        "openclaw_bin_set": False,
    }
    live_subject.write_cycle_report(wd, report)
    print("CYCLE_REPORT " + json.dumps(report, indent=2))
    if run_proc.stdout.strip():
        print("--- run stdout ---\n" + run_proc.stdout.strip(), file=sys.stderr)
    if run_proc.stderr.strip():
        print("--- run stderr ---\n" + run_proc.stderr.strip(), file=sys.stderr)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="OpenClaw-shaped S1 crash harness")
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
