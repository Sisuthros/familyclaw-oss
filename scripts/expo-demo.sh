#!/usr/bin/env bash
# FamilyClaw — Expo demo (Bash). ~2-4 minutes on a warm build.
#
#   bash scripts/expo-demo.sh
#
# A short, live, self-contained showcase for a booth or a talk. No API keys, no
# network, no Python environment, no paid services. It runs the two proofs that
# execute in seconds, then summarizes the crash-safety benchmark from the
# committed artifact (the full LangGraph reproduction is a separate command).
#
# Fails immediately if any step fails.

set -euo pipefail

cd "$(dirname "$0")/.."

step() {
    echo ""
    echo "=== $1 ==="
    shift
    if ! "$@"; then
        echo "FAILED: $*"
        exit 1
    fi
}

# 1. Positioning statement.
echo "═══════════════════════════════════════════════════════════════"
echo "  FamilyClaw — a Rust-native reliability runtime for long-running"
echo "  AI agents: memory, coordination, safe external actions, and"
echo "  crash recovery. Every claim below is proven live or reproducible."
echo "═══════════════════════════════════════════════════════════════"

# 2. Flagship continuity demo.
step "1/2  Flagship continuity demo (two live agents on one bus)" \
    cargo run -p familyclaw-agent --example two_agents_memory

# 3. Durable crash-replay proof (shortest deterministic crash proof).
step "2/2  Durable crash-replay proof (write -> crash -> restart -> verify)" \
    bash scripts/demo-crash-replay.sh

# 4. LangGraph comparison summary — from the committed, reproducible artifact.
echo ""
echo "=== Crash-safe dispatch benchmark (summary from committed artifact) ==="
echo "  After a process crash, how many money-touching external side effects re-execute?"
echo ""
echo "    Crash point                                  FamilyClaw   LangGraph"
echo "    clean (no crash)                                  0           0"
echo "    before_write (effect done, record not yet)        0           1"
echo "    mid_replay  (re-crash during replay)              0           2"
echo ""
echo "  Honesty note: this measures duplicate external side-effect dispatch under"
echo "  specific crash windows. It is NOT a throughput, latency, usability, or"
echo "  model-quality comparison. Full numbers: bench-competitors/langgraph/RESULTS.md"

# 5. Exact reproduction commands.
echo ""
echo "=== Reproduce everything yourself ==="
echo "  Flagship demo : cargo run -p familyclaw-agent --example two_agents_memory"
echo "  Crash replay  : bash scripts/demo-crash-replay.sh"
echo "  Scorecard (8) : cargo run -p familyclaw-bench --bin bench -- all"
echo "  LangGraph bench (needs Python, separate):"
echo "    cd bench-competitors/langgraph && python -m venv .venv \\"
echo "      && .venv/bin/python -m pip install langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0 \\"
echo "      && .venv/bin/python crash_harness.py"

# 6. Capability summary.
echo ""
echo "=== FamilyClaw proves ==="
echo "  * persistent multi-agent continuity"
echo "  * durable crash replay"
echo "  * duplicate-prevented external action dispatch"
echo "  * approval-gated action execution"
echo "  * model failover with cooldown and key rotation"
echo "  * deterministic local verification"
echo ""
echo "Expo demo complete. See docs/EXPO_BRIEF.md for the full brief."
