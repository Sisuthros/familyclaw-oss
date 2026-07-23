# RESULTS — FamilyClaw vs Hermes-shaped MEMORY.md (~2.2k) model

> **Scope.** Single-metric crash-safety under process crash. **Not** a live
> Hermes Agent product audit. Subject = Hermes-*shaped* model of documented
> file-memory limits + restart re-run of incomplete steps.

**Verified:** 2026-07-23 (Python stdlib only).

## Overcount table

| Crash point | FamilyClaw | Hermes-shaped |
|---|:--:|:--:|
| `clean` | **0** | **0** |
| `before_write` | **0** | **1** |
| `mid_replay` | **0** | **2** |

## Verdict

Hermes-shaped restart re-fires incomplete steps → `before_write` overcount **1**,
`mid_replay` overcount **2**. FamilyClaw stays at **0**. Replace the shaped
column only after a pinned live Hermes Subject adapter exists.
