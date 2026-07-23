# RESULTS — FamilyClaw vs OpenClaw-shaped MEMORY.md model

> **Scope.** Single-metric crash-safety: side-effect re-execution under process
> crash. **Not** a live OpenClaw product audit. The subject is an
> OpenClaw-*shaped* model of documented failure modes (file `MEMORY.md`,
> restart re-runs incomplete steps). Same metric as
> [`../langgraph/RESULTS.md`](../langgraph/RESULTS.md).

**Verified:** 2026-07-23 (Python 3.x, no deps beyond stdlib).

## Versions

- Subject: `openclaw-shaped-memory-md` (`memory_md_agent.py`)
- Steps: 4; crash via `os._exit(137)` after side effect, before durable "completed"
- Live binary: optional `OPENCLAW_BIN` (notes live intent; matrix uses shaped model)

## Overcount table

| Crash point | FamilyClaw | OpenClaw-shaped | Markdown control |
|---|:--:|:--:|:--:|
| `clean` | **0** | **0** | **0** |
| `before_write` | **0** | **1** | ≥1 |
| `mid_replay` | **0** | **2** | ≥2 |

Reproduce:

```bash
python crash_harness.py cycle --crash-point before_write --workdir _runs/before_write
# side_effect_overcount == 1
```

## Verdict

Under SIGKILL after an external side effect but before a durable "step done"
record, the OpenClaw-shaped model **re-fires** the effect on resume. FamilyClaw's
idempotency-keyed outbox keeps `side_effect_overcount = 0` at every crash point.

When a pinned live OpenClaw Subject adapter exists, re-run this matrix and
replace the "shaped" column — do not claim "beats OpenClaw product" until then.
