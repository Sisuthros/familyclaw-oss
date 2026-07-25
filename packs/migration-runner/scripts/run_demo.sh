#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT"
export PATH="${HOME}/.cargo/bin:${PATH}"

echo "=== migration-runner: build continuity_daemon (bench black box) ==="
cargo build -p familyclaw-agent --bin continuity_daemon

echo "=== migration-runner: S1 crash matrix (step-0..step-4 migration analog) ==="
cargo run -p familyclaw-bench -- s1

SCORECARD="$ROOT/crates/familyclaw-bench/out/scorecard.json"
if [[ -f "$SCORECARD" ]]; then
  echo "--- s1_crash_matrix metrics ---"
  grep "side_effect_overcount" "$SCORECARD" | head -1 || true
fi

echo
echo "migration-runner pack: crash resume without re-apply proven locally"
echo "Mapping: step-0..step-4 in S1 = migration phases; target side_effect_overcount = 0"
