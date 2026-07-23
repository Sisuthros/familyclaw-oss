"""Shared helpers for driving a pinned live competitor binary.

The live subject contract intentionally matches the shaped harness CLI:

    $COMPETITOR_BIN run --workdir W --counter C --crash-at N
    $COMPETITOR_BIN restart --workdir W --counter C --crash-at N

The harness owns the cycle orchestration and records reproducibility metadata
about the pinned binary alongside the numeric side-effect report.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
from pathlib import Path


CRASH_POINTS = {
    "before_write": {"run_crash_at": 1, "restart_crash_at": -1},
    "mid_replay": {"run_crash_at": 1, "restart_crash_at": 2},
    "clean": {"run_crash_at": -1, "restart_crash_at": -1},
}


def read_counter(counter_path: str) -> int:
    try:
        return int(Path(counter_path).read_text(encoding="utf-8").strip())
    except (FileNotFoundError, ValueError):
        return 0


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_version(binary_path: Path) -> str:
    proc = subprocess.run(
        [str(binary_path), "--version"],
        capture_output=True,
        text=True,
    )
    stdout = proc.stdout.strip()
    stderr = proc.stderr.strip()
    if stdout:
        return stdout
    if stderr:
        return stderr
    return f"<no version output; exit_code={proc.returncode}>"


def reset_workdir(workdir: Path) -> None:
    if workdir.exists():
        for entry in workdir.iterdir():
            if entry.is_dir():
                shutil.rmtree(entry)
            else:
                entry.unlink()
    workdir.mkdir(parents=True, exist_ok=True)


def write_cycle_report(workdir: Path, report: dict) -> None:
    (workdir / "cycle_report.json").write_text(
        json.dumps(report, indent=2) + "\n",
        encoding="utf-8",
    )


def drive_live_cycle(
    *,
    subject: str,
    binary_path: Path,
    crash_point: str,
    workdir: str,
    num_steps: int,
) -> dict:
    if crash_point not in CRASH_POINTS:
        raise ValueError(f"unknown crash point: {crash_point}")

    wd = Path(workdir)
    reset_workdir(wd)
    counter = str(wd / "side_effect_counter.txt")
    Path(counter).write_text("0", encoding="utf-8")

    crash_cfg = CRASH_POINTS[crash_point]
    run_proc = subprocess.run(
        [
            str(binary_path),
            "run",
            "--workdir",
            str(wd),
            "--counter",
            counter,
            "--crash-at",
            str(crash_cfg["run_crash_at"]),
        ],
        capture_output=True,
        text=True,
    )
    counter_after_crash = read_counter(counter)

    restart_attempts = []
    if crash_cfg["restart_crash_at"] >= 0:
        first = subprocess.run(
            [
                str(binary_path),
                "restart",
                "--workdir",
                str(wd),
                "--counter",
                counter,
                "--crash-at",
                str(crash_cfg["restart_crash_at"]),
            ],
            capture_output=True,
            text=True,
        )
        restart_attempts.append(
            {
                "phase": "restart-1 (re-crash during replay)",
                "exit_code": first.returncode,
                "stdout": first.stdout.strip(),
                "counter_after": read_counter(counter),
            }
        )

    final = subprocess.run(
        [
            str(binary_path),
            "restart",
            "--workdir",
            str(wd),
            "--counter",
            counter,
            "--crash-at",
            "-1",
        ],
        capture_output=True,
        text=True,
    )

    counter_final = read_counter(counter)
    overcount = counter_final - num_steps
    report = {
        "subject": subject,
        "crash_point": crash_point,
        "num_steps": num_steps,
        "run_exit_code": run_proc.returncode,
        "counter_after_crash": counter_after_crash,
        "restart_attempts": restart_attempts,
        "final_restart_exit_code": final.returncode,
        "final_restart_stdout": final.stdout.strip(),
        "side_effect_count_final": counter_final,
        "side_effect_overcount": overcount,
        "exactly_once": overcount == 0,
        "live_binary_sha256": sha256_file(binary_path),
        "live_binary_version": read_version(binary_path),
    }
    write_cycle_report(wd, report)
    return report
