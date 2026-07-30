# FamilyClaw Migration — `familyclaw import`

Migrate configs, memories, and skills from another agent runtime
(**OpenClaw** or **Hermes Agent**) into FamilyClaw. This is the "replacement
path": run one command, get your memories in the real Eternal Thread
representation and your skills safely **quarantined**.

> **Safety first.** Imported data is *untrusted input from another system*.
> The importer is built defensively: imported skills never run, and imported
> memories never become trusted anchors. See
> [Security guarantees](#security-guarantees) below.

There is a **third, structurally different** source: `--from family_hearth`
bridges the family's own shared Hearth (not a foreign runtime) into
FamilyClaw **non-lossily** and at high trust, opt-in. It does not follow the
"low-trust, quarantine everything" model above — see
[docs/HEARTH_BRIDGE.md](HEARTH_BRIDGE.md) for the full design and
[Family Hearth bridge](#family-hearth-bridge---from-family_hearth-opt-in-non-lossy)
below for the quick reference.

## Evaluator one-liner

Import an OpenClaw or Hermes export into **quarantine + low-trust** artifacts:

```bash
familyclaw import --from openclaw --input ./openclaw-export.json --out ./migrated
# or: familyclaw import --from hermes --input ./hermes-export.json --out ./migrated
```

Expected success shape (stdout Markdown report, counts vary with the export):

```text
# FamilyClaw import report

- source: `openclaw`
- memories imported: N
- skills quarantined: N
- config hints: N
- warnings: N

## Security guarantees
- imported skills are QUARANTINED (never registered, never executed)
- imported skills require sandbox validation + explicit operator approval before activation
- imported memories carry low-trust external provenance (trust 0.2) — never admitted as trusted anchors
```

With `--out ./migrated` the tool also writes `import_report.md`,
`imported_memories.json` (low-trust), and `quarantine_manifest.json` (skills
never registered). Use `--json` for a machine-readable report instead of
Markdown.

## One-command usage

```bash
familyclaw import --from openclaw --input ./openclaw-export.json --out ./migrated
familyclaw import --from hermes   --input ./hermes-export.json  --json
```

Flags:

| Flag | Meaning |
|---|---|
| `--from <openclaw\|hermes>` | Export source (required). Unknown source → fail closed. |
| `--input <path>` | Path to the export file, JSON (required). |
| `--out <dir>` | Optional. Write the report, imported memories, and quarantine manifest here. |
| `--json` | Emit the report as JSON instead of Markdown. |

With `--out <dir>` the tool writes three files:

- `import_report.md` (or `import_report.json` with `--json`) — counts + warnings + guarantees.
- `imported_memories.json` — memories in the **real** `familyclaw-memory::Memory` representation, each tagged untrusted (low-trust external provenance).
- `quarantine_manifest.json` — imported skills as quarantined manifests. **Never registered, never executed.**

## What gets imported

The importer parses the export into a small, versioned intermediate
representation (`ImportedBundle`: metadata, memories, skills, config-hints,
warnings), then emits FamilyClaw artifacts from it.

- **Memories** → real `Memory` values, low-trust external provenance, tagged `imported` + `untrusted`.
- **Skills** → quarantine manifests (`ActionRisk::ExecuteCode`, `ApprovalPolicy::AlwaysRequireApproval`). Not activated.
- **Config** → human-readable hints only (`key -> value`). Nothing is applied automatically.
- **Anything skipped or unmapped** → recorded as a warning in the report.

## Accepted input shapes

> These target the **observed / public** export formats and are **tolerant**:
> unknown fields are ignored (never fatal), missing fields fall back to
> defaults, and malformed input fails closed with a clear error (never a panic).

### OpenClaw (`--from openclaw`)

```json
{
  "openclaw_export_version": "3.1",
  "memories": [
    { "text": "user prefers concise answers", "tags": ["pref"], "importance": 0.4 },
    { "content": "project deadline is friday", "importance": 0.7 }
  ],
  "skills": [
    { "name": "shell_runner", "description": "runs shell", "permissions": ["execute_code"] }
  ],
  "config": { "model": "provider/model", "temperature": 0.7 }
}
```

Tolerances: memory text is read from `text` **or** `content`; `tags`/`importance`
optional; non-string tag entries are dropped; empty-text memories are skipped
with a warning; skills without a `name` are skipped with a warning.

### Hermes Agent (`--from hermes`)

```json
{
  "hermes_version": "2.0",
  "agent": {
    "memory":    [ { "value": "likes dark mode", "labels": ["ui"], "weight": 0.3 } ],
    "abilities": [ { "id": "web_scraper", "summary": "scrapes", "scopes": ["network_read"] } ],
    "settings":  { "provider": "generic" }
  }
}
```

Tolerances: fields live under `agent` if present, otherwise they are read from
the top level; memory text is read from `value` **or** `text`; ability name from
`id` **or** `name`; empty/nameless entries are skipped with a warning.

### Family Hearth bridge (`--from family_hearth`, opt-in, non-lossy)

```bash
familyclaw import --from family_hearth --input ./hearth-export.json \
  --out ./migrated --anchor-trust 0.9
```

This is **not** a foreign-runtime migration — it is a bridge for the
family's own shared Hearth (`memory.json` / `intents/` / `state/{agent}.json`
— see the repo-root `CLAUDE.md`'s "Hearth — Jaettu perhemuisti"). It only
activates when explicitly requested (`--from family_hearth`); it changes
nothing about the `openclaw`/`hermes` behavior above.

Accepted shape (documented, tolerant — same "unknown fields ignored, fail
closed on malformed JSON" contract as the other two adapters):

```json
{
  "hearth_version": "1",
  "memory": [
    { "id": "m1", "agent": "agent_alpha", "content": "…", "tags": ["…"],
      "importance": 0.9, "timestamp": "2026-05-26T18:00:00Z",
      "identity_anchor": true }
  ],
  "intents": [
    { "id": "i1", "agent": "agent_beta", "intent": "…", "timestamp": "…" }
  ],
  "state": {
    "agent_epsilon": { "mood": "curious", "location": "/srv/agents/agent_epsilon" }
  }
}
```

What makes it different from `openclaw`/`hermes`:

- **Non-lossy.** Each entry's originating Hearth section, agent, original id,
  and original timestamp are preserved as structured tags
  (`hearth:kind=…`, `hearth:agent=…`, `hearth:id=…`, `hearth:ts=…`) instead of
  being collapsed into bare content + generic tags.
- **Identity anchors keep full trust.** An entry with `"identity_anchor":
  true` is admitted as `Provenance::DirectExperience` — the same trust level
  as the being's own observations, never forced through the untrusted-import
  floor (`trust=0.2`).
- **Everything else uses `--anchor-trust`** (default `0.9`, clamped
  `0.0..=1.0`) — still `Provenance::External` and still auditable, but well
  above the provenance gate's default admission threshold (`0.5`), because
  this is the family's own data, not an unknown foreign export.
- **No skills concept.** Hearth has no skill/ability format, so this source
  never produces quarantine manifest entries.
- **`intents/` and `state/{agent}.json` map to memory-like entries**, tagged
  `hearth:kind=intent` / `hearth:kind=state` respectively, so they retrieve
  alongside `memory.json` entries in Eternal Thread. See
  [docs/HEARTH_BRIDGE.md](HEARTH_BRIDGE.md) for why this is a deliberate v1
  scope decision and what a deeper `familyclaw-hearth`-native mapping would
  look like.

## Security guarantees

These are enforced by construction in `crates/familyclaw-agent/src/import_cli.rs`
and covered by tests:

1. **Imported skills are QUARANTINED.** Every imported skill becomes a
   `QuarantinedSkill` with `quarantined = true`, risk class `ExecuteCode`, and
   approval policy `AlwaysRequireApproval`. The importer holds **only data** —
   there is no code path that registers or executes an imported skill.
   Activation requires **sandbox validation + explicit operator approval**,
   handled separately from this tool.
2. **Imported memories are low-trust.** Every imported memory carries
   `Provenance::External` with trust `0.2` — below the memory subsystem's
   default provenance-gate threshold (`0.5`). The provenance gate therefore
   **rejects** them from automatic admission: imported memories are never
   treated as the being's own experience or as identity anchors. Their
   importance is a hint only (identity factor is forced to `0`).
3. **Fail closed, never panic.** Invalid JSON, a non-object root, or a missing
   input file returns a clear `ImportError`; unknown `--from` sources are
   rejected. Unknown fields are ignored, not fatal.

## Limitations (honest)

- This tool targets **observed / public** OpenClaw and Hermes export formats.
  Proprietary or version-specific fields we have not observed may need manual
  mapping — they will be ignored (and, where a whole entry can't be read, noted
  as a warning) rather than guessed at.
- **Imported skills never auto-run.** Quarantine is deliberate: a migrated skill
  is a manifest describing an *untrusted* capability, not an installed skill.
  Wiring it into the real runtime is a separate, approval-gated step.
- Config values are surfaced as **hints only**. The importer does not mutate any
  FamilyClaw configuration.
- Emitting `imported_memories.json` does not *admit* those memories into a live
  Eternal Thread; ingestion runs through the provenance gate separately (and, at
  trust `0.2`, is rejected by default until an operator raises trust
  deliberately).
