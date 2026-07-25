#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT"
export PATH="${HOME}/.cargo/bin:${PATH}"

echo "=== refund-guard: at-most-once dispatch red-team ==="
cargo test -p familyclaw-actions --test redteam_dispatch_exactly_once -- --test-threads=1
echo "PASS: redteam_dispatch_exactly_once"

echo "=== refund-guard: crash_replay across process boundary ==="
cargo run -p familyclaw-agent --bin crash_replay -- full
echo "PASS: crash_replay full"

echo
echo "refund-guard pack: at-most-once proven locally"
