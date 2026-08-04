# DRAFT — I crash-tested LangGraph's strongest durability mode. Here's the window where it double-fires.

> **Status:** publication draft (Horisontti 2 launch asset). Publish under the
> author's own name (personal blog / dev.to / HN). Numbers below are pinned to
> [bench-competitors/langgraph/RESULTS.md](../bench-competitors/langgraph/RESULTS.md)
> — regenerate before publishing if the harness changes.
>
> **Purpose:** this post is the credibility artifact. FamilyClaw is the *answer*
> in the post, not the topic. The topic is the failure window every agent
> developer with money-touching side effects should know about.

---

## The setup

Long-running agents crash. Everyone knows this, and every serious framework has
an answer: checkpointing. LangGraph's `SqliteSaver` with `durability="sync"` —
its strongest mode — persists a node's writes synchronously after the node
returns, before the next step starts.

So I asked one narrow question:

> **After a SIGKILL, how many money-touching external side effects re-execute?**

The correct answer is zero. A duplicate refund, a duplicate teardown, a
duplicate migration step — these cost real money and real trust.

## The harness

No LLM, no API keys, fully deterministic and reproducible from one `git clone`:

- A 4-node linear `StateGraph` (`langgraph==1.2.6`,
  `langgraph-checkpoint-sqlite==3.1.0`, Python 3.13), compiled with
  `SqliteSaver`, invoked with `durability="sync"`.
- Each node bumps an **on-disk counter** — a stand-in for an external mutation
  (a refund, an API write). The bump lives *inside the node body*, which is
  where real side effects live.
- A cross-process SIGKILL (`os._exit(137)`) fires inside the armed node
  **after** the side effect but **before** the node returns — i.e. before its
  post-node checkpoint.
- A fresh child process resumes via `graph.invoke(None, config)` from the same
  `thread_id`.

## The result

Overcount = side effects that fired minus side effects that should have fired
exactly once. Lower is better; 0 is correct.

| Crash point | LangGraph (`durability="sync"`) | Naive file-memory baseline |
|---|:--:|:--:|
| `clean` — no crash | **0** | 0 |
| `before_write` — effect fired, checkpoint not yet written | **1** | 4 |
| `mid_replay` — re-crash during the resume itself | **2** | 5 |

The restart report proves the resume genuinely read state from disk
(`checkpoint_existed: true`, `next_before_resume: ["step-1"]`) — and then
re-ran the node whose side effect had already fired.

## Why this happens (and why it's not a LangGraph bug)

To be clear about what I am **not** claiming: LangGraph's durable state replay
works. It resumes from the correct checkpoint. A crash strictly *between* nodes
does not re-fire anything. I did not cripple the checkpointer — `sync` is its
strongest mode.

The issue is structural: the checkpoint barrier sits **between** nodes, so the
external side effect and the "node done" record are **not atomic**. Kill the
process in the window between them and the node is still marked pending, so the
resume re-runs it — and re-fires the effect. Under `mid_replay` it compounds.

**Durable state replay and at-most-once external side effects are two different
guarantees.** Most checkpointing systems give you the first. Money-touching
work needs the second.

## Closing the window

The fix is the transactional-outbox idea applied to agent side effects: an
**idempotency-keyed intent → effect → committed** protocol.

- Before dispatch, durably record an *intent* keyed by an idempotency key.
- After the effect, durably record the *committed* outcome.
- On resume: a committed record replays the recorded outcome **without
  re-running** the effect. An intent-only record (the crash window above)
  **fails closed** — the effect is not silently re-fired; recovery requires an
  explicit decision.

That last point matters: this is honest **at-most-once dispatch**, not magical
exactly-once *completion*. A crash in the intent-only window means 0-or-1
executions and a loud stop — never 2.

I implemented this in [FamilyClaw](https://github.com/Sisuthros/familyclaw-oss), a
Rust agent runtime (MIT, `unsafe` forbidden workspace-wide). Same harness
shape, same metric:

| Crash point | FamilyClaw |
|---|:--:|
| `clean` | **0** |
| `before_write` | **0** |
| `mid_replay` | **0** |
| corrupted journal | loud refusal (no silent re-run) |

The cross-process red-team test is the proof I care most about: the *old* path
(no outbox) reproduces the double-fire (`side_effect_count = 2`); the outbox
path holds at exactly 1 across a real process boundary, and the intent-only
window fails closed with a policy denial.

## Reproduce it yourself

Everything is pinned; no LLM runs in either harness, so every run is
byte-reproducible:

```bash
git clone https://github.com/Sisuthros/familyclaw-oss
cd familyclaw

# FamilyClaw side (Rust):
cargo build -p familyclaw-agent --bin continuity_daemon
cargo run -p familyclaw-bench --bin bench -- all     # 8-scenario scorecard, expects ALL PASSED

# LangGraph side (Python 3.13):
cd bench-competitors/langgraph
python -m venv .venv
.venv/bin/python -m pip install langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0
.venv/bin/python crash_harness.py cycle --crash-point before_write --workdir _runs/bw
cat _runs/bw/side_effect_counter.txt   # -> 5 (overcount 1)
```

Every `main` CI run also regenerates the scorecard and publishes it in the run
summary, so you can check the claim without installing anything.

## What this doesn't claim

1. Not "LangGraph is broken" — its state replay is real and works.
2. Not exactly-once *completion* — duplicate-prevention under crash, fail-closed
   in the narrow window.
3. Not a throughput/cost/ecosystem comparison — LangGraph wins on breadth and
   tooling; that's not what this measures.

If your agent only reads and summarizes, none of this matters — stay in Python.
If it mutates the world and a crash costs money, run the bench and check my
work.

---

*Discussion: [GitHub](https://github.com/Sisuthros/familyclaw-oss) — I'm looking
for the first external person to run `familyclaw serve` in their own
environment and report back. That report is worth more to me than stars.*
