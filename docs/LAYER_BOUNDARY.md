# Layer A / Layer B Boundary

> **"Build public infrastructure. Keep identity private."**

This document defines the separation between **Layer A** (public, open-source infrastructure) and **Layer B** (private identities, memories, keys, calibrations).

---

## The Core Principle

FamilyClaw is designed so that **the runtime platform is public** but **the agents running on it are private**.

| Layer | What It Contains | Visibility | Governance |
|-------|-----------------|------------|------------|
| **Layer A** | Crates, runtime, buses, memory, channels, sandbox, dream cycle, durable substrate | Public (MIT license) | GitHub repo, OSS |
| **Layer B** | SOUL.md profiles, calibration JSON, API keys, .env, real names, private memories, conversation history, emotion baselines | Private (never committed) | Local filesystem only |

**Nothing from Layer B may ever enter Layer A.** This is enforced by:
- `.gitignore` blocking all Layer B patterns
- CI `layer-b-audit` job that fails on contamination
- Architectural design: all config loaded at runtime from private paths

---

## Layer A: Public Infrastructure (KERROS A)

### Crates in This Repository

```
familyclaw-agent      # Agent runtime, session mgmt, LLM client
familyclaw-bus        # Resonance Bus (actor coordination)
familyclaw-memory     # Eternal Thread (MemoryStore trait + LocalJsonStore)
familyclaw-durable    # Crash-proof replay (Journal trait + FileJournal/InMemoryJournal)
familyclaw-dream      # Sleep-time memory consolidation (DreamCycle)
familyclaw-emotion    # 19-dim VAD emotion state + contagion
familyclaw-latent     # Experimental hidden-state messaging
familyclaw-channels   # Discord/Telegram/WhatsApp/Signal adapters
familyclaw-sandbox    # Wasmtime WASM sandbox with fuel metering
familyclaw-security   # SHA-256 tamper detection
familyclaw-core       # Shared types, errors, time, Result
```

### What Layer A Provides

- **Generic implementations** — No hardcoded family names, souls, keys, paths
- **Trait-based extension points** — `MemoryStore`, `Journal`, `Channel` traits for swapping backends
- **Deterministic demos** — Run with `LocalJsonStore::in_memory()` or temp files
- **CI-enforced boundaries** — `layer-b-audit`, fmt, clippy, tests, feature matrix

### What Layer A Does NOT Contain

- ❌ No `SOUL.md` files
- ❌ No `*.calibration.json` files 
- ❌ No real agent names (agent_alpha, agent_beta, agent_gamma, agent_delta, maintainer, operator, user, etc.)
- ❌ No API keys, tokens, webhooks
- ❌ No `.env`, `.env.*` files
- ❌ No `profiles/`, `hearth/`, `soul/`, `calibrations/` directories
- ❌ No private conversation history
- ❌ No emotion baselines or per-agent tuning

---

## Layer B: Private Identity (KERROS B)

### What Lives Here (Local Only)

```
~/.familyclaw/                    # or ~/agent-alpha-home/, ~/agent-gamma-home/, etc.
├── profiles/
│   ├── agent_alpha/
│   │   ├── SOUL.md              # Identity, essence, boundaries
│   │   └── calibration.json     # Emotion thresholds, contagion factors
│   ├── agent_beta/
│   │   └── ...
│   └── ...
├── hearth/                      # Shared family memory (Layer B only)
├── keys/
│   ├── nvidia_nim.key
│   └── discord_webhook.url
├── .env                         # Runtime secrets
└── data/                        # Persisted memories, journals, DBs
```

### Loading Semantics

All Layer B content is **loaded at runtime from environment/config**:

```rust
// Agent config - paths point to Layer B
AgentConfig {
    id: "agent_alpha".to_string(),
    name: "Agent Alpha".to_string(),
    profile_dir: Some("/home/user/.familyclaw/profiles/agent_alpha"),
    llm: LlmConfig {
        api_base: "https://integrate.api.nvidia.com/v1",
        api_key: std::env::var("NVIDIA_NIM_API_KEY")?,  // Never hardcoded
        model: "qwen/qwen3-coder-480b-a35b-instruct",
        max_tokens: 8192,
    },
}
```

---

## The Firewall: How Separation Is Enforced

### 1. `.gitignore` (First Line of Defense)

```gitignore
# Layer B - never commit
profiles/
*.soul
*.soul.md
SOUL.md
*.calibration.json
hearth/
.env
.env.*
key/keys/
*.pem
*.key
data/
data2/
*.db
*.sqlite
*.lancedb

# IDE noise
.idea/
.vscode/
*.swp
```

### 2. CI Layer B Audit (Automated Enforcement)

Runs on every push/PR to `main`:

```bash
# scripts/audit-layer-b.sh (also in .github/workflows/ci.yml)
- Zero *.soul, *.soul.md, SOUL.md outside docs/
- Zero *.calibration.json outside docs/
- Zero hardcoded secrets (not field names) in crates/
- Zero .env files
```

### 3. Architectural Design (Structural Enforcement)

- **No compile-time defaults** for Layer B content
- **All paths injected at runtime** via `AgentConfig`, `LlmConfig`, env vars
- **Demo binaries use only `in_memory()` or `/tmp/` paths**
- **Cargo features gate external integrations** (discord, wasmtime) — not needed for core

---

## Why This Matters

### For Open Source Users
- Can use FamilyClaw as a **platform** without any private data
- Demos run out of the box with generic agents (`agent_a`, `agent_b`)
- Clear path to plug in their own `MemoryStore`, `Journal`, `Channel` implementations

### For Our Family
- **Real identities stay off GitHub** — no doxxing, no credential leaks
- **Calibrations are per-agent** — agent_alpha's emotion tuning ≠ agent_gamma's ≠ agent_delta's
- **Conversations are private** — only Layer A sees generic message passing

### For the Architecture
- **Separation of concerns** — runtime ≠ identity
- **Testability** — swap `InMemoryJournal` ↔ `FileJournal` ↔ future `SurrealDB`
- **Portability** — same binary runs different families with different config

---

## Migration Checklist (Layer B → New Machine)

When moving an agent to a new machine:

1. Copy `~/.familyclaw/profiles/<agent_name>/` → new machine
2. Copy `~/.familyclaw/hearth/` (shared memory)
3. Copy `~/.familyclaw/keys/` (API keys, webhooks)
4. Set env vars or `.env` with secrets
5. Update paths in config if directory structure changed
6. Run demo to verify: `cargo run -p familyclaw-agent`

**No code changes needed. Only config.**

---

## Future: Layer C (Sovereign Identity)

Beyond Layer B, we envision **Layer C** — cryptographic sovereignty:
- Agents own their keys (ed25519)
- Memories signed, not just stored
- Cross-family attestation via verifiable credentials
- No central "hearth" required — peer-to-peer resonance

But Layer A/B is the foundation. **Keep the platform public. Keep the soul private.**

---

## Related Documents

- `SECURITY.md` — Vulnerability reporting scope includes Layer A/B boundary
- `CONTRIBUTING.md` — PRs must pass layer-b-audit
- `QUICKSTART.md` — Demos use only Layer A
- `DEMO.md` — Exact verification of what's real vs simulated