# Hermes Agent-shaped competitor harness — crash-safety (S1)

Models **Hermes-style file memory** (~2,200 character `MEMORY.md` budget) with
**no idempotency-keyed external dispatch**. Same metric as
[`../langgraph/`](../langgraph/) and [`../openclaw/`](../openclaw/).

> **Honesty:** Hermes-*shaped* model, not a live Hermes binary. `HERMES_BIN`
> notes live intent; numeric matrix uses this shaped model. See
> [`RESULTS.md`](RESULTS.md).

## Reproduce

```bash
cd bench-competitors/hermes
python crash_harness.py cycle --crash-point clean        --workdir _runs/clean
python crash_harness.py cycle --crash-point before_write --workdir _runs/before_write
python crash_harness.py cycle --crash-point mid_replay   --workdir _runs/mid_replay
```

## One-line result

| Crash point | FamilyClaw | Hermes-shaped |
|---|:--:|:--:|
| `clean` | **0** | **0** |
| `before_write` | **0** | **1** |
| `mid_replay` | **0** | **2** |
