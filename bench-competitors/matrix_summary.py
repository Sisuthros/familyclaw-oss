"""Render bench-competitors/MATRIX.md from per-cycle report files."""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path


CRASH_POINTS = ("clean", "before_write", "mid_replay")


def load_report(path: Path) -> dict | None:
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def format_subject(report: dict | None, default_subject: str, missing: str) -> str:
    if report is None:
        return missing
    subject = report.get("subject", default_subject)
    overcount = report.get("side_effect_overcount", "?")
    sha = report.get("live_binary_sha256")
    if sha:
        return f"{overcount} (`{subject}`, sha256 `{sha[:12]}`)"
    return f"{overcount} (`{subject}`)"


def main() -> int:
    parser = argparse.ArgumentParser(description="Render crash matrix summary markdown")
    parser.add_argument("--root", required=True, help="Workspace root")
    parser.add_argument("--familyclaw-cell", required=True, help="FamilyClaw table cell")
    args = parser.parse_args()

    root = Path(args.root)
    bench_dir = root / "bench-competitors"
    generated_at = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%SZ")

    lines = [
        "# Crash Matrix Summary",
        "",
        f"_Generated: {generated_at}_",
        "",
        "Cells show `side_effect_overcount` plus the reported subject mode.",
        "",
        "| Crash point | FamilyClaw | OpenClaw | Hermes | LangGraph |",
        "|---|---|---|---|---|",
    ]

    for crash_point in CRASH_POINTS:
        openclaw = load_report(
            bench_dir / "openclaw" / "_runs" / crash_point / "cycle_report.json"
        )
        hermes = load_report(
            bench_dir / "hermes" / "_runs" / crash_point / "cycle_report.json"
        )
        langgraph = load_report(
            bench_dir / "langgraph" / "_runs" / crash_point / "cycle_report.json"
        )
        lines.append(
            "| {cp} | {familyclaw} | {openclaw} | {hermes} | {langgraph} |".format(
                cp=f"`{crash_point}`",
                familyclaw=args.familyclaw_cell,
                openclaw=format_subject(
                    openclaw,
                    "openclaw-report-missing-subject",
                    "not run",
                ),
                hermes=format_subject(
                    hermes,
                    "hermes-report-missing-subject",
                    "not run",
                ),
                langgraph=format_subject(
                    langgraph,
                    "langgraph-harness",
                    "not run (no .venv)",
                ),
            )
        )

    lines.extend(
        [
            "",
            "## Notes",
            "",
            "- `FamilyClaw` is a fresh local `cargo run -p familyclaw-bench --bin bench -- s1` result when available; otherwise the matrix says `run separately`.",
            "- `OpenClaw` and `Hermes` default to shaped harnesses and switch to `*-live-pinned` only when the corresponding `*_BIN` path is set and exists.",
            "- Live-pinned rows record the pinned binary SHA-256 in `cycle_report.json` and show a short SHA here.",
        ]
    )

    (bench_dir / "MATRIX.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
