# familyclaw-core

The FamilyClaw platform's core crate: shared types, error handling,
configuration, and time helpers. This is the Layer A (OSS) foundation on
which all other crates are built. Independent of other familyclaw crates.

## Contents

| Module | Responsibility |
|---------|--------|
| `error` | `FamilyClawError` (thiserror) + `Result<T>` — config/io/serde/bus/memory/not-found/invalid-input variants |
| `ids` | `AgentId`, `FamilyId`, `MessageId` — UUID-based newtype identifiers (serde-transparent) |
| `config` | `FamilyConfig`, `AgentConfig`, `ModelConfig` — loadable from JSON, with validation |
| `time` | UTC timestamps (`Timestamp`), RFC 3339 / Unix conversions |

## Principles

- **No `unwrap()`/`expect()`/`panic!()` on the production path.** All
  errors flow through the `Result` type.
- **Typed identifiers** prevent identifiers from being mixed up at
  compile time.
- **OSS boundary (Layer A):** no hardcoded souls, keys, tokens, IP
  addresses, or personal paths. Agent profiles are loaded at runtime
  (`AgentConfig::profile_dir`, cf. `FAMILYCLAW_PROFILE_DIR`).

## Example

```rust
use familyclaw_core::{AgentConfig, FamilyConfig, ModelConfig};

let family = FamilyConfig::new("demo_family").with_agent(AgentConfig::new(
    "agent_a",
    ModelConfig::new("provider/model").with_fallback("provider/backup"),
));
family.validate().expect("valid config");
```

License: MIT.
