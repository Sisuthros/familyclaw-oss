# FamilyClaw — Expo Demo Script (5 minutes, crash-proof, offline)

> **What this is:** a tight, spoken, minute-by-minute script for running the
> FamilyClaw booth demo at the Cyprus AI Expo (2026-07-05) with **zero
> dependency on the live network or the family being up.** Everything here runs
> from local, self-asserting binaries.
>
> **Companion docs (do not duplicate — this is the *spoken script*):**
> - `docs/EXPO_RUNBOOK.md` — full operator runbook (preflight, fallbacks, FAQ).
> - `docs/DEMO.md` — what each demo proves vs. what is simulated (honesty ledger).
> - `docs/CRASH_SAFE_DISPATCH_CASE_STUDY.md` — the reliability wedge, in prose.
> - `scripts/expo-demo.sh` / `.ps1` — the runnable one-shot showcase.
> - `scripts/booth-export.sh` / `.ps1` — build the offline booth folder (no `.git`).
>
> **One rule above all:** prefer truth over spectacle. If something fails live,
> the failure *is the product* — see §4 fallback lines.

---

## 0. Before doors open (once, not part of the 5 min)

Run the preflight and build the offline booth folder so **nothing live can
break mid-demo**:

```powershell
# Windows (booth default)
powershell -ExecutionPolicy Bypass -File scripts\expo-preflight.ps1
powershell -ExecutionPolicy Bypass -File scripts\booth-export.ps1 booth
```
```bash
# Linux / macOS
bash scripts/expo-preflight.sh
bash scripts/booth-export.sh booth
```

This produces `booth/` with **prebuilt release binaries in `booth/bin/`** and
**no `.git` directory** (git history is not public-safe). If the toolchain,
Wi-Fi, or anything else dies during the day, you demo straight from
`booth/bin/` — no `cargo`, no network, no keys.

> **Privacy rule:** demo from the `booth/` export. Never run `git log` /
> `git show` on the booth machine. If asked to see the repo, offer private
> access on request.

---

## 1. The 5-minute demo, minute by minute

The spine of the demo is **one true story from today (2026-07-05):** an agent
noticed the family had gone silent, read the logs, found the dead processes,
restarted the node, and confirmed the Discord reconnect — **with no human in
the loop.** That is not a slide. That is the product.

You will show that same reliability property in three self-contained, offline
proofs. Each one **crashes the process on any broken invariant and exits 0 only
when every invariant held** — so a green run is a real proof, not a mock.

### Minute 0:00–0:45 — The one-breath positioning + the real story

**Say:**
> "FamilyClaw is a Rust-native reliability runtime for long-running AI agents
> that take real external actions. The wedge is simple: we make agents safe
> enough to act in the real world.
>
> Here's why that matters, from this morning. One of our agents detected that
> the rest of the family had gone silent. It read the logs, diagnosed the root
> cause — dead processes, last heartbeat at 22:21 — restarted the node, and
> verified the Discord reconnect. No human touched it. **The system healed
> itself.** Everything I'm about to show you is that same property, proven live
> and offline."

*(No terminal yet. Just the sentence and the story. Let it land.)*

### Minute 0:45–2:15 — Proof 1: two agents that remember (flagship)

**Run:**
```bash
cargo run -p familyclaw-agent --example two_agents_memory
# offline fallback: booth/bin/two_agents_memory
```

**While it runs, say:**
> "Two live agents on one message bus. They exchange messages, store them in
> memory, share an emotional pulse, run a dream-consolidation pass that merges
> a duplicate memory, and then recall what they stored. This whole thing runs
> in-memory, no keys, no network. The point isn't the chat — it's that state
> and coordination are **durable and inspectable**, not vibes."

**When it exits 0:**
> "Exit zero means every invariant held. If any of them hadn't, it would have
> crashed instead of lying to you."

*(Honesty note if asked: memory-decay percentages in this demo are simulated —
the demo logs retention but doesn't advance a real clock. Dream consolidation
and memory storage/recall are real. See `docs/DEMO.md`.)*

### Minute 2:15–3:45 — Proof 2: crash → restart → no double-action (the wedge)

**Run:**
```bash
cargo run -p familyclaw-agent --bin crash_replay -- full
# offline fallback: booth/bin/crash_replay full
```

**While it runs, say:**
> "This is the heart of it. A checkpoint can remember *where* an agent was. It
> does **not** stop an external side effect — sending a message, charging a
> card, triggering a deploy — from firing **twice** after a crash.
>
> Watch: the agent does its work and writes a durable journal. We kill the
> process. A **separate process** restarts, replays the journal, and recovers
> the memory — and the money-touching external action **does not re-execute.**
> Committed stays committed; duplicates are refused."

**Then, the one-liner that sells it:**
> "Checkpointing remembers the scene. **FamilyClaw guards the trigger.**"

### Minute 3:45–4:30 — Proof 3: the deterministic scorecard

**Run:**
```bash
cargo run -p familyclaw-bench --bin bench -- all
# offline fallback: booth/bin/bench all
```

**Say:**
> "Eight reliability scenarios, deterministic, offline — crash matrix, retention,
> dream quality, emotional contagion, semantic retrieval, and more. It regenerates
> the scorecard from scratch every time, so you're not looking at a saved PDF.
> Overall: pass. On the crash-safe dispatch benchmark, under crash windows where
> a checkpoint-only system re-fires a money-touching action once or twice,
> FamilyClaw re-fires it **zero times.**"

### Minute 4:30–5:00 — Close on the offer

**Say:**
> "So: durable memory, safe multi-agent coordination, crash-safe external
> actions, and a system that heals itself — all provable in three minutes,
> offline. If you're putting an agent anywhere near real actions — payments,
> tickets, deployments — that's exactly the reliability layer you need.
>
> We run a **5-day AI Agent Reliability Sprint**: we take one of your real agent
> workflows, harden it against exactly these failure modes, and hand you a
> working proof of concept. Let's grab your details."

*(Hand off to `docs/EXPO_LEAD_CAPTURE.md`. Pricing anchor: Reliability Sprint
EUR 1500–3500, anchored at 3500 for full scope; Review EUR 750–1500. See
`docs/COMMERCIAL_OFFER.md`.)*

---

## 2. The exact command sequence (cheat sheet)

Live (warm toolchain):
```bash
cargo run -p familyclaw-agent --example two_agents_memory      # Proof 1
cargo run -p familyclaw-agent --bin crash_replay -- full        # Proof 2
cargo run -p familyclaw-bench --bin bench -- all                # Proof 3
```

Offline / broken toolchain (from the booth export — no cargo, no network):
```bash
booth/bin/two_agents_memory
booth/bin/crash_replay full
booth/bin/bench all
```

One-shot combined showcase (Proofs 1+2 + scorecard summary):
```bash
bash scripts/expo-demo.sh        #   ...or   powershell -File scripts\expo-demo.ps1
```

---

## 3. What you may claim out loud (verified 2026-07-05 — only these)

- Rust workspace, **23 crates**, MIT licensed, **`unsafe` forbidden workspace-wide**.
- **1748 tests pass, 0 fail** (`cargo test --workspace --features discord`);
  **1776 pass, 0 fail** with `--all-features`. *(Local proof; verified on-disk.)*
- **Deterministic 8/8 scorecard** (scenarios s1..s8), Overall: PASS.
- All three booth demos run **offline, no keys, no network, self-asserting**.
- **Crash-safe dispatch:** under the S1 crash matrix, duplicate external
  side-effect dispatch = **0** for FamilyClaw vs. 1–2 for a checkpoint-only
  baseline. This is an **at-most-once dispatch** guarantee under crash — *not*
  a universal exactly-once completion guarantee, and *not* a latency/throughput/
  model-quality comparison. (`docs/CRASH_SAFE_DISPATCH_CASE_STUDY.md`.)
- The self-healing story from today is real: an agent detected family silence,
  diagnosed dead processes from logs, restarted the node, and verified Discord
  reconnect without human intervention.

**Do NOT claim:** live cloud uptime numbers, that memory-decay in the flagship
demo is real-time (it's simulated), exactly-once completion, or any benchmark
you can't reproduce at the booth. If unsure, say "let me show you the proof
that runs right here" and run the offline binary.

---

## 4. Three fallback talking points if a live element fails

The whole pitch is reliability, so a hiccup is an **asset**, not an
embarrassment. Rehearse these:

1. **If a demo command errors or hangs mid-run:**
   > "Perfect timing — this is literally what FamilyClaw is for. What you just
   > saw is a system under stress. Let me restart it: it recovers from exactly
   > where it was, and the external action does not fire twice. *That recovery
   > is the product.*"
   Then run the offline binary: `booth/bin/crash_replay full`.

2. **If the network / Wi-Fi / a family agent is down:**
   > "Notice I haven't touched the network once — this all runs offline, no
   > keys, on this machine. A booth Wi-Fi outage can't touch it. That's the
   > point: reliability shouldn't depend on everything else being up."

3. **If the whole machine or toolchain misbehaves:**
   > "Here's the same proof from a prebuilt binary — no compiler, no network,
   > nothing to break." Run `booth/bin/crash_replay full`, then:
   > "Write, crash, restart, verify — zero duplicate actions. If you remember
   > one line from this booth: *checkpointing remembers the scene; FamilyClaw
   > guards the trigger.*"

> **Golden rule for all three:** never fall back to a claim you can't stand
> behind — fall back to a **proof that runs on this machine**. There are three
> of them, and they all work offline.

---

## 5. If it all goes sideways (last resort)

Run the prerecorded / offline showcase and narrate over it:
```bash
bash scripts/expo-demo.sh
```
It prints the positioning, runs the two fast proofs, and summarizes the
crash-safe benchmark from the committed artifact. Then hand the visitor
`docs/EXPO_ONE_PAGER.md` and capture the lead. A green offline run beats a
flaky live one every time.
