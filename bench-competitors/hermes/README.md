# Hermes Agent-shaped competitor harness — crash-safety (S1)

Models **Hermes-style file memory** (~2,200 character `MEMORY.md` budget) with
**no idempotency-keyed external dispatch**. Same metric as
[`../langgraph/`](../langgraph/) and [`../openclaw/`](../openclaw/).

> **Honesty:** the default path is still a Hermes-*shaped* model, not a live
> product claim. If you set `HERMES_BIN` to a real pinned binary that implements
> the harness contract, `crash_harness.py` now runs that binary for
> `run`/`restart` phases and records its SHA-256 plus `--version` output in
> `cycle_report.json`. See [`RESULTS.md`](RESULTS.md).

## Reproduce

```bash
cd bench-competitors/hermes
python crash_harness.py cycle --crash-point clean        --workdir _runs/clean
python crash_harness.py cycle --crash-point before_write --workdir _runs/before_write
python crash_harness.py cycle --crash-point mid_replay   --workdir _runs/mid_replay

# optional: real pinned binary, same CLI contract
HERMES_BIN=/path/to/hermes \
python crash_harness.py cycle --crash-point mid_replay --workdir _runs/live-mid-replay
```

## One-line result

| Crash point | FamilyClaw | Hermes-shaped |
|---|:--:|:--:|
| `clean` | **0** | **0** |
| `before_write` | **0** | **1** |
| `mid_replay` | **0** | **2** |

## Nightly CI

The repository's nightly crash-matrix workflow runs the shaped Hermes/OpenClaw
matrix on GitHub Actions, verifies the expected overcounts (`0/1/2`), and
regenerates `bench-competitors/MATRIX.md`. That file is a build product, not
committed — run `scripts/run-competitor-crash-matrix.sh` to generate it
locally. Live-pinned mode stays opt-in and
local because it depends on a real pinned binary path.
