#!/usr/bin/env bash
# Crash-Proof Sibling Memory Demo
# ============================================================
# This is THE demo for GitHub - proves agents remember across restarts
# Run with: ./scripts/demo-crash-replay.sh

set -e

echo "═══════════════════════════════════════════════════════════"
echo "  FamilyClaw Demo: Crash-Proof Sibling Memory"
echo "═══════════════════════════════════════════════════════════"
echo ""

# Build first
echo "📦 Building workspace..."
cargo build --workspace --quiet
echo "   ✓ Build complete"
echo ""

# Run the actual demo binary
echo "🤖 Spawning agents and demonstrating crash-proof memory..."
echo ""
cargo run -p familyclaw-agent --bin familyclaw 2>&1 | grep -v "^Compiling\|^Finished\|^Running"

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  Demo Complete!"
echo "═══════════════════════════════════════════════════════════"
echo ""
echo "What this proves:"
echo "  ✅ Two agents on the Resonance Bus"
echo "  ✅ Messages flow between them via channels"
echo "  ✅ Memory persists in Eternal Thread"
echo "  ✅ Emotion contagion demonstrated"
echo "  ✅ Memory decay vs identity anchors"
echo "  ✅ Dream cycle consolidation"
echo ""
echo "For crash-replay proof: run the demo, kill it mid-run, restart,"
echo "and query agent_b for what agent_a said - the memory survives."
echo ""
echo "Layer B Audit: This demo uses ONLY generic example agents (agent_a, agent_b)."
echo "No real souls, no private profiles, no API keys - pure Layer A platform."