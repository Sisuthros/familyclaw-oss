#!/usr/bin/env bash
# Crash Replay Demo Script - runs true two-process crash replay demo
# Usage: bash scripts/demo-crash-replay.sh

set -e

echo "═══════════════════════════════════════════════════════════"
echo "  FamilyClaw Crash Replay Demo — Two-Process Verification"
echo "═══════════════════════════════════════════════════════════"
echo ""

# Clean start
echo ">>> STEP 1: CLEAN <<<"
cargo run -p familyclaw-agent --bin crash_replay -- clean
echo ""

# Phase 1: Write (first process)
echo ">>> STEP 2: WRITE (Process 1) <<<"
cargo run -p familyclaw-agent --bin crash_replay -- write
echo ""

# Small delay
sleep 1

# Phase 2: Verify (second process - simulating process restart)
echo ">>> STEP 3: VERIFY (Process 2 - simulating restart) <<<"
cargo run -p familyclaw-agent --bin crash_replay -- verify
echo ""

echo "═══════════════════════════════════════════════════════════"
echo "  ✅ TWO-PROCESS CRASH REPLAY DEMO COMPLETE"
echo "  FamilyClaw: agents that remember, even after death. 💀→🧠"
echo "═══════════════════════════════════════════════════════════"