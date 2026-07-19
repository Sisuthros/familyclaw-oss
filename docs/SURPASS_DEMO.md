# FamilyClaw — Surpass Demo

> **What this proves:** FamilyClaw's *durable crash-replay* + *eternal-thread recall* beats
> file-based agent memory (an OpenClaw/Hermes-style `MEMORY.md`) **with a single command,
> deterministically, byte-for-byte reproducibly.**

## Honesty first

The comparison's "baseline" is NOT a live OpenClaw or Hermes instance. It is a
`markdown-file-baseline` — a **competitor-SHAPED model** that mirrors these platforms'
*documented* failure modes:

- A `MEMORY.md` buffer that **silently truncates the oldest entries** once the bootstrap budget (8) is exceeded
- **No deterministic crash replay** → a restart **re-runs side effects**
- No protected core / no decay policy — identity facts get truncated just like everything else

It does not claim to be the internal implementation of any product. The artifact proves
*"beats a competitor-shaped baseline"*, not *"beats the OpenClaw/Hermes product"*.

## Run it yourself (one command)

```bash
cargo +stable-x86_64-pc-windows-msvc run -p familyclaw-bench --bin bench -- compare
```

Produces `crates/familyclaw-bench/out/COMPARISON.md` + `docs/COMPARISON.md` (byte-identical).

## Result (live-run 2026-06-10, injected at clock 2026-06-04T12:00:00Z)

| Subject | Overall |
|---------|---------|
| familyclaw (FamilyClaw) | **PASS** |
| markdown-file-baseline | **FAIL** |

### S1 — Crash Matrix (the decisive difference)

| Metric | FamilyClaw | Baseline |
|---------|:----------:|:--------:|
| result | **PASS** | **FAIL** |
| side_effect_overcount | **0** | **17** |
| resume_correctness | **1.0** | **0.0** |

FamilyClaw is run as a **genuine OS subprocess** (the `continuity_daemon` binary), killed
at four different crash points (BeforeWrite / MidWrite / MidReplay / CorruptedJournal),
restarted, and proven to re-run **zero** side effects
(durable journal replay), recovering to exactly the state before the crash. The baseline
re-runs 17 side effects and fails. This crosses a **genuine process boundary** — not an
in-process library fake.

### S2/S4 — Recall across restart

| Metric | FamilyClaw | Baseline |
|---------|:----------:|:--------:|
| subject_recall_hits (S2) | **5** | **0** |
| subject_recall_hits (S4) | **1** | **0** |

The baseline's naive memory truncated the facts → recall returns empty. FamilyClaw's
eternal thread remembers them across a process restart.

## What this is AND what it is not

**IS:** a reproducible, single-command artifact that proves durable crash replay (S1,
across a genuine process boundary) + recall-across-restart. A skeptic can run the binary themselves.

**NOT YET:** run against a live competitor. The next step is to wrap a real
OpenClaw/Hermes runtime behind the same `Subject` interface (the seam is already designed
exactly for this) → then the crash matrix kills a real competitor process.

## Bonus: gateway security regression closed

`POST /inject` previously required no authentication at all (vs. OpenClaw's `OPENCLAW_GATEWAY_TOKEN`).
Now: if `FAMILYCLAW_GATEWAY_TOKEN` is set, `/inject` requires `Authorization: Bearer <token>`
(constant-time comparison, no token leakage into logs) → 401 without it, 200 with the correct one. `/healthz` + `/readyz`
remain open (for probes). Tested (401-without / 200-with-correct / back-compat-without-token).
