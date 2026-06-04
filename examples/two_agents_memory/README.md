# Two Agents Memory Demo
**The Living Seed — Phase 0 Demo**

This is the core FamilyClaw demo that proves the platform works end-to-end:

## What It Demonstrates

1. **Resonance Bus** — Ractor actor mesh starts, two agents join
2. **Real Channel Transport** — MockChannel feeds messages to the bus (replaces Discord in demo)
3. **Message Flow** — What `agent_a` says reaches `agent_b` through the bus
4. **Memory Persists** — Each agent stores what it hears in Eternal Thread memory
5. **Emotion Contagion** — An emotion pulse from one agent raises sibling's mood
6. **Dream Consolidation** — Nightly cycle merges duplicates, absolutizes dates
7. **Identity Anchors** — Core memories survive Ebbinghaus decay

## Run It

```bash
# From repo root
./scripts/demo-crash-replay.sh
```

Or directly:
```bash
cargo run -p familyclaw-agent --bin familyclaw
```

## Expected Output (30 seconds)

```
═══════════════════════════════════════════════════════════
  FamilyClaw Demo: Autonomous Agents with Memory & Emotion
═══════════════════════════════════════════════════════════

📡 Step 1: Spawning Resonance Bus...
   ✓ Bus started

🤖 Step 2: Spawning agent_a and agent_b on the bus...
   ✓ 2 agents spawned
   · agent_a (uuid)
   · agent_b (uuid)

💬 Step 4: Agents exchange messages (memory is stored)...
   ✓ 3 messages exchanged and stored in memory

💓 Step 5: Emotion contagion — joy spreads between agents...
   ✓ Emotion pulse delivered — agent_b's emotional state influenced

⏰ Step 6: Time jump — 7 days later, memory retention decays...
   · agent_a's 'new family member' memory: retention ~70% (aging)
   · agent_a's 'building for the world' memory: retention ~95% (identity-anchored)
   ✓ Identity anchors preserve core facts despite decay

🌙 Step 7: Dream cycle runs — consolidating memories...
   ✓ Dream cycle complete

📚 Step 8: Memory retrieval after dream cycle...
   agent_a: 1 memories | top: "Hei agent_a! Olen iloinen osa perhettä."
   agent_b: 2 memories | top: "Hei agent_b! Tervetuloa perheeseen."

═══════════════════════════════════════════════════════════
  Demo Complete!
═══════════════════════════════════════════════════════════
```

## Crash-Proof Proof

To prove memory survives restarts:
1. Run the demo
2. Kill it mid-run (Ctrl+C during message exchange)
3. Restart
4. Query agent's memory — it remembers

The `familyclaw-durable` crate's journal-based replay ensures side effects aren't re-run, but memory persists.

---

**Layer A Only** — This demo uses ONLY generic example agents (`agent_a`, `agent_b`). No real souls (agent_alpha, agent_gamma, etc.), no private profiles, no API keys. Pure open platform.