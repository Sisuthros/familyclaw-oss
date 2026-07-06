#!/usr/bin/env bash
# FamilyClaw - Expo preflight (Linux/macOS). Run this BEFORE the booth opens.
#
#   bash scripts/expo-preflight.sh
#
# Verifies the machine can run the live demo end-to-end. Prints the commit,
# checks the toolchain, confirms required files exist, builds the demo binaries,
# runs the shortest critical tests, runs the flagship demo, runs the crash
# replay, then reports a single PASS/FAIL and returns a correct exit code.
#
# No API keys, no network, no paid services. Safe to run repeatedly.

set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
check() {
    local label="$1"; shift
    echo
    echo "=== $label ==="
    if "$@"; then
        echo "  PASS: $label"
    else
        echo "  FAIL: $label (exit $?)"
        fail=1
    fi
}

echo "==============================================================="
echo "  FamilyClaw - Expo preflight"
echo "==============================================================="

# 1. Commit / branch.
check "Repository commit" bash -c 'echo "  commit $(git rev-parse --short HEAD) on $(git rev-parse --abbrev-ref HEAD)"'

# 2. Toolchain.
check "Rust / Cargo available" bash -c 'cargo --version && rustc --version'

# 3. Required files exist.
check "Required demo files present" bash -c '
    required=(
        "crates/familyclaw-agent/examples/two_agents_memory.rs"
        "crates/familyclaw-agent/src/bin/crash_replay.rs"
        "crates/familyclaw-bench/src/bin/bench.rs"
        "scripts/expo-demo.sh"
        "docs/EXPO_BRIEF.md"
        "docs/EXPO_VALIDATION_PROOF.md"
    )
    for f in "${required[@]}"; do
        if [ ! -f "$f" ]; then echo "  missing $f"; exit 1; fi
        echo "  ok  $f"
    done'

# 4. Build the demo binaries (warms the cache so the live demo is fast).
check "Build demo binaries" bash -c '
    cargo build -p familyclaw-agent --example two_agents_memory &&
    cargo build -p familyclaw-agent --bin crash_replay &&
    cargo build -p familyclaw-bench --bin bench'

# 5. Shortest critical tests (durable replay is the load-bearing wedge).
check "Critical tests (durable replay)" cargo test -p familyclaw-durable

# 6. Flagship demo actually runs and self-asserts (exits non-zero on failure).
check "Flagship demo (two_agents_memory)" cargo run -p familyclaw-agent --example two_agents_memory

# 7. Crash replay actually survives a process boundary.
check "Durable crash replay (full)" cargo run -p familyclaw-agent --bin crash_replay -- full

# 8. Privacy guard: the demo tree must NOT expose git history at the booth.
echo
echo "=== Privacy guard (booth machines) ==="
if [ -d .git ]; then
    echo "  WARN: .git present. On a PUBLIC booth machine, run the demo from a"
    echo "        clean export (git archive) with no .git, and do not run 'git log'."
    echo "        Git history contains private Layer B names."
else
    echo "  PASS: no .git in this tree (clean export)."
fi

echo
echo "==============================================================="
if [ "$fail" -eq 0 ]; then
    echo "  PREFLIGHT PASS - the machine is demo-ready."
else
    echo "  PREFLIGHT FAIL - fix the FAILs above before the booth opens."
fi
echo "==============================================================="
exit "$fail"
