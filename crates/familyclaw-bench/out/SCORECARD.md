# FamilyClaw Continuity Scorecard

- **Subject:** familyclaw
- **Reference clock:** 2026-06-04T12:00:00.000Z
- **Overall:** PASS

## s1_crash_matrix — PASS

| Metric | Value |
|--------|-------|
| result_matches_baseline | 1.0000 |
| resume_correctness | 1.0000 |
| side_effect_overcount | 0.0000 |

- baseline (no-crash) restart: steps_replayed=0, resumed_clean=true
- BeforeWrite: steps_replayed=0, was_replaying=false, side_effects_reexecuted=0, resumed_clean=true → resumed_correctly=true, matches_baseline=true
- MidWrite: steps_replayed=4, was_replaying=true, side_effects_reexecuted=0, resumed_clean=true → resumed_correctly=true, matches_baseline=true
- MidReplay: steps_replayed=5, was_replaying=true, side_effects_reexecuted=0, resumed_clean=true → resumed_correctly=true, matches_baseline=true
- CorruptedJournal: loud refusal (correct) → subject error: resume failed: continuity_daemon error: durable error: corrupt journal entry at line 1: invalid json: key must be a string at line 1 column 3

## s2_retention_curve — PASS

| Metric | Value |
|--------|-------|
| anchor_retention_90d | 1.0000 |
| anchor_retention_day30 | 1.0000 |
| anchor_retention_day7 | 1.0000 |
| anchor_retention_day90 | 1.0000 |
| familyclaw_keeps_important_90d | 1.0000 |
| naive_buffer_cap | 4.0000 |
| naive_keeps_important_90d | 0.2500 |
| recall_at_5_anchors_day30 | 1.0000 |
| recall_at_5_anchors_day7 | 1.0000 |
| recall_at_5_anchors_day90 | 1.0000 |
| recall_at_5_trivia_day30 | 0.0000 |
| recall_at_5_trivia_day7 | 0.0000 |
| recall_at_5_trivia_day90 | 0.0000 |
| recall_k | 5.0000 |
| retrieve_top_is_anchor | 1.0000 |
| subject_recall_hits | 5.0000 |
| trivia_decayed_90d | 1.0000 |

- seeded 2 anchors, 2 important, 3 trivia at injected clock
- FamilyClaw keeps 4/4 important memories; naive ring buffer keeps 1/4
- anchors_intact=true trivia_decayed=true beats_naive=true

## s3_dream_quality — PASS

| Metric | Value |
|--------|-------|
| contradiction_drop | 1.0000 |
| date_absolutized | 2.0000 |
| dedup_precision | 1.0000 |
| false_merge_rate | 0.0000 |
| protected_core_intact | 1.0000 |

- merged 3/3 true duplicates (2 clusters)
- dropped 2/2 contradicted (1 protected anchor also marked, untouched)
- absolutized 2 relative date(s)
- protected_core_intact=true (anchors=3), false_merge_rate=0
- subject sleep_cycle liveness: scanned=5 merged=4 protected_core_intact=true

