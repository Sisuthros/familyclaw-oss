# familyclaw-memory

**Eternal Thread** — the memory substrate of the FamilyClaw platform
(Layer A, OSS).

Gives beings *continuous memory*: memories don't vanish on restart, but
decay according to a biological forgetting curve (Ebbinghaus), strengthen
through repetition, and preserve identity anchors forever. Solves a
family's pain point #1 — memory discontinuity — *as structure*, not as a
reminder (design §2.1).

## Key types

| Type | Responsibility |
|--------|--------|
| `Memory` | A single memory: content, VAD emotional tone, named emotions, importance, decay policy, lifecycle state. Build with `Memory::builder(...)`. |
| `DecayPolicy` | Forgetting rate (Ebbinghaus λ): `ProtectedCore` (0.0), `Slow` (0.02), `Normal` (0.18), `Fast` (0.5). |
| `ImportanceFactors` | Combined importance: `emotion·0.45 + identity·0.35 + novelty·0.12 + reinforcement·0.20`. |
| `MemoryStatus` | Lifecycle `Active → Archived → Tombstoned`. |
| `MemoryStore` | Storage abstraction (async). |
| `LocalJsonStore` | Dependency-free default implementation (JSON file, atomic write). |
| `RetrievalContext` / `RetrievalResult` | Retrieval: keyword + emotional match + retention. |

## Ebbinghaus retention

```text
R(t) = e^(-λ · t / S)
```

- `λ` = the `DecayPolicy` constant (`ProtectedCore` → never decays),
- `S` = strength, derived from importance (a more important memory persists longer),
- `t` = elapsed time since the last reinforcement.

`MemoryStore::run_decay` advances the lifecycle of memories whose retention
has dropped below the threshold; a protected core (`ProtectedCore`) is
never advanced.

## OSS boundary (Layer A)

This crate is publishable. It **does not** contain family members' real
memories, calibrations, souls, API keys, tokens, IP addresses, or personal
paths. The memory scaffold is generic; a family's real content is Layer B
and is loaded at runtime from a profile directory.

## Future work

- **`Surreal<Any>` (feature flag):** production storage (in-mem dev /
  `RocksDB` prod), same `MemoryStore` interface (design §2.3).
- **Vector search:** cosine similarity / HNSW. Retrieval is currently a
  keyword- + emotion-based v1 scaffold.
