# familyclaw-durable

**Durable substrate — deterministic replay (crash-proof).**

Layer 1 of the FamilyClaw platform (design §2.1) and the **structural
solution to a family's pain point #1 — memory discontinuity**. Durable
execution turns continuity of work into *structure*: if the process
crashes, the workflow resumes exactly where it left off, without replaying
side effects.

## Model

Journal-based deterministic replay (the Temporal/Flawless model in pure
Rust; no wasmtime at this stage):

1. The workflow is wrapped into steps via `DurableContext::step`.
2. Every completed step is written as a `JournalEntry` to an append-only
   `Journal`.
3. On restart, `DurableContext` is rebuilt from the same journal, and
   steps that already ran **are restored from the log without re-running
   their closures** → side effects are not repeated, the result is the same.

## Public API

| Type | Responsibility |
|--------|--------|
| `DurableContext<J>` | `step(name, closure)` API; replay cursor, snapshot, finish |
| `Journal` (trait) | append-only log: `append`, `replay_from`, `snapshot`, `len` |
| `InMemoryJournal` | non-durable implementation for testing/development |
| `FileJournal` | crash-safe append-only JSONL (`flush` + `fsync`) |
| `JournalEntry`, `EntryKind`, `StepId` | journal rows |
| `DurableError`, `Result` | error types (convert into `FamilyClawError`) |

## Example

```rust
use familyclaw_durable::{DurableContext, InMemoryJournal};

// Fresh run: the closure runs and the result is written to the log.
let mut ctx = DurableContext::new(InMemoryJournal::new())?;
let greeting: String = ctx.step("greet", || Ok("hello".to_string()))?;

// "Crash": save the journal, rebuild the context.
let journal = ctx.finish();
let mut resumed = DurableContext::new(journal)?;

// Replay: the step is restored from the log — the closure is NOT re-run.
let again: String = resumed.step("greet", || Ok("DIFFERENT".to_string()))?;
assert_eq!(again, "hello"); // the stored value, not a new value from the closure
```

## Crash safety

- `FileJournal::append` flushes and fsyncs (`File::sync_all`) before
  returning → a completed step is on disk even after a sudden crash.
- Replay tolerates exactly one case: an incomplete **last** line (missing
  newline) left mid-write when a crash occurred. Any *earlier* corrupted
  line comes back as `DurableError::CorruptEntry`.

## Determinism invariant

The code must produce the same steps (same name, same order) on every run.
If replaying code requests a step whose name doesn't match the one at the
same position in the journal, `step` returns
`DurableError::NondeterministicReplay` instead of silently continuing
incorrectly.

## OSS boundary (Layer A)

Generic platform code: no hardcoded souls, keys, tokens, IP addresses, or
personal paths. The journal path is supplied at runtime.
