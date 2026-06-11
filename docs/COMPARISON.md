# FamilyClaw vs Baseline — Continuity Comparison

> **Honesty note:** the baseline is a *competitor-SHAPED model* (a `MEMORY.md` that truncates oldest-first + side effects re-run on restart), **NOT** a real OpenClaw / Hermes Agent instance. It models the documented failure modes those file-based memories exhibit — it does not claim to be any real product's internals.

- **Reference clock (injected):** 2026-06-04T12:00:00.000Z
- **FamilyClaw subject:** familyclaw
- **Baseline subject:** markdown-file-baseline

## Summary

| Subject | Overall |
|---------|---------|
| familyclaw (FamilyClaw) | PASS |
| markdown-file-baseline (baseline) | FAIL |

## s1_crash_matrix

| Dimension | FamilyClaw | Baseline |
|-----------|------------|----------|
| result | PASS | FAIL |
| side_effect_overcount | 0.0000 | 17.0000 |
| resume_correctness | 1.0000 | 0.0000 |

## s2_retention_curve

| Dimension | FamilyClaw | Baseline |
|-----------|------------|----------|
| result | PASS | PASS |
| anchor_retention_90d | 1.0000 | 1.0000 |
| subject_recall_hits | 5.0000 | 0.0000 |

## s3_dream_quality

| Dimension | FamilyClaw | Baseline |
|-----------|------------|----------|
| result | PASS | PASS |

## s4_emotional_contagion

| Dimension | FamilyClaw | Baseline |
|-----------|------------|----------|
| result | PASS | PASS |
| subject_recall_hits | 1.0000 | 0.0000 |

## s5_semantic_retrieval

| Dimension | FamilyClaw | Baseline |
|-----------|------------|----------|
| result | PASS | PASS |

## s6_eternal_thread

| Dimension | FamilyClaw | Baseline |
|-----------|------------|----------|
| result | PASS | PASS |

## Verdict

On **S1 Crash Matrix**, FamilyClaw re-executes `side_effect_overcount: 0` side effects across every crash point and passes; the baseline re-runs `> 0` side effects on restart and fails. Durable replay runs each side effect exactly once — the truncating file-memory baseline cannot.
