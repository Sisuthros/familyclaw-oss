#!/usr/bin/env bash
# Run OpenClaw-shaped + Hermes-shaped + (optional) LangGraph crash matrices.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PY="${PYTHON:-python3}"

run_one() {
  local dir="$1"
  echo "=== $dir ==="
  for cp in clean before_write mid_replay; do
    "$PY" "$ROOT/bench-competitors/$dir/crash_harness.py" cycle \
      --crash-point "$cp" --workdir "$ROOT/bench-competitors/$dir/_runs/$cp"
  done
}

run_one openclaw
run_one hermes

if [[ -d "$ROOT/bench-competitors/langgraph" ]]; then
  echo "=== langgraph (requires venv; skip if missing) ==="
  if [[ -x "$ROOT/bench-competitors/langgraph/.venv/bin/python" ]]; then
    VENV="$ROOT/bench-competitors/langgraph/.venv/bin/python"
  elif [[ -x "$ROOT/bench-competitors/langgraph/.venv/Scripts/python.exe" ]]; then
    VENV="$ROOT/bench-competitors/langgraph/.venv/Scripts/python.exe"
  else
    VENV=""
  fi
  if [[ -n "$VENV" ]]; then
    for cp in clean before_write mid_replay; do
      "$VENV" "$ROOT/bench-competitors/langgraph/crash_harness.py" cycle \
        --crash-point "$cp" --workdir "$ROOT/bench-competitors/langgraph/_runs/$cp"
    done
  else
    echo "(skip langgraph — no .venv; see bench-competitors/langgraph/README.md)"
  fi
fi

echo "Done. FamilyClaw side: cargo run -p familyclaw-bench -- s1"
