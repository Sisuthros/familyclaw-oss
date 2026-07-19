#!/usr/bin/env bash
# bench.sh — single-command FamilyClaw continuity benchmark.
#
# Builds the continuity_daemon binary (the black box the harness runs),
# then runs all scenarios (S1 Crash Matrix, S2 Retention Curve,
# S3 Dream Quality) with a fixed injected clock and writes:
#   - crates/familyclaw-bench/out/scorecard.json
#   - crates/familyclaw-bench/out/SCORECARD.md
#   - docs/SCORECARD.md
#
# Output is reproducible: two consecutive runs produce a byte-identical
# scorecard.json (design §6).
#
# Run:  bash scripts/bench.sh
#
# NOTE: the GNU toolchain is broken on this machine -> use stable-MSVC.

set -euo pipefail

TOOLCHAIN="${BENCH_TOOLCHAIN:-+stable-x86_64-pc-windows-msvc}"

echo "═══════════════════════════════════════════════════════════"
echo "  FamilyClaw Continuity Benchmark — reproducible proof"
echo "═══════════════════════════════════════════════════════════"

# 1) Build the black box (continuity_daemon) BEFORE running — the harness
#    locates it in the target/<profile>/ directory.
echo ">>> building continuity_daemon (black box) <<<"
cargo "$TOOLCHAIN" build -p familyclaw-agent --bin continuity_daemon

# 2) Run all scenarios with a fixed clock -> scorecard.
echo ">>> running all scenarios <<<"
cargo "$TOOLCHAIN" run -p familyclaw-bench -- all

echo "═══════════════════════════════════════════════════════════"
echo "  ✅ scorecard written to crates/familyclaw-bench/out/ + docs/"
echo "═══════════════════════════════════════════════════════════"
