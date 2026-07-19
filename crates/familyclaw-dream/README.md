# familyclaw-dream

**Dreaming — nightly memory consolidation (hippocampal model).**

The "sleep" phase of the `FamilyClaw` platform (Layer A, OSS). Mirrors
Anthropic's Dreaming model (2026-05-06) and a family's Amplifier memory
prosthesis as native memory maintenance: a nightly `DreamCycle` reads
memories from [`familyclaw-memory`] storage and conflict data from the
durable journal, and cleans up memory in five phases.

## Five phases

1. **`merge_duplicates`** — near-identical memories are merged into a
   single reinforced representative (emotions + tags are unioned, the
   rest are tombstoned). Similarity is a dependency-free Jaccard word set.
2. **`drop_contradicted`** — memories the durable journal has flagged as
   contradicted are tombstoned. The journal is the source of truth — the
   dream cycle doesn't guess.
3. **`absolutize_dates`** — relative date words ("yesterday", "tomorrow")
   are converted to absolute ISO dates (`<word> (YYYY-MM-DD)`). Concretely
   solves the "yesterday expires" problem.
4. **`consolidate`** — high-importance memories are reinforced, low-retention
   (R < threshold) memories are archived.
5. produces a `DreamReport` in which every phase records its `Reflection`.

Phases run in a fixed order → the same input produces the same report
(deterministic, repeatable).

## Identity anchors are sacred

No phase ever tombstones or archives a `ProtectedCore` memory — identity
does not decay during sleep (anchor λ = 0.0).

## Public API

| Type / function | Responsibility |
|------------------|--------|
| `DreamCycle` | the dream-cycle engine (`run`, `run_without_journal`) |
| `DreamConfig` | phase thresholds + switches |
| `DreamReport` / `Reflection` / `ReflectionKind` | result report |
| `mark_contradicted` / `contradicted_ids` | conflict markers in the journal |
| `jaccard` / `is_near_duplicate` | text similarity |
| `absolutize` / `AbsolutizeResult` | date absolutization |

## Example

```rust,ignore
use familyclaw_dream::{DreamCycle, DreamConfig};
use familyclaw_memory::{LocalJsonStore, Memory, MemoryStore};
use familyclaw_durable::InMemoryJournal;

let store = LocalJsonStore::in_memory();
store.add(Memory::builder("we shipped the release").build()).await?;
store.add(Memory::builder("we shipped the release").build()).await?; // duplikaatti

let journal = InMemoryJournal::new();
let cycle = DreamCycle::with_config(&store, DreamConfig::default());
let report = cycle.run(&journal, familyclaw_core::time::now()).await?;
assert!(report.merged >= 1);
```

## OSS boundary (Layer A)

Generic platform code. No hardcoded souls, calibrations, keys, tokens, IP
addresses, or personal paths. All family-specific memories and thresholds
are supplied at runtime.

[`familyclaw-memory`]: ../familyclaw-memory
