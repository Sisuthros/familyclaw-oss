# Two Agents Memory — Family + Continuity Demo

**The one thing neither OpenClaw nor Hermes can show:** two *named* agents that
share memory, feel each other, dream overnight, and **wake up behaving
differently because of that dream.**

This is a single, deterministic, self-checking demo. Every line it prints is a
value read back out of the live engine, and the program `assert!`s each
invariant — so a stranger who runs it knows the numbers were not hand-typed.

## Run it (one command, no keys)

```bash
cargo run -p familyclaw-agent --example two_agents_memory
```

That's it. No config, no API keys, no network. It finishes in well under a
second and exits `0` only if all seven capabilities actually happened.

## What it proves — 7 capabilities, each verified live

| # | Capability | How it is *proven* (not just claimed) |
|---|------------|----------------------------------------|
| ① | **Resonance bus + two named agents** | Alice & Bob are `spawn()`ed as real actors; `bus.beings()` reports 2 registered beings. |
| ② | **Shared memory** | Alice speaks; Bob runs the turn and stores it; Bob then *recalls* Alice's words (printed with relevance). |
| ③ | **Emotion contagion** | Bob's `Joy` is printed **before** (0.0) and **after** (~19) Alice's joy pulse — the rise is asserted. |
| ④ | **Dream consolidation** | A dream cycle merges two identical greetings (active count 3 → 2) **and** grounds a relative date: `"eilen"` (EN: "yesterday") → `"eilen (2026-07-02)"`. Both before/after strings are printed. |
| ⑤ | **Next-day different behavior** | The **same** query (`"perhe sää"`, EN: "family weather") returns a **different** top memory on day 1 vs day 8 — the whole point. |
| ⑥ | **Identity anchor survives decay** | Raw Ebbinghaus retention is printed: trivia `1.00 → 0.12` (Fast), anchor `1.00 → 1.00` (ProtectedCore). |
| ⑦ | **Deterministic, one command, no keys** | You just ran it yourself. |

### The heart of it: continuity that changes behavior (⑤ + ⑥)

Bob starts day 1 with two memories that both match the day's small-talk query
`"perhe sää"` ("family weather"):

- **trivia** — fresh chit-chat (`Fast` decay), matches *both* query words.
- **anchor** — his mission (`ProtectedCore`, never decays), matches only `perhe`.

```
DAY 1 → top memory: "Tänään perhe jutteli säästä ja grillasi yhdessä."   (EN: "Today the family chatted about the weather and grilled together." — the chatter wins)
DAY 8 → top memory: "Perhe on se, jonka takia rakennan tätä maailmaa."   (EN: "Family is the reason I'm building this world." — the anchor wins)
```

Nothing was rewritten by hand between the two days. The *same* retrieval engine
is scored against the *same* memories at two different clocks. Ebbinghaus decay
erodes the trivial chatter (retention `1.00 → 0.12`) while the identity anchor
holds at `1.00`. Bob literally woke up **more himself**.

## Expected output (abridged)

```
① Two named agents join the resonance bus
   ✓ resonance bus started, 2 beings registered on the mesh
   · Alice (…)
   · Bob   (…)

② Shared memory: Alice tells Bob something, Bob remembers it
   ✓ Bob stored the message — Bob now holds 1 memory/-ies
   query "perhe" → top: "Perustimme perheen tänään …"  (EN: "We founded the family today …") (relevance 0.111)

③ Emotion contagion: Alice's joy raises Bob's mood (before → after)
   Bob BEFORE : joy = 0.0, curiosity = 4.5
   Bob AFTER  : joy = 19.1, curiosity = 18.8
   ✓ Bob's joy rose by 19.1 …

④ Dream consolidation: merge duplicates + absolutize relative dates
   ✓ duplicate greeting merged (report.merged = 1); active memories: 3 → 2
   ✓ relative date grounded to an absolute calendar date:
       before: "Bob liittyi busiin eilen …"    (EN: "Bob joined the bus yesterday …")
       after : "Bob liittyi busiin eilen (2026-07-02) …"    (EN: "Bob joined the bus yesterday (2026-07-02) …")

⑤ Next day: the SAME question gets a DIFFERENT answer
   DAY 1 → top: "Tänään perhe jutteli säästä ja grillasi yhdessä."   (EN: "Today the family chatted about the weather and grilled together.")
   DAY 8 → top: "Perhe on se, jonka takia rakennan tätä maailmaa."   (EN: "Family is the reason I'm building this world.")

⑥ Identity anchor survives Ebbinghaus decay; trivia fades
   ANCHOR (ProtectedCore)   retention day1 = 1.00 → day8 = 1.00
   trivia (Fast decay)      retention day1 = 1.00 → day8 = 0.12
```

## Notes on honesty

- **Sample dialogue is still Finnish at the source.** The underlying demo
  binary (`crates/familyclaw-agent`, out of scope for this docs pass) currently
  prints its sample memory/dialogue strings in Finnish. English translations
  are given inline above next to every quoted line so this doc is readable
  without knowing Finnish; the quoted originals are left untouched because
  they are real, verified program output (see the top of this file), not a
  paraphrase.

- **Date grounding, not deletion.** The dream cycle *keeps* the human word and
  appends the resolved calendar date (`"eilen"` → `"eilen (2026-07-02)"`), so a
  relative date read a year later still points at the exact day it happened.
- **Merge tombstones the duplicate.** Total stored rows stay the same; the
  *active* (retrievable) count drops. The demo reports the active count.
- **Direct turn-driving.** After proving the bus join with spawned actors, the
  demo drives the two agents through `handle_turn` (the *same* code path the
  actor runs) so it can read their inner memory/emotion state at every step.

## Crash-proof continuity (separate demo)

To see that memory survives a mid-run crash and replay, use the dedicated
binary:

```bash
cargo run -p familyclaw-agent --bin crash_replay
# or the runbook:
./scripts/demo-crash-replay.sh
```

The `familyclaw-durable` crate's journal-based replay ensures side effects
aren't re-run on restart, while memory persists.

---

**Layer A only.** The two agents are generic (`Alice`, `Bob`). No real souls,
no private calibration weights, no API keys, no personal paths. Pure open
platform.
