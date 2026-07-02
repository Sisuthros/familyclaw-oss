#!/usr/bin/env bash
# Public release demo (Layer A) — no API keys, no private profiles, no channels.
#
# Default mode is fast (~2-4 min on a warm build): the flagship continuity demo,
# a durable crash-replay proof, and the 8-scenario continuity scorecard. It does
# NOT run the full workspace test suite.
#
#   bash scripts/public-demo.sh          # fast public demo
#   bash scripts/public-demo.sh --full   # full verification (slow)
#
# --full adds: the entire workspace test suite, the --all-features test suite,
# the comparative LangGraph benchmark, and the Layer B leak audit.

set -euo pipefail

cd "$(dirname "$0")/.."

FULL=0
if [[ "${1:-}" == "--full" || "${1:-}" == "-Full" ]]; then
    FULL=1
fi

step() {
    echo ""
    echo "=== $1 ==="
    shift
    if ! "$@"; then
        echo "FAILED: $*"
        exit 1
    fi
}

echo "=== FamilyClaw public demo (Layer A) ==="
echo "No API keys, no network, no private profiles, no paid services."

# 1. Flagship continuity demo — two live agents on the bus.
step "1/3  Flagship continuity demo (two_agents_memory)" \
    cargo run -p familyclaw-agent --example two_agents_memory

# 2. Durable crash-replay proof — two-process, write then restart-and-verify.
step "2/3  Durable crash-replay proof" \
    bash scripts/demo-crash-replay.sh

# 3. Continuity scorecard — 8 deterministic scenarios (s1..s8).
step "3/3  Continuity scorecard (8 scenarios)" \
    cargo run -p familyclaw-bench --bin bench -- all

if [[ "$FULL" == "1" ]]; then
    echo ""
    echo "--- Full verification (this is slow) ---"
    step "Full  Workspace test suite" \
        cargo test --workspace --features discord
    step "Full  All-features test suite" \
        cargo test --workspace --all-features
    step "Full  Comparative benchmark (vs LangGraph harness)" \
        cargo run -p familyclaw-bench --bin bench -- compare
    step "Full  Layer B leak audit" \
        bash scripts/audit-layer-b.sh
fi

echo ""
echo "Public demo complete."
echo "Scorecard output: crates/familyclaw-bench/out/SCORECARD.md"
echo "Next: see docs/EXPO_BRIEF.md, STATUS.md, and docs/QUICKSTART.md."
