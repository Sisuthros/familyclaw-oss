#!/usr/bin/env bash
# Run competitor crash matrices and regenerate bench-competitors/MATRIX.md.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PY="${PYTHON:-python3}"
CRASH_POINTS=(clean before_write mid_replay)

run_one() {
  local dir="$1"
  echo "=== $dir ==="
  for cp in "${CRASH_POINTS[@]}"; do
    "$PY" "$ROOT/bench-competitors/$dir/crash_harness.py" cycle \
      --crash-point "$cp" --workdir "$ROOT/bench-competitors/$dir/_runs/$cp"
  done
}

run_langgraph() {
  local venv=""
  if [[ -x "$ROOT/bench-competitors/langgraph/.venv/bin/python" ]]; then
    venv="$ROOT/bench-competitors/langgraph/.venv/bin/python"
  elif [[ -x "$ROOT/bench-competitors/langgraph/.venv/Scripts/python.exe" ]]; then
    venv="$ROOT/bench-competitors/langgraph/.venv/Scripts/python.exe"
  fi

  echo "=== langgraph ==="
  if [[ -z "$venv" ]]; then
    echo "(skip langgraph - no .venv; see bench-competitors/langgraph/README.md)"
    return
  fi

  for cp in "${CRASH_POINTS[@]}"; do
    local workdir="$ROOT/bench-competitors/langgraph/_runs/$cp"
    "$venv" "$ROOT/bench-competitors/langgraph/crash_harness.py" cycle \
      --crash-point "$cp" --workdir "$workdir" | tee "$workdir/cycle_stdout.txt"
    "$PY" - "$workdir/cycle_stdout.txt" "$workdir/cycle_report.json" <<'PYCODE'
import json
import sys
from pathlib import Path

stdout_path = Path(sys.argv[1])
report_path = Path(sys.argv[2])
text = stdout_path.read_text(encoding="utf-8")
marker = "CYCLE_REPORT "
start = text.find(marker)
if start < 0:
    raise SystemExit(f"missing {marker!r} in {stdout_path}")
payload = text[start + len(marker):].strip()
report = json.loads(payload)
report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PYCODE
  done
}

familyclaw_cell() {
  if ! command -v cargo >/dev/null 2>&1; then
    printf '%s\n' "run separately"
    return 0
  fi

  echo "=== familyclaw ===" >&2
  cargo build -p familyclaw-agent --bin continuity_daemon
  cargo run -p familyclaw-bench --bin bench -- s1
  "$PY" - "$ROOT/crates/familyclaw-bench/out/scorecard.json" <<'PYCODE'
import json
import sys
from pathlib import Path

card = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
for scenario in card.get("scenarios", []):
    if scenario.get("id") == "s1_crash_matrix":
        value = scenario.get("metrics", {}).get("side_effect_overcount")
        if value is None:
            raise SystemExit("missing side_effect_overcount in scorecard")
        print(f"{int(value)} (`familyclaw`)")
        raise SystemExit(0)
raise SystemExit("missing s1_crash_matrix in scorecard")
PYCODE
}

run_one openclaw
run_one hermes
run_langgraph
FAMILYCLAW_CELL="$(familyclaw_cell)"
"$PY" "$ROOT/bench-competitors/matrix_summary.py" \
  --root "$ROOT" --familyclaw-cell "$FAMILYCLAW_CELL"
echo "Wrote $ROOT/bench-competitors/MATRIX.md"
