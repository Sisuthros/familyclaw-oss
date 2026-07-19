---
name: Benchmark regression
about: Report a regression in the continuity benchmark or scorecard
title: "[BENCH] "
labels: benchmark, regression
assignees: ''
---

## Scenario affected

Which `familyclaw-bench` scenario regressed (e.g. `s1` crash matrix, retention,
dream quality)?

## Command run

```bash
cargo run -p familyclaw-bench --bin bench -- all
```

## Scorecard: before vs. after

Attach or paste both `scorecard.json` files (or the relevant diff), and the
commit/tag each was generated from.

**Before:**
```json
(paste here)
```

**After:**
```json
(paste here)
```

## What changed

The commit, PR, or dependency bump you believe introduced the regression, if
known.

## Impact

- [ ] `side_effect_overcount` increased (crash-safety regression — highest priority)
- [ ] Retention/decay metrics diverged
- [ ] Dream consolidation quality diverged
- [ ] Other (describe)

## Additional context

Anything else useful for triage — flakiness on retry, platform-specific, etc.
