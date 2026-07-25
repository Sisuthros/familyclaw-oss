#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT"
export PATH="${HOME}/.cargo/bin:${PATH}"

echo "=== infra-teardown: Time Machine dry-run (replay demo) ==="
cargo run -p familyclaw-agent -- replay demo
echo "PASS: replay demo (dry-run capture)"

echo "=== infra-teardown: approval gate spot-check (shell_exec) ==="
cargo test -p familyclaw-actions shell_exec:: -- --nocapture
echo "PASS: write-external skills require approval"

echo
echo "infra-teardown pack: dry-run + approval gates demonstrated locally"
