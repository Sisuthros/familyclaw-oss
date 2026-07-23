# OpenClaw-shaped competitor harness — crash-safety (S1)

Standalone, reproducible benchmark modeling **OpenClaw's documented
file-memory + restart-re-run** failure mode on the same metric as
[`../langgraph/`](../langgraph/): **how many external side effects re-execute
after a process crash?** (target: 0).

> **Honesty:** this is an OpenClaw-*shaped* process model (`MEMORY.md`
> bootstrap budget, no idempotency-keyed dispatch), not a pinned live OpenClaw
> binary. Set `OPENCLAW_BIN` to note live intent; the numeric matrix remains
> this shaped model until a product-level Subject adapter lands. See
> [`RESULTS.md`](RESULTS.md).

## Reproduce

```bash
cd bench-competitors/openclaw
python crash_harness.py cycle --crash-point clean        --workdir _runs/clean
python crash_harness.py cycle --crash-point before_write --workdir _runs/before_write
python crash_harness.py cycle --crash-point mid_replay   --workdir _runs/mid_replay
```

## One-line result

| Crash point | FamilyClaw | OpenClaw-shaped |
|---|:--:|:--:|
| `clean` | **0** | **0** |
| `before_write` | **0** | **1** |
| `mid_replay` | **0** | **2** |

FamilyClaw side: `cargo test -p familyclaw-actions --test redteam_dispatch_exactly_once`
and `cargo run -p familyclaw-bench -- s1`.
