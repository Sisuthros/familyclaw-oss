# familyclaw-security

Identity integrity and human veto for the FamilyClaw platform (Layer A, OSS).

Two security mechanisms:

1. **Identity anchors** (`IdentityAnchor`) — protected, non-forgettable
   memories (decay-λ = 0) that carry a being's identity.
2. **Human corrections** (`HumanCorrection`) — a human veto: highest
   priority in memory retrieval, slow decay (`DecayClass::Slow`).

## Core design decision: identity IS in memory, NOT in a hash

A being's identity **is not** in a SHA-256 digest of the SOUL content. It
is in the substrate of protected anchor memories that the being never
forgets (λ = 0). The digest (`AnchorHash`) is **only a tamper alarm** — it
warns if the anchored content has changed since anchoring, but it does not
*carry* identity.

When tampering is detected (`IdentityStatus::Tampered`), the system
**does not** lose identity or touch the substrate — it raises an alarm and
leaves the anchor memories intact. **The substrate is the truth; the hash
is the sentry.**

## OSS boundary (Layer A)

The crate is publishable. It does not contain family members' souls, the
real content of human corrections, keys, tokens, IP addresses, or personal
paths. An anchor stores only a *digest* of the content + a reference to
the memory; the content stays in the Layer B profile.

## Public API

| Type / function | Responsibility |
|------------------|--------|
| `IdentityAnchor` | A protected anchor: `memory_id`, `anchor_hash`, `protected`, `decay`. |
| `IdentityAnchor::verify` | Compares current content against the anchored digest (does not mutate). |
| `IdentityStatus` | `Intact` / `Tampered { memory_id, expected, actual }`. |
| `verify_identity` | Checks a set of anchors against a content source. |
| `AnchorHash` | A validated SHA-256 hex digest, constant-time comparison. |
| `DecayLambda` | Ebbinghaus λ; `ZERO` = an eternal anchor. |
| `HumanCorrection` | A human veto: `content`, `priority` (1.0), `decay` (Slow), `applied_at`. |
| `HumanCorrection::wins_against` | Whether the veto beats a competing retrieval score (ties go to the human). |
| `CorrectionPriority` | A clamped priority `0.0..=1.0`; `MAX` = 1.0. |
| `DecayClass` | A named decay class (`Eternal` / `Slow` / `Normal` / `Fast`) → λ. |
| `SecurityError`, `Result` | The crate's error type (converts into `FamilyClawError`). |
