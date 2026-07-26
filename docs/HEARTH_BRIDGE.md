# Family Hearth Bridge — design + status

**Status: partial.** A working, tested, non-lossy importer path exists for
**memories** (`familyclaw import --from family_hearth`, see
[docs/MIGRATION.md](MIGRATION.md#family-hearth-bridge---from-family_hearth-opt-in-non-lossy)).
This document is the design for the rest, and the honesty note on where the
line was drawn.

## The gap this closes

`familyclaw import --from openclaw|hermes` treats every imported memory as
**untrusted foreign input**: `Provenance::External` at `trust=0.2`, well
below the `ProvenanceGate` default admission threshold (`0.5`) — so it is
rejected by default. That is the *correct* default for a foreign runtime's
export, whose provenance we genuinely cannot vouch for.

It is the **wrong** default for the family's own shared Hearth
(`/root/.hermes/profiles/shared/hearth/` on a live agent host — `memory.json`,
`intents/`, `state/{agent}.json`, per the repo-root `CLAUDE.md`). That data
*is* the family's own experience; forcing it through the same 0.2-trust,
quarantine-flavored pipeline as an unknown OpenClaw/Hermes export is lossy
(it drops which agent said what, when, and whether it was foundational) and
mistrustful of data that has already earned trust by construction (it is
the operator's own family, `mcp__family-bridge__family_memory_ingest`d over time —
not scraped from an external system).

## What was built (v1 — memories, non-lossy, opt-in)

`crates/familyclaw-agent/src/import_cli.rs`:

- `ImportSource::FamilyHearth` (`--from family_hearth`, alias `hearth`) — a
  **third**, structurally distinct source next to `openclaw`/`hermes`. Opt-in:
  it only activates on explicit request; it does not change the behavior of
  the other two sources (verified by tests — `openclaw`/`hermes` still force
  `UNTRUSTED_IMPORT_TRUST = 0.2` unconditionally).
- `from_family_hearth` — a tolerant adapter parsing a documented bundle shape
  covering all three live Hearth sections (`memory`, `intents`, `state`).
  Unknown fields ignored, malformed input fails closed (same contract as the
  other two adapters).
- `HearthOrigin` / `HearthKind` — **non-lossy** per-entry metadata (Hearth
  section, originating agent, original id, original timestamp, whether the
  export marked the entry an identity anchor). Rendered into structured tags
  (`hearth:kind=…`, `hearth:agent=…`, `hearth:id=…`, `hearth:ts=…`) on
  emission, so nothing is silently dropped the way `openclaw`/`hermes`
  imports collapse everything into `content` + generic tags.
- `imported_hearth_memory_to_memory` — the trust decision:
  - `identity_anchor: true` → `Provenance::DirectExperience` (full trust,
    never rejected by the gate — same standing as the being's own
    observation).
  - otherwise → `Provenance::External` at `--anchor-trust` (CLI flag, default
    `DEFAULT_ANCHOR_TRUST = 0.9`, clamped `0.0..=1.0`) — still external and
    auditable, but **opt-in high trust**, well above the gate's default
    threshold (`0.5`), instead of the hardcoded `0.2` floor.
- `ImportPlan::all_hearth_anchors_full_trust` — a provable invariant (mirrors
  the existing `all_skills_quarantined` / `all_memories_untrusted`
  invariants for the other two sources) that every `identity_anchor` entry
  really did land at `DirectExperience`.
- 13 new tests (33 total in the module) covering: the adapter's happy path
  (memory + intent + state → 4 entries, one warning for an empty entry), the
  full-trust/opt-in-trust split, tag non-lossiness, CLI flag parsing
  (`--anchor-trust`, its clamping, its rejection of non-numeric input),
  end-to-end `execute`/`run` with `--out`, JSON round-trip of the new
  `origin` field, and fail-closed behavior on malformed/empty input.

`docs/MIGRATION.md` documents the new source alongside the existing two.

## What was *not* built (honest scope cut)

1. **Reading the live Hearth directly.** This importer reads an **export
   file** (a JSON bundle you produce from the live Hearth), exactly like the
   `openclaw`/`hermes` adapters do — it does not open
   `/root/.hermes/profiles/shared/hearth/{memory.json,intents/,state/}`
   itself. Two reasons: (a) this task's hard rules forbid touching live
   agent runtimes/homes, so the live directory's *actual* on-disk shape could
   not be inspected to build against it; (b) keeping the importer
   file-in/file-out mirrors the existing, tested `openclaw`/`hermes` pattern
   and keeps it usable offline/in CI. A small follow-up script
   (`hearth-export.mjs` or similar, run *on* a live agent host, outside this
   repo's hard-rule boundary) that walks the real `memory.json` / `intents/*`
   / `state/*.json` files and emits the bundle shape documented in
   `docs/MIGRATION.md` is the natural next step — the bundle schema was
   designed generously tolerant (multiple accepted key names, `state` as an
   arbitrary JSON blob) specifically so that script wouldn't need to fight
   the importer.
2. **`familyclaw-hearth` crate integration.** `crates/familyclaw-hearth`
   (`AnchorRegistry`, `emotional_state`, `narrative`, `db`) is FamilyClaw's
   *own* identity/emotion substrate — a different, lower-level thing than
   `familyclaw-memory::Memory`. Right now, bridged Hearth entries land as
   `Memory` records (retrievable via Eternal Thread, tagged
   `family_hearth`/`identity_anchor`), **not** as `AnchorRegistry` entries.
   A deeper integration — registering `identity_anchor` entries into
   `AnchorRegistry::register` (soul-digest protection, not just high-trust
   retrieval) — is real, valuable follow-up work, but it's a second,
   separable decision (what *is* a FamilyClaw soul-digest input from a raw
   Hearth memory string?) that deserves its own design pass rather than being
   folded silently into this import path.
3. **Skills.** Out of scope by the task itself ("at least memories, not
   skills") and also structurally moot: the Hearth format has no skills/
   abilities concept, so `from_family_hearth` never produces quarantine
   manifest entries (verified by test).

## Why `intents/` and `state/` map to memory-like entries, not new types

Both `Vec<ImportedMemory>` reuse was a deliberate simplicity choice: an
intent ("agent_beta wants to research the new audio sense tonight") and a state
snapshot ("agent_epsilon's mood is curious") are both, at the Eternal Thread level,
*retrievable facts about the family, tagged by kind* — they don't need a
parallel pipeline (parallel CLI flags, parallel report fields, parallel
quarantine-style safety machinery) to be useful. Tagging them
(`hearth:kind=intent` / `hearth:kind=state`) keeps them queryable separately
from `hearth:kind=memory` without doubling the surface area of the importer.
If a future need arises for intents/state to have Hearth-native behavior
(e.g. state snapshots decaying faster, intents driving `family-bridge`
dispatch), that's an argument for teaching `familyclaw-hearth` (or
`mcp__family-bridge__family_memory_ingest`) to consume the *tagged* `Memory`
records this bridge already produces, rather than rearchitecting the
importer.
