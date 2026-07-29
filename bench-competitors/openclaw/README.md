# OpenClaw-shaped competitor harness — crash-safety (S1)

Standalone, reproducible benchmark modeling **OpenClaw's documented
file-memory + restart-re-run** failure mode on the same metric as
[`../langgraph/`](../langgraph/): **how many external side effects re-execute
after a process crash?** (target: 0).

> **Honesty:** the default path is still an OpenClaw-*shaped* process model
> (`MEMORY.md` bootstrap budget, no idempotency-keyed dispatch). If you set
> `OPENCLAW_BIN` to a real pinned binary that implements the harness contract,
> `crash_harness.py` now runs that binary for `run`/`restart` phases and records
> its SHA-256 plus `--version` output in `cycle_report.json`. See
> [`RESULTS.md`](RESULTS.md).

## Reproduce

```bash
cd bench-competitors/openclaw
python crash_harness.py cycle --crash-point clean        --workdir _runs/clean
python crash_harness.py cycle --crash-point before_write --workdir _runs/before_write
python crash_harness.py cycle --crash-point mid_replay   --workdir _runs/mid_replay

# optional: real pinned binary, same CLI contract
OPENCLAW_BIN=/path/to/openclaw \
python crash_harness.py cycle --crash-point before_write --workdir _runs/live-before-write
```

## One-line result

| Crash point | FamilyClaw | OpenClaw-shaped |
|---|:--:|:--:|
| `clean` | **0** | **0** |
| `before_write` | **0** | **1** |
| `mid_replay` | **0** | **2** |

FamilyClaw side: `cargo test -p familyclaw-actions --test redteam_dispatch_exactly_once`
and `cargo run -p familyclaw-bench -- s1`.

## Nightly CI

The repository's nightly crash-matrix workflow runs the shaped OpenClaw/Hermes
matrix on GitHub Actions, verifies the expected overcounts (`0/1/2`), and
regenerates `bench-competitors/MATRIX.md`. That file is a build product, not
committed — run `scripts/run-competitor-crash-matrix.sh` to generate it
locally. Live-pinned mode stays opt-in and
local because it depends on a real pinned binary path.
