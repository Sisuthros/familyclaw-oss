# FamilyClaw — Expo Operator Runbook

> **Purpose:** Everything the operator needs to run the FamilyClaw booth at the
> Cyprus AI Expo. This is an **operator document**, not marketing. It tells you
> exactly what to run, what you should see, what to say, what to do when
> something breaks, and which claims are safe to make out loud.
>
> **One rule above all:** Prefer truth over spectacle. If a demo fails, fall
> back to a proof we can stand behind — never to a claim we cannot.

---

## 0. What FamilyClaw is (say this in one breath)

> **FamilyClaw is a Rust-native reliability runtime for long-running AI agents
> that take real external actions.** The wedge: *we make AI agents safe enough
> to act in the real world.*

Everything at the booth exists to make that sentence believable in under three
minutes, live, offline, with no API keys.

**Facts you can lean on:**

- Rust workspace, **23 crates**, **v1.2.0**, MIT licensed.
- `unsafe` is **forbidden workspace-wide**.
- **~1721 tests**; deterministic **8/8 scorecard** (scenarios s1..s8).
- All three booth demos run **offline, with no keys, no network**, and are
  **self-asserting** (they crash the process on any broken invariant and exit 0
  only when every invariant held).

---

## 1. Preflight (do this before doors open)

Run the preflight script for your platform. It builds the demos warm, runs a
quick sanity pass, and confirms the machine is ready.

**Windows (booth default):**

```powershell
powershell -ExecutionPolicy Bypass -File scripts\expo-preflight.ps1
```

**Linux / macOS:**

```bash
bash scripts/expo-preflight.sh
```

**What preflight should do / confirm:**

1. `cargo build -p familyclaw-agent --example two_agents_memory` — warm the
   flagship build so the live demo starts instantly.
2. `cargo build -p familyclaw-agent --bin crash_replay` — warm the crash proof.
3. `cargo build -p familyclaw-bench --bin bench` — warm the scorecard.
4. A dry run of the flagship demo to confirm exit code 0.
5. Confirm `booth/` prebuilt binaries and `docs/EXPO_VALIDATION_PROOF.md` are
   present as the compile-failure fallback (see §6).

**If preflight exits non-zero: STOP.** Do not open with a live demo. Switch to
the prebuilt-binary path (§6) or the prerecorded demo (§7) until you can rebuild.

> **Privacy rule (mandatory):** Run the booth from a **clean export with no
> `.git` directory**. **Never run `git log`, `git show`, or any history command
> on the booth machine.** The working tree is publishable; the git *history* is
> not (it contains private internal names — see `docs/EXPO_VALIDATION_PROOF.md`
> and `docs/PUBLISH_ORPHAN_PLAN.md`). If someone asks to see the repo, offer
> **private demo access on request** — do not open a terminal into git history.

---

## 2. The three verified demos

All three are real, present in the tree, offline, keyless, and self-asserting.

| # | Purpose | Command |
|---|---------|---------|
| **Flagship** | Two live agents on one bus: memory, delivery, emotion, dream consolidation, decay | `cargo run -p familyclaw-agent --example two_agents_memory` |
| **Crash proof** | Write → crash → restart → verify: no duplicate external action | `cargo run -p familyclaw-agent --bin crash_replay -- full` |
| **Scorecard** | Deterministic 8/8 reliability scorecard + LangGraph crash-window summary | `cargo run -p familyclaw-bench --bin bench -- all` |

### 2.1 Flagship — `two_agents_memory`

**What it prints (and asserts live — exits non-zero on any failure):**

1. Two named agents register on the resonance bus (2 beings live).
2. Real message delivery: one agent publishes → the other's actor receives and
   stores it → it is recalled from the real store. The sender does **not** store
   her own broadcast.
3. Real emotion contagion over the bus (a Joy value rises after a pulse, read
   through the emotion probe; the pulse itself is **not** stored as a memory).
4. **Dream consolidation** on one agent's memory: it **reshapes the memory set** —
   active memories drop as duplicates are merged, and a relative date like
   `"yesterday"` is **grounded** to an absolute date.
5. **Separately** (honestly named as a different section): **time + Ebbinghaus
   decay change what recall returns** — the same query returns a different top
   memory on day 1 vs day 8.
6. A protected identity anchor survives decay (retention stays 1.00) while
   fast-decay trivia fades.

> **Say the distinction correctly:** the **dream cycle reshapes the memory SET**
> (merges duplicates, grounds relative dates). **Decay** is what changes the
> **recall output** over time. These are two different mechanisms shown in two
> different sections. Do not blur them.

**Expected result:** all invariants asserted, process **exits 0**.

### 2.2 Crash proof — `crash_replay -- full`

**What it does:** performs a write, simulates a process crash at a defined
window, restarts, and replays from the durable journal — then verifies the
external action was dispatched **at most once**.

**Expected result:** the run asserts no duplicate external dispatch under the
tested crash windows and **exits 0**. This is the shortest deterministic proof
of crash-safe, at-most-once external dispatch.

### 2.3 Scorecard — `bench -- all`

**What it prints:** the deterministic reliability scorecard, `s1`..`s8`, all
PASS, `Overall: PASS`, plus the crash-safe dispatch summary vs a **pinned
LangGraph 1.2.6** reproduction:

```
Crash point                                  FamilyClaw   LangGraph
clean (no crash)                                  0           0
before_write (effect done, record not yet)        0           1
mid_replay  (re-crash during replay)              0           2
```

**Expected result:** `Overall: PASS`, exit 0. The numbers are the count of
money-touching external side effects that **re-execute** after a crash.

---

## 3. The 3-minute demo sequence (what to say + what to run)

Keep it to three moves. Talk while it runs; every demo is fast on a warm build.

### Move 1 — Flagship (≈75s)

**Say:** *"Long-running agents forget, duplicate work, and act on stale context.
Watch two agents share one memory bus — real delivery, real emotion contagion,
and a nightly dream cycle that cleans the memory set. This asserts every claim
live; if anything is false, the process dies."*

**Run:**
```
cargo run -p familyclaw-agent --example two_agents_memory
```

**Point at:** the dream line (memory set shrinks, date grounded) **and** the
separate decay line (different top memory on day 1 vs day 8). Name them as two
different mechanisms.

### Move 2 — Crash proof (≈60s)

**Say:** *"The scary part of agents that touch money is crashing mid-action and
doing it twice. We write, we kill the process, we restart, we replay from a
durable journal — and the external action fires at most once."*

**Run:**
```
cargo run -p familyclaw-agent --bin crash_replay -- full
```

**Point at:** exit 0 and the at-most-once assertion.

### Move 3 — Scorecard + LangGraph (≈45s)

**Say:** *"And it's not a one-off. Here's a deterministic 8-of-8 reliability
scorecard, plus a pinned-LangGraph crash-window benchmark. Under these crash
points, LangGraph re-fires the external effect one or two times. FamilyClaw:
zero. This measures duplicate side-effect dispatch under specific crash windows —
not speed, not model quality."*

**Run:**
```
cargo run -p familyclaw-bench --bin bench -- all
```

**Close:** *"All of that ran offline, no keys, no network, and every line was
asserted by the program itself."*

---

## 4. Fallback: network fails

**There is no network fallback needed — that's the point.** All three demos are
**offline by design**: no API keys, no network calls, no external services, no
Python environment. If the venue Wi-Fi is down, say so and use it:

> *"Good timing — none of this needs a network. It's pure Rust, no external
> services. Watch it run fully offline."*

A dead network is a **feature demonstration**, not a failure.

---

## 5. Fallback: a demo misbehaves live

If the flagship demo hiccups (rare), re-run it once. If it fails twice, do **not**
keep retrying in front of a visitor. Switch to:

1. The **crash proof** alone (§2.2) — it's the strongest single claim.
2. Then the **scorecard** (§2.3), which reads from committed artifacts.
3. If both are unstable on the machine, go to §6 (prebuilt binaries) or §7
   (prerecorded demo).

Never narrate around a broken run. Move to something that passes.

---

## 6. Fallback: compilation fails

If `cargo` won't build on the booth machine (toolchain, disk, cold cache):

1. **Use the prebuilt `booth/` binaries.** Preflight (§1) confirms they exist.
   Run the same three demos from the prebuilt binaries instead of `cargo run`.
2. **Show `docs/EXPO_VALIDATION_PROOF.md`** — the printed, exact, reproducible
   results of the full local verification suite (fmt, Layer B audit, build,
   clippy `-D warnings`, ~1721 tests, 8/8 scorecard, flagship + crash demos).
   This is our authoritative local proof.

Have a **printed copy of `EXPO_VALIDATION_PROOF.md`** at the booth. If the
machine is dead, the paper still proves what passes.

> Note honestly if asked: that proof is **local**. Hosted CI may be
> billing-blocked on a zero-spend account — do **not** claim hosted CI is green.

---

## 7. Fallback: prerecorded demo

Keep a short **screen recording** of all three demos running clean (same
sequence as §3) on the booth machine and on a phone.

- Use it when there's a queue, when the machine is rebuilding, or when a visitor
  only has 30 seconds.
- **Always label it as a recording.** *"This is a recording of the exact same
  command — I'm happy to run it live for you now if you'd like."*
- Never present a recording as a live run.

---

## 8. Claims the operator MAY safely make

Every one of these is grounded in verified facts:

- "It's a **Rust-native reliability runtime for long-running AI agents that take
  real external actions**."
- "**23 crates, v1.2.0, MIT**, `unsafe` forbidden across the whole workspace."
- "Around **1721 tests**, and a **deterministic 8-of-8 reliability scorecard**."
- "**Crash-safe dispatch**: after a crash, external actions fire **at most once**
  under the tested crash windows."
- "**Durable, deterministic replay** — journal plus replay, **no closure
  re-execution**, pure Rust, **no external services**."
- "**Content-hash-bound approval gates**: TOCTOU-safe, **fail-closed** — if the
  action payload doesn't match what was approved, it's rejected
  (`ApprovalPayloadMismatch`)."
- "**Auditable** — `/turns/audit` and `/approvals` endpoints, Prometheus metrics."
- "**Persistent memory with Ebbinghaus decay** and a **nightly dream cycle** that
  reshapes the memory set — merges duplicates, grounds relative dates. Decay,
  separately, changes what recall returns."
- "**Provider/model failover**: on 401/403 it rotates within the key pool before
  cooling a provider; on 429 it climbs a cooldown ladder (60s / 5m / 25m / 1h);
  auth failures climb a slower ladder (5m / 30m / 2h / 6h). Two-pass: healthy
  providers first, then a last-resort pass so it **never hangs**. It **fails
  closed** on non-retryable errors (Parse / InvalidTool)."
- "**Deny-by-default WASM sandbox** on Wasmtime + Cranelift, with **fuel
  metering**."
- "**Self-modification is impossible by construction** — there is no `apply()`
  function; growth/WASM can't rewrite the running system."
- "A genuine **pinned LangGraph 1.2.6** crash-window benchmark: under specific
  crash points, LangGraph re-fires external effects; FamilyClaw fires zero."
- "Everything you just saw ran **offline, no keys, no network**, and **asserted
  itself**."

---

## 9. Claims the operator MUST NOT make

Do not say, imply, or nod along to any of these:

- ❌ **No production deployments.** We have none.
- ❌ **No customers.** None.
- ❌ **No revenue.** None.
- ❌ **No certifications.** None (no SOC 2, no ISO, nothing).
- ❌ **No enterprise claims** ("enterprise-grade", "battle-tested in production",
  "used by teams", etc.).
- ❌ Never say **"27 crates."** It is **23**.
- ❌ Never claim the **dream cycle changes recall output.** It reshapes the memory
  **set**; **decay** changes the output.
- ❌ Never claim **hosted CI passed.** The proof is **local**; hosted CI may be
  billing-blocked.
- ❌ Do not publish the **private repo URL** as a public link. Say **"private demo
  access on request."**
- ❌ Do not turn the LangGraph benchmark into a **throughput / latency / usability
  / model-quality** claim. It only measures duplicate external side-effect
  dispatch under specific crash windows.

If unsure whether a claim is safe, **don't make it.** Offer to show the code or
the validation proof instead.

---

## 10. Common technical Q&A

**Q: How is it crash-safe? What if it dies mid-action?**
It journals intent durably before dispatch and replays deterministically on
restart — **no closure re-execution**, pure Rust, no external services. External
actions are **at-most-once** under the tested crash windows. Live proof:
`crash_replay -- full`.

**Q: What does failover do on a 401 vs a 429?**
Different ladders. **401/403 (auth):** rotate within the key pool first, then
cool the provider on a slow ladder (5m / 30m / 2h / 6h). **429 (rate limit):**
climb an escalating cooldown ladder (60s / 5m / 25m / 1h). It runs **two passes**
— healthy providers first, then a last-resort pass that retries everything so it
**never hangs**. On non-retryable errors (Parse / InvalidTool) it **fails
closed** rather than guessing.

**Q: What does the LangGraph benchmark actually measure — and not measure?**
It measures how many **money-touching external side effects re-execute after a
crash** at specific crash windows, against a **pinned LangGraph 1.2.6**. Result:
LangGraph 1–2, FamilyClaw 0. It does **NOT** measure throughput, latency,
usability, or model quality. It's a correctness benchmark, narrow on purpose.

**Q: Can an agent modify itself / escape the sandbox?**
No. Self-modification is **impossible by construction** — there is no `apply()`
function to commit a change back into the running system. The WASM sandbox
(Wasmtime + Cranelift) is **deny-by-default** with **fuel metering**, so guest
code can't reach out or run unbounded.

**Q: How do approval gates work?**
They're **content-hash-bound**. The approval is tied to the exact action payload;
if the payload changes between approval and execution, the mismatch is caught
(`ApprovalPayloadMismatch`) and the action is **rejected — fail-closed**. That
closes the TOCTOU (time-of-check/time-of-use) hole.

**Q: What's Layer A vs Layer B?**
**Layer A** is the public, publishable runtime — the code you see running here.
**Layer B** is our private internal layer (souls, keys, profiles). A build-time
audit (`scripts/audit-layer-b.sh`) enforces that **no Layer B material leaks into
Layer A**. That separation is why the booth runs from a clean export and why repo
access is private-on-request.

**Q: Is it production-ready? Who uses it?**
Honest answer: **no production deployments and no customers yet.** What we have
is a rigorously tested runtime (~1721 tests, 8/8 scorecard, `unsafe` forbidden)
and reproducible proofs. That's exactly why we're here — to find the first pilot.

---

## 11. Commercial offers (have these ready)

Two offers. Anchor at the **top** of each range with a defined scope; the lower
end is the concession for a **narrower** engagement.

| Offer | What it is | Price |
|-------|-----------|-------|
| **A — AI Agent Reliability Review** | A focused review of an agent workflow for the failure modes FamilyClaw is built to prevent (duplicate actions, crash recovery, approval gaps, failover). | **EUR 750–1500** |
| **B — AI Agent Reliability Sprint** | A focused **5-day pilot** hardening **one real agent workflow**. Founding-pilot engagement. | **EUR 1500–3500** |

Lead with **anchor + scope**: "The Reliability Sprint is a focused 5-day pilot on
one real agent workflow, EUR 3500. If the scope is narrower, we can do it from
EUR 1500." Same pattern for the Review (EUR 1500 anchor, EUR 750 for a tighter
scope).

---

## 12. Contact

- **The FamilyClaw Authors** — `viltsu.operator@gmail.com`
- **GitHub:** `Sisuthros`
- **Repo:** **private** — offer **private demo access on request**. Do not hand
  out a repo URL.

---

## Appendix — Internal ops only (do NOT read out to visitors)

**Family council roles** are defined in the internal operating model, which is a
**Layer B document kept OUTSIDE this repository** (it names real team members and
private operating structure — it must never enter publishable content). The
operator (the operator) holds it in the private workspace, not in `docs/`.

**Autonomy boundary** (safe to keep here — names only the operator and "agents"
generically). Agents may autonomously research, analyze, draft, code in
branches, test, prepare demos, and prepare outreach drafts. **the operator approval is
REQUIRED before:** sending messages, publishing, merging to main, deployments,
spending money, signing agreements, accessing customer production systems,
handling customer secrets, or any destructive action.
