# FamilyClaw Security Model

> **Eight defense layers** for a generic agent runtime (Layer A only).
> Inspired by the Hermes seven-layer posture; FamilyClaw adds an explicit
> **Layer A / Layer B isolation** boundary as layer eight.

FamilyClaw treats security as **fail-closed by default**: when a layer cannot
prove safety, the operation stops rather than guessing. All examples below use
generic terms (`operator`, `agent_a`, `mock_provider`, `external_system`) —
never private identities, keys, or deployment paths.

---

## Layer 1 — Allowlist roots

**What it protects:** Unbounded filesystem or network reach from skills and tools.

**Mechanism:** Path- and host-scoped allowlists configured at runtime (Layer B
data). Skills such as `fs_read` and `file_write` canonicalize paths, reject
`..` escapes, and **deny by default** when no roots are configured.

| Surface | Crate / module |
|---------|----------------|
| Local file read | `familyclaw-actions` → `FsReadAllowlisted` |
| Local file write | `familyclaw-actions` → `FileWriteAllowlisted` |
| HTTP fetch | `familyclaw-actions` → `WebFetchSkill` (SSRF guards) |

**Invariant:** Empty allowlist = fail-closed (skill registered but rejects all targets).

---

## Layer 2 — Fail-closed approvals

**What it protects:** Irreversible or external side effects without human consent.

**Mechanism:** Every skill declares `risk` and `approval_policy` in its manifest.
Policy is derived from the manifest — **never** from attacker-controlled task
payload. High-risk actions (`write_external`, `spend_money`, `irreversible`) always
require approval. Pending approvals are TTL-bound, one-shot, and payload-hash-bound.

| Surface | Crate / module |
|---------|----------------|
| Approval gate | `familyclaw-actions` → `approval`, `policy` |
| Operator surface | `familyclaw-gateway` → `/approvals` routes |
| Durable pending | `familyclaw-actions` → `JournalPendingStore` |

**Invariant:** Uncertain approval state → deny execution (404 / `ApprovalMissing`).

---

## Layer 3 — Taint tracing

**What it protects:** Untrusted content flowing into trusted context without marking.

**Mechanism:** Tool outputs from network fetches, file reads outside trusted
roots, and LLM-generated text are treated as **tainted**. Taint propagates through
the tool loop; trusted project files may be exempt when explicitly allowlisted.

| Surface | Crate / module |
|---------|----------------|
| Skill outputs | `familyclaw-actions` → skill executors |
| Tool loop | `familyclaw-agent` → `think` / `handle_turn` |

**Invariant:** Tainted data is never promoted to trusted without an explicit,
audited boundary crossing.

---

## Layer 4 — Redaction

**What it protects:** Secrets and sensitive payloads leaking into proofs, audit
logs, operator UIs, or memory.

**Mechanism:** Proof bundles, suspend summaries, and turn-audit records pass through
`redact_free_text` before persistence. Manifests are scanned for secret-like
patterns at registration time.

| Surface | Crate / module |
|---------|----------------|
| Proof bundles | `familyclaw-actions` → `proof` |
| Free-text redaction | `familyclaw-actions` → `redact_free_text` |
| Manifest scan | `familyclaw-actions` → `manifest::validate` |

**Invariant:** Proofs record hashes and counts, not raw secrets or full file bodies.

---

## Layer 5 — Identity-anchor tamper alert

**What it protects:** Silent modification of an agent's protected identity substrate.

**Mechanism:** Identity lives in **protected memory** (decay λ=0), not in a hash.
`IdentityAnchor` stores a content hash as a **tamper alarm only**: if anchored
content changes after anchoring, the system raises `IdentityStatus::Tampered`
without destroying the substrate.

| Surface | Crate / module |
|---------|----------------|
| Anchors | `familyclaw-security` → `IdentityAnchor` |
| Verification | `familyclaw-security` → `verify_identity` |
| Memory substrate | `familyclaw-memory` (protected core) |

**Invariant:** Tamper detected → alert; identity substrate is not overwritten.

---

## Layer 6 — Wasmtime sandbox

**What it protects:** Arbitrary code execution from third-party or LLM-generated skills.

**Mechanism:** Optional Wasmtime backend with fuel metering, deny-by-default host
imports, and capability grants. Third-party skills **should** run inside the
sandbox; built-in Layer A skills are pure Rust. Enable at runtime with
`FAMILYCLAW_SANDBOX_SKILLS=1` (wired by `build_family` when the sandbox crate
is available).

| Surface | Crate / module |
|---------|----------------|
| Sandbox trait | `familyclaw-sandbox` → `CodeSandbox` |
| Wasmtime backend | `familyclaw-sandbox` (feature `wasmtime`) |
| Agent wiring | `familyclaw-runtime` → `build_family` |

**Invariant:** Without sandbox, `execute_code` returns an error — no silent fallback
to host execution.

---

## Layer 7 — At-most-once dispatch

**What it protects:** Duplicate external side effects after process crash (SIGKILL).

**Mechanism:** Durable dispatch outbox (`JournalDispatchOutbox`) tracks
idempotency keys through `InProgress` → `Committed` states. A crash after commit
returns the same result; a crash in the intent-only window fails closed and
requires operator recovery.

| Surface | Crate / module |
|---------|----------------|
| Dispatch outbox | `familyclaw-actions` → `dispatch_outbox` |
| Durable assembly | `familyclaw-runtime` → `build_family` |
| Red-team proof | `familyclaw-actions` → `dispatch_redteam` binary |

**Invariant:** `side_effect_overcount = 0` under the crash matrix (see continuity scorecard).

---

## Layer 8 — Layer A / Layer B isolation

**What it protects:** Private souls, keys, paths, and operator data entering the
public repository or generic runtime defaults.

**Mechanism:** Layer A (this repo) ships only generic types and example beings
(`agent_a`, `agent_b`). Layer B (private profiles) loads at runtime via
`FAMILYCLAW_PROFILE_DIR`, `FAMILYCLAW_DATA_DIR`, and related env vars. CI runs
`scripts/audit-layer-b.sh` on every merge.

| Surface | Enforcement |
|---------|-------------|
| Git ignore | `.gitignore` blocks Layer B patterns |
| CI audit | `layer-b-audit` job |
| Config resolution | Runtime reads paths from env, never hardcoded |

**Invariant:** Nothing from Layer B may be committed to Layer A.

---

## Signed external skills

Third-party skill manifests may declare `publisher` and `signature` (Ed25519).
External skills (non-empty `publisher`) **must** verify against trusted public
keys loaded from `FAMILYCLAW_SKILL_REGISTRY` (path to a JSON map of publisher →
hex-encoded Ed25519 public key). Invalid or missing signatures fail closed at
registration — the skill never enters the registry.

See `familyclaw-actions` → `manifest::SkillManifest::validate`.

---

## Related documents

| Document | Topic |
|----------|-------|
| [`LAYER_BOUNDARY.md`](LAYER_BOUNDARY.md) | Layer A / B split in detail |
| [`SCORECARD.md`](SCORECARD.md) | Continuity benchmark (crash matrix) |
| [`CRASH_SAFE_DISPATCH_CASE_STUDY.md`](CRASH_SAFE_DISPATCH_CASE_STUDY.md) | At-most-once dispatch proof |
| [`familyclaw-sandbox/README.md`](../crates/familyclaw-sandbox/README.md) | Wasmtime sandbox guarantees |
