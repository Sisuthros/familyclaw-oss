"""Verify a competitor cycle report against an expected overcount."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description="Verify cycle_report.json overcount")
    parser.add_argument("report", help="Path to cycle_report.json")
    parser.add_argument("--expect", type=int, required=True, help="Expected overcount")
    args = parser.parse_args()

    report_path = Path(args.report)
    try:
        report = json.loads(report_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        print(f"missing report: {report_path}", file=sys.stderr)
        return 2
    except json.JSONDecodeError as exc:
        print(f"invalid json in {report_path}: {exc}", file=sys.stderr)
        return 2

    actual = report.get("side_effect_overcount")
    subject = report.get("subject", "<unknown>")
    if actual != args.expect:
        print(
            f"FAIL {subject}: expected side_effect_overcount={args.expect}, got {actual}",
            file=sys.stderr,
        )
        return 1

    print(f"OK {subject}: side_effect_overcount={actual}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
