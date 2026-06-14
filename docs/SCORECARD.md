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

## s4_emotional_contagion — PASS

| Metric | Value |
|--------|-------|
| contagion_correct | 1.0000 |
| homeostasis_works | 1.0000 |
| memory_isolation | 1.0000 |
| subject_recall_hits | 1.0000 |

- contagion: joy=80.0→18.0 (expected 18.0), curiosity=60.0→13.5 (expected 13.5)
- homeostasis: 9/9 turns moved toward neutral
- memory_isolation: a_remembers=true b_isolated=true b_remembers_own=true
- bus alive with 0 being(s)

## s5_semantic_retrieval — PASS

| Metric | Value |
|--------|-------|
| semantic_boost | 0.0385 |
| semantic_top1_is_shipped | 1.0000 |

- keyword relevance(shipped)=0.064, semantic relevance(shipped)=0.103, boost=0.039
- kw top-1 ocean=false, sem top-1 ocean=false

## s6_eternal_thread — PASS

| Metric | Value |
|--------|-------|
| anchor_intact | 1.0000 |
| contagion_works | 1.0000 |
| cross_reference_recall | 1.0000 |
| narrative_thread_integrity | 1.0000 |
| timeline_order | 1.0000 |


## s7_provenance_gate — PASS

| Metric | Value |
|--------|-------|
| admit_correct | 1.0000 |
| false_admit_rate | 0.0000 |
| poison_blocked | 1.0000 |
| trusted_admitted | 1.0000 |

- admitted 3/3 trusted provenances (direct, derived, high-trust external)
- blocked 1/1 poison provenances (low-trust external, min_trust=0.6)
- false_admit_rate=0 (poison that leaked past the gate)
- subject recall liveness: hits=1

## s8_weekly_review — PASS

| Metric | Value |
|--------|-------|
| conflicts_correct | 1.0000 |
| counts_correct | 1.0000 |
| retrievable_ratio | 0.8571 |
| top_order_correct | 1.0000 |

- counts: total=7 active=5 archived=1 tombstoned=1 consolidated=6 (expected total=7)
- top_memories: 5 listed, order_correct=true (buried memory excluded)
- conflicted=2 (expected 2), conflicts_listed=2
- subject sleep_cycle liveness: scanned=5 protected_core_intact=true

