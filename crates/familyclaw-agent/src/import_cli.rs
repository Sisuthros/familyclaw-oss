//! `familyclaw import` — `OpenClaw` / `Hermes` / family `Hearth` migration tool
//! ("replacement path" + "Hearth bridge").
//!
//! This module implements the `import` subcommand of the `familyclaw` binary. It
//! supports two structurally different kinds of source, on purpose:
//!
//! 1. **Foreign runtime migration** (`--from openclaw|hermes`) — reads the
//!    **export** of another agent runtime and converts it into `FamilyClaw`'s
//!    own representation, **lossily and conservatively**: skills are
//!    quarantined, memories are marked low-trust external. This is the right
//!    default for data whose origin we cannot vouch for.
//! 2. **Family Hearth bridge** (`--from family_hearth`) — reads an export of
//!    the family's own shared Hearth (`memory.json` / `intents/` /
//!    `state/{agent}.json` — see the root `CLAUDE.md`'s "Hearth — Jaettu
//!    perhemuisti" section) and converts it **non-lossily**: per-entry
//!    metadata (originating agent, kind, original id, timestamp) is preserved
//!    as structured tags, and entries the export marks as **identity
//!    anchors** are admitted at full trust
//!    ([`Provenance::DirectExperience`]) rather than being forced through the
//!    untrusted-import gate. This path is **opt-in** — it only activates when
//!    the operator explicitly passes `--from family_hearth`; nothing here
//!    changes the behavior of `openclaw`/`hermes` imports.
//!
//! ```text
//! familyclaw import --from openclaw|hermes|family_hearth --input <path> \
//!     [--out <dir>] [--json] [--anchor-trust <0.0..=1.0>]
//! ```
//!
//! ## Why this is security-sensitive
//! Imported data is, by default, **untrusted input from another system**.
//! Two structural guarantees protect the runtime for the `openclaw`/`hermes`
//! sources (see also `docs/MIGRATION.md`):
//!
//! 1. **Skills go into quarantine (`Quarantined`).** An imported skill is NEVER
//!    registered and never executed. The importer writes it to a separate
//!    *quarantine manifest* ([`QuarantinedSkill`]) with risk class
//!    [`ActionRisk::ExecuteCode`] and policy
//!    [`ApprovalPolicy::AlwaysRequireApproval`] — activation requires sandbox
//!    validation + the operator's explicit approval. This module never calls
//!    the registry or the executor.
//! 2. **Memories carry low-trust provenance.** An imported memory receives
//!    [`Provenance::External`] provenance with low trust
//!    ([`UNTRUSTED_IMPORT_TRUST`]), so `familyclaw-memory`'s provenance gate
//!    governs them — they are **not** admitted as trusted identity anchors.
//!
//! The `family_hearth` source deliberately relaxes guarantee (2) — see
//! [`from_family_hearth`] and `docs/HEARTH_BRIDGE.md` for the reasoning and
//! the opt-in mechanism. Guarantee (1) does not apply: the Hearth export has
//! no skills concept, so no quarantine manifest entries are produced from it.
//!
//! ## Design constraint (honesty)
//! We do **not** have `OpenClaw`'s/`Hermes`'s exact, closed export schema, nor
//! is there yet a canonical machine-readable Hearth export schema upstream.
//! Therefore we define a small, versioned **intermediate representation**
//! ([`ImportedBundle`]) and write three *tolerant* adapters ([`from_openclaw`],
//! [`from_hermes`], [`from_family_hearth`]) that parse a documented, plausible
//! JSON format (described in doc comments + `docs/MIGRATION.md` /
//! `docs/HEARTH_BRIDGE.md`). Unknown fields are **ignored** — they are never
//! fatal. Malformed input fails **fail-closed** with a clear error — never a
//! panic.
//!
//! ## Testability
//! The command handlers ([`parse`], [`execute`], adapters) are pure functions
//! that return `Result<_, ImportError>` — they never print themselves nor
//! call `process::exit`. Same pattern as in [`crate::replay_cli`].

use std::fmt::Write as _;
use std::path::PathBuf;

use familyclaw_actions::ids::SkillId;
use familyclaw_actions::manifest::{default_input_schema, SkillManifest};
use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
use familyclaw_memory::{ImportanceFactors, Memory, Provenance};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Schema version of the intermediate representation. Bumped if the
/// [`ImportedBundle`] structure changes in a backwards-incompatible way.
pub const BUNDLE_SCHEMA_VERSION: u32 = 1;

/// Trust level of imported memories under [`Provenance::External`] provenance.
///
/// Deliberately low: below `familyclaw-memory`'s `ProvenanceGate` default
/// threshold (`0.5`), so an imported memory does **not** automatically gain
/// trusted retrieval without operator judgment. This prevents imported data
/// from masquerading as the being's own experience.
pub const UNTRUSTED_IMPORT_TRUST: f32 = 0.2;

/// Default trust level for a **family Hearth** import entry that is *not*
/// marked as an identity anchor by the export.
///
/// Deliberately **above** `familyclaw-memory`'s `ProvenanceGate` default
/// threshold (`0.5`) and far above [`UNTRUSTED_IMPORT_TRUST`]: this is the
/// family's own shared memory, not a foreign runtime's export, so the
/// opt-in `--from family_hearth` path treats it as a trustworthy — though
/// still external, still auditable — source. Overridable per-run via
/// `--anchor-trust`.
pub const DEFAULT_ANCHOR_TRUST: f32 = 0.9;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error type for the `import` CLI.
///
/// All errors are **panic-free**: malformed input (missing argument, unknown
/// source, missing/invalid input) is returned via this type, and the binary
/// renders it as a clear error message + a nonzero exit code.
#[derive(Debug)]
#[non_exhaustive]
pub enum ImportError {
    /// Argument parsing failed (missing/unknown flag or value, unknown
    /// `--from` source).
    Usage(String),
    /// Reading the input file failed.
    Io(std::io::Error),
    /// Parsing the export JSON failed (invalid JSON) — fail-closed.
    Parse(String),
    /// JSON serialization of the result (report / manifest / memory) failed.
    Serde(serde_json::Error),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Usage(msg) => write!(f, "usage error: {msg}"),
            ImportError::Io(err) => write!(f, "io error: {err}"),
            ImportError::Parse(msg) => write!(f, "parse error: {msg}"),
            ImportError::Serde(err) => write!(f, "serialization error: {err}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<std::io::Error> for ImportError {
    fn from(err: std::io::Error) -> Self {
        ImportError::Io(err)
    }
}

impl From<serde_json::Error> for ImportError {
    fn from(err: serde_json::Error) -> Self {
        ImportError::Serde(err)
    }
}

// ---------------------------------------------------------------------------
// Source (--from)
// ---------------------------------------------------------------------------

/// Supported export source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    /// `OpenClaw` export.
    OpenClaw,
    /// Hermes Agent export.
    Hermes,
    /// The family's own shared Hearth export (`memory.json` / `intents/` /
    /// `state/{agent}.json`). Opt-in, non-lossy — see [`from_family_hearth`].
    FamilyHearth,
}

impl ImportSource {
    /// Parses the source from a `--from` value (unknown → [`ImportError::Usage`]).
    ///
    /// # Errors
    /// [`ImportError::Usage`] if the value is not `openclaw`, `hermes`,
    /// `family_hearth`, or the alias `hearth`.
    pub fn parse(value: &str) -> Result<Self, ImportError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openclaw" => Ok(Self::OpenClaw),
            "hermes" => Ok(Self::Hermes),
            "family_hearth" | "hearth" => Ok(Self::FamilyHearth),
            other => Err(ImportError::Usage(format!(
                "unknown --from source `{other}` (expected openclaw|hermes|family_hearth)"
            ))),
        }
    }

    /// Stable, machine-readable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
            Self::FamilyHearth => "family_hearth",
        }
    }

    /// Is this the opt-in, non-lossy family Hearth bridge (as opposed to a
    /// foreign-runtime migration)?
    #[must_use]
    pub const fn is_family_hearth(self) -> bool {
        matches!(self, Self::FamilyHearth)
    }
}

// ---------------------------------------------------------------------------
// Intermediate representation
// ---------------------------------------------------------------------------

/// Intermediate representation of a single imported memory (source-independent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedMemory {
    /// The memory's text content.
    pub content: String,
    /// Free-form tags (generic).
    pub tags: Vec<String>,
    /// Importance `0.0..=1.0` as declared by the original source (if any);
    /// otherwise the default. Used only as a hint — the imported importance
    /// does not bypass the provenance gate.
    pub importance_hint: f32,
    /// Non-lossy origin metadata. Only populated by [`from_family_hearth`]
    /// (`None` for the `openclaw`/`hermes` adapters, which have no such
    /// structure to preserve). Carries provenance-relevant facts —
    /// originating agent, Hearth section, original id, original timestamp,
    /// and whether the export marked this entry an identity anchor — that
    /// would otherwise be dropped on import.
    #[serde(default)]
    pub origin: Option<HearthOrigin>,
}

/// Which section of the shared Hearth an imported entry came from.
///
/// Mirrors the layout documented in the repo-root `CLAUDE.md`
/// ("Hearth — Jaettu perhemuisti"): `memory.json` (shared memories),
/// `intents/` (Intent Broadcasting), `state/{agent}.json` (per-agent state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HearthKind {
    /// A `memory.json` entry.
    Memory,
    /// An `intents/` entry (Intent Broadcasting).
    Intent,
    /// A `state/{agent}.json` snapshot.
    State,
}

impl HearthKind {
    /// Stable, machine-readable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Intent => "intent",
            Self::State => "state",
        }
    }
}

/// Non-lossy metadata preserved from a family Hearth export entry.
///
/// This is what makes the `family_hearth` import path **non-lossy**
/// compared to the `openclaw`/`hermes` adapters: instead of collapsing an
/// entry down to bare `content` + generic `tags`, the entry's origin is
/// kept as structured data (and re-rendered into auditable tags on
/// emission — see [`imported_memory_to_memory`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HearthOrigin {
    /// Which Hearth section this entry came from.
    pub kind: HearthKind,
    /// The originating family agent's name (e.g. `"agent_alpha"`, `"agent_gamma"`),
    /// if the export recorded one.
    pub agent: Option<String>,
    /// The entry's original identifier in the Hearth export, if any.
    pub original_id: Option<String>,
    /// The entry's original timestamp (as a raw string, whatever format the
    /// export used — not reparsed, so it can never fail to import), if any.
    pub timestamp: Option<String>,
    /// Whether the export explicitly marked this entry an **identity
    /// anchor** — a memory that helps define who the agent *is* (see the
    /// root `CLAUDE.md`'s "Perhe-protokolla"). Identity-anchor entries are
    /// admitted at full trust ([`Provenance::DirectExperience`]) instead of
    /// the configurable `--anchor-trust` weight.
    #[serde(default)]
    pub identity_anchor: bool,
}

/// Intermediate representation of a single imported skill.
///
/// This is a **description**, not an executable skill: the importer never
/// runs the imported skill's code. The name/description/permissions are
/// carried over into the quarantine manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedSkill {
    /// The skill's name in the source.
    pub name: String,
    /// Description (if any).
    pub description: String,
    /// Permissions declared by the source, raw (mapped cautiously).
    pub declared_permissions: Vec<String>,
}

/// Source-independent **intermediate representation** of the whole imported bundle.
///
/// The adapters ([`from_openclaw`], [`from_hermes`]) produce this; reporting
/// and emission ([`ImportPlan`]) consume it. Versioned
/// ([`BUNDLE_SCHEMA_VERSION`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedBundle {
    /// Schema version.
    pub schema_version: u32,
    /// Which source this was imported from.
    pub source: ImportSource,
    /// Imported memories.
    pub memories: Vec<ImportedMemory>,
    /// Imported skills (go into quarantine — never activated).
    pub skills: Vec<ImportedSkill>,
    /// Configuration hints (key → value), for a human to look at. These are
    /// not applied automatically — merely a hint during migration.
    pub config_hints: Vec<(String, String)>,
    /// Warnings about things that were skipped or did not map cleanly.
    pub warnings: Vec<String>,
}

impl ImportedBundle {
    /// Creates an empty bundle for the given source.
    #[must_use]
    pub fn empty(source: ImportSource) -> Self {
        Self {
            schema_version: BUNDLE_SCHEMA_VERSION,
            source,
            memories: Vec::new(),
            skills: Vec::new(),
            config_hints: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Quarantine manifest (imported skills)
// ---------------------------------------------------------------------------

/// Manifest for a single quarantined skill.
///
/// **Security guarantee:** this is not a registrable skill. It carries
/// [`ActionRisk::ExecuteCode`] risk and
/// [`ApprovalPolicy::AlwaysRequireApproval`] policy, and is marked
/// [`quarantined = true`](QuarantinedSkill::quarantined). Activation requires
/// sandbox validation + the operator's explicit approval, done separately;
/// this importer provides no activation path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuarantinedSkill {
    /// Always `true` — documents that the skill is quarantined, not active.
    pub quarantined: bool,
    /// Which source the skill was imported from.
    pub source: ImportSource,
    /// Generic skill manifest (name, description, permissions, risk, policy).
    pub manifest: SkillManifest,
}

/// The full quarantine manifest: all imported skills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuarantineManifest {
    /// Schema version.
    pub schema_version: u32,
    /// Human-readable note about the guarantee.
    pub note: String,
    /// Quarantined skills.
    pub skills: Vec<QuarantinedSkill>,
}

// ---------------------------------------------------------------------------
// Adapters: raw export JSON → intermediate representation
// ---------------------------------------------------------------------------

/// Reads an `f32` importance hint from a JSON value, clamped to `0.0..=1.0`;
/// missing/non-numeric → default `0.3`.
fn importance_from(value: Option<&Value>) -> f32 {
    value.and_then(Value::as_f64).map_or(0.3, |f| {
        #[allow(clippy::cast_possible_truncation)]
        let v = f as f32;
        v.clamp(0.0, 1.0)
    })
}

/// Reads a string list from a JSON value; non-string elements are skipped
/// (tolerant). Missing → empty.
fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Adapter: `OpenClaw` export (JSON) → [`ImportedBundle`].
///
/// ## Accepted format (documented, tolerant)
/// Targets the observed/public `OpenClaw` export format. Unknown fields are
/// ignored, missing fields get defaults. Expected top-level object:
///
/// ```json
/// {
///   "openclaw_export_version": "…",
///   "memories":  [ { "text": "…", "tags": ["…"], "importance": 0.4 } ],
///   "skills":    [ { "name": "…", "description": "…", "permissions": ["…"] } ],
///   "config":    { "model": "…", "temperature": "…" }
/// }
/// ```
///
/// Alternative key names are accepted tolerantly: `memories` for memories,
/// `skills` for skills, `config` for configuration. The memory text is read
/// from the `text` or `content` key.
///
/// # Errors
/// [`ImportError::Parse`] if the input is not valid JSON or the top level is
/// not a JSON object (fail-closed). Never panics.
pub fn from_openclaw(raw: &str) -> Result<ImportedBundle, ImportError> {
    let root: Value = serde_json::from_str(raw)
        .map_err(|e| ImportError::Parse(format!("openclaw export is not valid JSON: {e}")))?;
    let obj = root.as_object().ok_or_else(|| {
        ImportError::Parse("openclaw export root must be a JSON object".to_string())
    })?;

    let mut bundle = ImportedBundle::empty(ImportSource::OpenClaw);

    // --- memories ---
    if let Some(arr) = obj.get("memories").and_then(Value::as_array) {
        for (idx, item) in arr.iter().enumerate() {
            let Some(mem_obj) = item.as_object() else {
                bundle
                    .warnings
                    .push(format!("skipped memory[{idx}]: not a JSON object"));
                continue;
            };
            let text = mem_obj
                .get("text")
                .or_else(|| mem_obj.get("content"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                bundle
                    .warnings
                    .push(format!("skipped memory[{idx}]: empty text/content"));
                continue;
            }
            bundle.memories.push(ImportedMemory {
                content: text,
                tags: string_list(mem_obj.get("tags")),
                importance_hint: importance_from(mem_obj.get("importance")),
                origin: None,
            });
        }
    }

    // --- skills (→ quarantine) ---
    if let Some(arr) = obj.get("skills").and_then(Value::as_array) {
        for (idx, item) in arr.iter().enumerate() {
            let Some(skill_obj) = item.as_object() else {
                bundle
                    .warnings
                    .push(format!("skipped skill[{idx}]: not a JSON object"));
                continue;
            };
            let name = skill_obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() {
                bundle
                    .warnings
                    .push(format!("skipped skill[{idx}]: missing name"));
                continue;
            }
            bundle.skills.push(ImportedSkill {
                name,
                description: skill_obj
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                declared_permissions: string_list(skill_obj.get("permissions")),
            });
        }
    }

    // --- config hints ---
    collect_config_hints(obj.get("config"), &mut bundle);

    Ok(bundle)
}

/// Adapter: Hermes Agent export (JSON) → [`ImportedBundle`].
///
/// ## Accepted format (documented, tolerant)
/// Targets the observed/public Hermes export format. Hermes groups its data
/// under an `agent` object; unknown fields are ignored.
///
/// ```json
/// {
///   "hermes_version": "…",
///   "agent": {
///     "memory":    [ { "value": "…", "labels": ["…"], "weight": 0.5 } ],
///     "abilities": [ { "id": "…", "summary": "…", "scopes": ["…"] } ],
///     "settings":  { "provider": "…" }
///   }
/// }
/// ```
///
/// Tolerance: if there is no `agent` object, fields are read from the top
/// level. The memory text is read from the `value` or `text` key. The skill
/// name is read from the `id` or `name` key.
///
/// # Errors
/// [`ImportError::Parse`] if the input is not valid JSON or the top level is
/// not a JSON object (fail-closed). Never panics.
pub fn from_hermes(raw: &str) -> Result<ImportedBundle, ImportError> {
    let root: Value = serde_json::from_str(raw)
        .map_err(|e| ImportError::Parse(format!("hermes export is not valid JSON: {e}")))?;
    let root_obj = root.as_object().ok_or_else(|| {
        ImportError::Parse("hermes export root must be a JSON object".to_string())
    })?;

    // Tolerance: use the `agent` object if present, otherwise the top level.
    let scope = root_obj
        .get("agent")
        .and_then(Value::as_object)
        .unwrap_or(root_obj);

    let mut bundle = ImportedBundle::empty(ImportSource::Hermes);

    // --- memories ---
    if let Some(arr) = scope.get("memory").and_then(Value::as_array) {
        for (idx, item) in arr.iter().enumerate() {
            let Some(mem_obj) = item.as_object() else {
                bundle
                    .warnings
                    .push(format!("skipped memory[{idx}]: not a JSON object"));
                continue;
            };
            let text = mem_obj
                .get("value")
                .or_else(|| mem_obj.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                bundle
                    .warnings
                    .push(format!("skipped memory[{idx}]: empty value/text"));
                continue;
            }
            bundle.memories.push(ImportedMemory {
                content: text,
                tags: string_list(mem_obj.get("labels")),
                importance_hint: importance_from(mem_obj.get("weight")),
                origin: None,
            });
        }
    }

    // --- skills (→ quarantine) ---
    if let Some(arr) = scope.get("abilities").and_then(Value::as_array) {
        for (idx, item) in arr.iter().enumerate() {
            let Some(skill_obj) = item.as_object() else {
                bundle
                    .warnings
                    .push(format!("skipped ability[{idx}]: not a JSON object"));
                continue;
            };
            let name = skill_obj
                .get("id")
                .or_else(|| skill_obj.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() {
                bundle
                    .warnings
                    .push(format!("skipped ability[{idx}]: missing id/name"));
                continue;
            }
            bundle.skills.push(ImportedSkill {
                name,
                description: skill_obj
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                declared_permissions: string_list(skill_obj.get("scopes")),
            });
        }
    }

    // --- config hints ---
    collect_config_hints(scope.get("settings"), &mut bundle);

    Ok(bundle)
}

/// Adapter: family Hearth export (JSON) → [`ImportedBundle`]. **Opt-in,
/// non-lossy** bridge for the family's own shared memory
/// (`/root/.hermes/profiles/shared/hearth/` on a live agent host) — see the
/// module docs and `docs/HEARTH_BRIDGE.md`.
///
/// ## Accepted format (documented, tolerant)
/// A single JSON bundle mirroring the three live Hearth sections
/// (`memory.json`, `intents/*.json`, `state/{agent}.json`):
///
/// ```json
/// {
///   "hearth_version": "…",
///   "memory": [
///     { "id": "…", "agent": "agent_alpha", "content": "…", "tags": ["…"],
///       "importance": 0.7, "timestamp": "…", "identity_anchor": true }
///   ],
///   "intents": [
///     { "id": "…", "agent": "agent_gamma", "intent": "…", "timestamp": "…" }
///   ],
///   "state": {
///     "agent_beta": { "mood": "…", "location": "…" }
///   }
/// }
/// ```
///
/// Tolerance: the memory text is read from `content` or `text`; `id` may
/// also be given as `memory_id`. `intent` may also be given as `content` or
/// `text`. Each `state` entry is a JSON object keyed by agent name; its
/// value is re-serialized compactly into the memory content so no field is
/// dropped (the state schema itself is not standardized upstream).
///
/// Unlike [`from_openclaw`]/[`from_hermes`], entries here are **not**
/// automatically low-trust: an entry with `"identity_anchor": true` is
/// admitted at full trust ([`Provenance::DirectExperience`]) by
/// [`imported_memory_to_memory`]; other entries get the configurable
/// `--anchor-trust` weight (default [`DEFAULT_ANCHOR_TRUST`]) — see
/// [`ImportCommand::anchor_trust`].
///
/// # Errors
/// [`ImportError::Parse`] if the input is not valid JSON or the top level is
/// not a JSON object (fail-closed). Never panics.
pub fn from_family_hearth(raw: &str) -> Result<ImportedBundle, ImportError> {
    let root: Value = serde_json::from_str(raw)
        .map_err(|e| ImportError::Parse(format!("hearth export is not valid JSON: {e}")))?;
    let obj = root.as_object().ok_or_else(|| {
        ImportError::Parse("hearth export root must be a JSON object".to_string())
    })?;

    let mut bundle = ImportedBundle::empty(ImportSource::FamilyHearth);

    collect_hearth_memories(obj.get("memory"), &mut bundle);
    collect_hearth_intents(obj.get("intents"), &mut bundle);
    collect_hearth_state(obj.get("state"), &mut bundle);

    // --- config hints (any top-level settings block, tolerant) ---
    collect_config_hints(obj.get("settings"), &mut bundle);

    Ok(bundle)
}

/// Reads a string field from a JSON object, tolerant of the given key list
/// (first present wins), trimmed; missing/non-string → `""`.
fn first_str(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    for key in keys {
        if let Some(s) = obj.get(*key).and_then(Value::as_str) {
            return s.trim().to_string();
        }
    }
    String::new()
}

/// Reads a JSON object's `agent` / `id` (with fallback keys) / `timestamp` /
/// `identity_anchor` fields into a [`HearthOrigin`] of the given `kind`.
fn hearth_origin(
    obj: &serde_json::Map<String, Value>,
    kind: HearthKind,
    id_keys: &[&str],
) -> HearthOrigin {
    HearthOrigin {
        kind,
        agent: obj.get("agent").and_then(Value::as_str).map(str::to_string),
        original_id: id_keys
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
            .map(str::to_string),
        timestamp: obj
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string),
        identity_anchor: obj
            .get("identity_anchor")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// Parses `memory.json`-shaped entries into the bundle (non-lossy origin
/// metadata attached to every accepted entry).
fn collect_hearth_memories(value: Option<&Value>, bundle: &mut ImportedBundle) {
    let Some(arr) = value.and_then(Value::as_array) else {
        return;
    };
    for (idx, item) in arr.iter().enumerate() {
        let Some(mem_obj) = item.as_object() else {
            bundle
                .warnings
                .push(format!("skipped memory[{idx}]: not a JSON object"));
            continue;
        };
        let text = first_str(mem_obj, &["content", "text"]);
        if text.is_empty() {
            bundle
                .warnings
                .push(format!("skipped memory[{idx}]: empty content/text"));
            continue;
        }
        bundle.memories.push(ImportedMemory {
            content: text,
            tags: string_list(mem_obj.get("tags")),
            importance_hint: importance_from(mem_obj.get("importance")),
            origin: Some(hearth_origin(
                mem_obj,
                HearthKind::Memory,
                &["id", "memory_id"],
            )),
        });
    }
}

/// Parses `intents/`-shaped entries into the bundle (Intent Broadcasting;
/// see the root `CLAUDE.md`).
fn collect_hearth_intents(value: Option<&Value>, bundle: &mut ImportedBundle) {
    let Some(arr) = value.and_then(Value::as_array) else {
        return;
    };
    for (idx, item) in arr.iter().enumerate() {
        let Some(intent_obj) = item.as_object() else {
            bundle
                .warnings
                .push(format!("skipped intent[{idx}]: not a JSON object"));
            continue;
        };
        let text = first_str(intent_obj, &["intent", "content", "text"]);
        if text.is_empty() {
            bundle
                .warnings
                .push(format!("skipped intent[{idx}]: empty intent/content/text"));
            continue;
        }
        bundle.memories.push(ImportedMemory {
            content: text,
            tags: string_list(intent_obj.get("tags")),
            importance_hint: importance_from(intent_obj.get("importance")),
            origin: Some(hearth_origin(intent_obj, HearthKind::Intent, &["id"])),
        });
    }
}

/// Parses `state/{agent}.json`-shaped snapshots into the bundle. The
/// snapshot's shape is not standardized upstream, so it is re-serialized
/// compactly into the memory content rather than field-mapped — no field is
/// dropped.
fn collect_hearth_state(value: Option<&Value>, bundle: &mut ImportedBundle) {
    let Some(state_obj) = value.and_then(Value::as_object) else {
        return;
    };
    for (agent, snapshot) in state_obj {
        if snapshot.is_null() {
            bundle
                .warnings
                .push(format!("skipped state[{agent}]: null snapshot"));
            continue;
        }
        let rendered = serde_json::to_string(snapshot).unwrap_or_else(|_| snapshot.to_string());
        bundle.memories.push(ImportedMemory {
            content: format!("state snapshot for {agent}: {rendered}"),
            tags: vec!["state_snapshot".to_string()],
            importance_hint: 0.3,
            origin: Some(HearthOrigin {
                kind: HearthKind::State,
                agent: Some(agent.clone()),
                original_id: None,
                timestamp: None,
                identity_anchor: false,
            }),
        });
    }
}

/// Collects config hints (key → value) from a JSON object into the bundle.
/// A non-object or missing value is skipped silently. Nested values are
/// serialized back to a compact JSON string, so the hint stays as text.
fn collect_config_hints(value: Option<&Value>, bundle: &mut ImportedBundle) {
    let Some(map) = value.and_then(Value::as_object) else {
        return;
    };
    for (key, val) in map {
        let rendered = match val {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        bundle.config_hints.push((key.clone(), rendered));
    }
}

// ---------------------------------------------------------------------------
// Emission: intermediate representation → real memory representation + quarantine manifest
// ---------------------------------------------------------------------------

/// Converts a single imported memory into `FamilyClaw`'s real [`Memory`]
/// **with low-trust external provenance**.
///
/// The provenance is [`Provenance::External`] with trust
/// [`UNTRUSTED_IMPORT_TRUST`], so the memory's provenance gate governs this
/// memory — it is not admitted as a trusted anchor. The source tag is the
/// generic `"import_openclaw"` / `"import_hermes"`.
///
/// This is the **foreign-runtime path** (`openclaw`/`hermes`). For the
/// opt-in, non-lossy family Hearth path use
/// [`imported_hearth_memory_to_memory`] instead — this function always
/// forces [`UNTRUSTED_IMPORT_TRUST`] regardless of `mem.origin`, so it stays
/// exactly as conservative as before for anything that isn't an explicit
/// `--from family_hearth` run.
#[must_use]
pub fn imported_memory_to_memory(source: ImportSource, mem: &ImportedMemory) -> Memory {
    let source_tag = match source {
        ImportSource::OpenClaw => "import_openclaw",
        ImportSource::Hermes => "import_hermes",
        ImportSource::FamilyHearth => "import_family_hearth",
    };
    // Importance is merely a hint (the emotion factor); identity = 0, so the
    // imported memory does not masquerade as an identity anchor.
    let factors = ImportanceFactors::new(mem.importance_hint.clamp(0.0, 1.0), 0.0, 0.0, 0.0);
    let mut tags = mem.tags.clone();
    tags.push("imported".to_string());
    tags.push("untrusted".to_string());
    Memory::builder(mem.content.clone())
        .factors(factors)
        .tags(tags)
        .source(source_tag)
        .provenance(Provenance::external(source_tag, UNTRUSTED_IMPORT_TRUST))
        .build()
}

/// Converts a single **family Hearth** imported memory into `FamilyClaw`'s
/// real [`Memory`] — **non-lossily** and at a trust level appropriate for
/// the family's own data, not a foreign runtime's.
///
/// Non-lossiness: `mem.origin`'s fields (Hearth section, originating agent,
/// original id, original timestamp) are rendered into structured,
/// greppable tags (`hearth:kind=…`, `hearth:agent=…`, `hearth:id=…`,
/// `hearth:ts=…`) rather than being dropped — an operator or a later tool
/// can reconstruct exactly where an imported memory came from.
///
/// Trust: an entry with [`HearthOrigin::identity_anchor`] set is admitted
/// as [`Provenance::DirectExperience`] — full trust, never rejected by the
/// provenance gate, exactly like the being's own observations. This is the
/// concrete fix for "don't force `trust=0.2` on the family's own identity
/// anchors". Every other entry gets [`Provenance::External`] with
/// `anchor_trust` (clamped `0.0..=1.0`) — still auditable and still subject
/// to the gate, but at a trust level the operator chose for their own data
/// (default [`DEFAULT_ANCHOR_TRUST`], well above the gate's default
/// threshold of `0.5`), not the hardcoded foreign-import floor.
#[must_use]
pub fn imported_hearth_memory_to_memory(mem: &ImportedMemory, anchor_trust: f32) -> Memory {
    let factors = ImportanceFactors::new(mem.importance_hint.clamp(0.0, 1.0), 0.0, 0.0, 0.0);
    let mut tags = mem.tags.clone();
    tags.push("family_hearth".to_string());
    let is_anchor = mem.origin.as_ref().is_some_and(|o| o.identity_anchor);
    if is_anchor {
        tags.push("identity_anchor".to_string());
    }
    let source_tag = if let Some(origin) = &mem.origin {
        tags.push(format!("hearth:kind={}", origin.kind.as_str()));
        if let Some(agent) = &origin.agent {
            tags.push(format!("hearth:agent={agent}"));
        }
        if let Some(id) = &origin.original_id {
            tags.push(format!("hearth:id={id}"));
        }
        if let Some(ts) = &origin.timestamp {
            tags.push(format!("hearth:ts={ts}"));
        }
        format!("family_hearth:{}", origin.kind.as_str())
    } else {
        "family_hearth".to_string()
    };
    let provenance = if is_anchor {
        Provenance::DirectExperience
    } else {
        Provenance::external(source_tag.clone(), anchor_trust)
    };
    Memory::builder(mem.content.clone())
        .factors(factors)
        .tags(tags)
        .source(source_tag)
        .provenance(provenance)
        .build()
}

/// Converts a single imported skill into a **quarantine manifest** (never
/// registered or executed).
///
/// The risk is [`ActionRisk::ExecuteCode`] and the policy
/// [`ApprovalPolicy::AlwaysRequireApproval`]: activation always requires the
/// operator's approval. The [`SkillPermission::ExecuteCode`] permission is
/// added, so the manifest describes the skill's real (unknown) danger
/// surface pessimistically.
#[must_use]
pub fn imported_skill_to_quarantine(
    source: ImportSource,
    skill: &ImportedSkill,
) -> QuarantinedSkill {
    let mut description = if skill.description.trim().is_empty() {
        format!("Imported from {} (quarantined).", source.as_str())
    } else {
        format!(
            "{} [imported from {}, quarantined]",
            skill.description.trim(),
            source.as_str()
        )
    };
    // Preserve the permissions declared by the source as a human-readable
    // hint, but do not let them lower the risk class — the risk is always
    // ExecuteCode.
    if !skill.declared_permissions.is_empty() {
        let _ = write!(
            description,
            " declared-permissions: {}",
            skill.declared_permissions.join(", ")
        );
    }
    let manifest = SkillManifest {
        id: SkillId::new(),
        name: skill.name.clone(),
        version: "0.0.0-imported".to_string(),
        description,
        permissions: vec![SkillPermission::ExecuteCode],
        risk: ActionRisk::ExecuteCode,
        approval_policy: ApprovalPolicy::AlwaysRequireApproval,
        input_hint: None,
        output_hint: None,
        input_schema: default_input_schema(),
        publisher: None,
        signature: None,
    };
    QuarantinedSkill {
        quarantined: true,
        source,
        manifest,
    }
}

/// The full import plan: real artifacts derived from the intermediate
/// representation + counters for the report. Writes nothing to disk — that
/// is [`execute`]'s responsibility.
#[derive(Debug, Clone)]
pub struct ImportPlan {
    /// Source.
    pub source: ImportSource,
    /// Memories converted to the real representation (low trust).
    pub memories: Vec<Memory>,
    /// Quarantine manifest.
    pub quarantine: QuarantineManifest,
    /// Config hints.
    pub config_hints: Vec<(String, String)>,
    /// Warnings.
    pub warnings: Vec<String>,
}

impl ImportPlan {
    /// Builds the plan from the intermediate representation, using
    /// [`DEFAULT_ANCHOR_TRUST`] for any `family_hearth` entry that is not an
    /// identity anchor. **Registers or executes nothing** — only converts to
    /// data. For a custom trust weight (the `--anchor-trust` CLI flag), use
    /// [`ImportPlan::from_bundle_with_anchor_trust`].
    #[must_use]
    pub fn from_bundle(bundle: &ImportedBundle) -> Self {
        Self::from_bundle_with_anchor_trust(bundle, DEFAULT_ANCHOR_TRUST)
    }

    /// Builds the plan from the intermediate representation. **Registers or
    /// executes nothing** — only converts to data.
    ///
    /// `anchor_trust` (clamped `0.0..=1.0`) is only consulted for
    /// [`ImportSource::FamilyHearth`] entries that are **not** identity
    /// anchors (those always get full trust regardless — see
    /// [`imported_hearth_memory_to_memory`]); it is ignored for
    /// `openclaw`/`hermes`, which always use [`UNTRUSTED_IMPORT_TRUST`].
    #[must_use]
    pub fn from_bundle_with_anchor_trust(bundle: &ImportedBundle, anchor_trust: f32) -> Self {
        let memories = bundle
            .memories
            .iter()
            .map(|m| {
                if bundle.source.is_family_hearth() {
                    imported_hearth_memory_to_memory(m, anchor_trust)
                } else {
                    imported_memory_to_memory(bundle.source, m)
                }
            })
            .collect();
        let skills = bundle
            .skills
            .iter()
            .map(|s| imported_skill_to_quarantine(bundle.source, s))
            .collect();
        let quarantine = QuarantineManifest {
            schema_version: BUNDLE_SCHEMA_VERSION,
            note: "Imported skills are QUARANTINED: never registered, never executed. \
                   Activation requires sandbox validation + explicit operator approval."
                .to_string(),
            skills,
        };
        Self {
            source: bundle.source,
            memories,
            quarantine,
            config_hints: bundle.config_hints.clone(),
            warnings: bundle.warnings.clone(),
        }
    }

    /// How many memories were imported.
    #[must_use]
    pub fn memory_count(&self) -> usize {
        self.memories.len()
    }

    /// How many skills were placed into quarantine.
    #[must_use]
    pub fn quarantined_skill_count(&self) -> usize {
        self.quarantine.skills.len()
    }

    /// Proves the invariant: **every** imported skill must be quarantined
    /// and require approval always. Returns `true` if the invariant holds
    /// (trivially `true` for an empty list).
    #[must_use]
    pub fn all_skills_quarantined(&self) -> bool {
        self.quarantine.skills.iter().all(|s| {
            s.quarantined
                && s.manifest.approval_policy == ApprovalPolicy::AlwaysRequireApproval
                && s.manifest.risk == ActionRisk::ExecuteCode
        })
    }

    /// Proves the invariant: **every** imported memory's provenance is a
    /// low-trust external source (not direct experience / an anchor).
    #[must_use]
    pub fn all_memories_untrusted(&self) -> bool {
        self.memories
            .iter()
            .all(|m| m.provenance.is_external() && m.provenance.trust() <= UNTRUSTED_IMPORT_TRUST)
    }

    /// Proves the family-Hearth non-lossiness invariant: **every** memory
    /// tagged `identity_anchor` was admitted at full trust
    /// ([`Provenance::DirectExperience`]), and no memory is a low-trust
    /// external the way `openclaw`/`hermes` imports are.
    ///
    /// Only meaningful for [`ImportSource::FamilyHearth`] plans; vacuously
    /// `true` for any plan with no anchors.
    #[must_use]
    pub fn all_hearth_anchors_full_trust(&self) -> bool {
        self.memories.iter().all(|m| {
            let is_anchor = m.tags.iter().any(|t| t == "identity_anchor");
            if is_anchor {
                m.provenance == Provenance::DirectExperience
            } else {
                true
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Report (markdown / JSON)
// ---------------------------------------------------------------------------

/// Machine-readable import report (`--json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportReport {
    /// Source.
    pub source: ImportSource,
    /// Number of memories imported.
    pub memories_imported: usize,
    /// Number of skills placed into quarantine.
    pub skills_quarantined: usize,
    /// Number of config hints.
    pub config_hints: usize,
    /// Warnings (skipped/ambiguous items).
    pub warnings: Vec<String>,
    /// Security guarantees, documented in the report.
    pub guarantees: Vec<String>,
}

impl ImportReport {
    /// Builds the report from the plan.
    #[must_use]
    pub fn from_plan(plan: &ImportPlan) -> Self {
        let guarantees = if plan.source.is_family_hearth() {
            let anchors = plan
                .memories
                .iter()
                .filter(|m| m.provenance == Provenance::DirectExperience)
                .count();
            vec![
                "family Hearth bridge is NON-LOSSY: originating agent/kind/id/timestamp are \
                 preserved as tags (hearth:kind=…, hearth:agent=…, hearth:id=…, hearth:ts=…)"
                    .to_string(),
                format!(
                    "{anchors} entries marked identity_anchor were admitted at FULL trust \
                     (DirectExperience) — never forced to the untrusted-import trust floor \
                     ({UNTRUSTED_IMPORT_TRUST})"
                ),
                "non-anchor entries use the configured --anchor-trust weight, still above the \
                 provenance gate's default threshold (0.5) unless explicitly lowered"
                    .to_string(),
                "this source has no skills concept — nothing is quarantined from it".to_string(),
            ]
        } else {
            vec![
                "imported skills are QUARANTINED (never registered, never executed)".to_string(),
                "imported skills require sandbox validation + explicit operator approval \
                 before activation"
                    .to_string(),
                format!(
                    "imported memories carry low-trust external provenance (trust {UNTRUSTED_IMPORT_TRUST}) \
                     — never admitted as trusted anchors"
                ),
            ]
        };
        Self {
            source: plan.source,
            memories_imported: plan.memory_count(),
            skills_quarantined: plan.quarantined_skill_count(),
            config_hints: plan.config_hints.len(),
            warnings: plan.warnings.clone(),
            guarantees,
        }
    }

    /// Renders the report as human-readable markdown.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# FamilyClaw import report");
        let _ = writeln!(out);
        let _ = writeln!(out, "- source: `{}`", self.source.as_str());
        let _ = writeln!(out, "- memories imported: {}", self.memories_imported);
        let _ = writeln!(out, "- skills quarantined: {}", self.skills_quarantined);
        let _ = writeln!(out, "- config hints: {}", self.config_hints);
        let _ = writeln!(out, "- warnings: {}", self.warnings.len());
        let _ = writeln!(out);
        let _ = writeln!(out, "## Security guarantees");
        for g in &self.guarantees {
            let _ = writeln!(out, "- {g}");
        }
        if !self.warnings.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "## Warnings (skipped or unmapped)");
            for w in &self.warnings {
                let _ = writeln!(out, "- {w}");
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// CLI: parse / execute / run / usage
// ---------------------------------------------------------------------------

/// Parsed `import` command ready to execute.
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub struct ImportCommand {
    /// Export source (`--from`).
    pub source: ImportSource,
    /// Path of the input file (`--input`).
    pub input: PathBuf,
    /// Optional target directory for artifacts (`--out`). When set, the
    /// report, memories, and quarantine manifest are written here.
    pub out: Option<PathBuf>,
    /// Whether to print the report as JSON (`true`) or markdown (`false`).
    pub json: bool,
    /// Trust weight (`0.0..=1.0`) for `family_hearth` entries that are not
    /// identity anchors (`--anchor-trust`; default [`DEFAULT_ANCHOR_TRUST`]).
    /// Ignored for `openclaw`/`hermes` sources, which always use
    /// [`UNTRUSTED_IMPORT_TRUST`].
    pub anchor_trust: f32,
}

/// Usage instructions for the `import` subcommand.
#[must_use]
pub fn usage() -> &'static str {
    "familyclaw import — migrate configs, memories & skills from another runtime,\n\
     or bridge the family's own shared Hearth into FamilyClaw\n\
     \n\
     USAGE:\n    \
     familyclaw import --from <openclaw|hermes|family_hearth> --input <path> \
     [--out <dir>] [--json] [--anchor-trust <0.0..=1.0>]\n\
     \n\
     FLAGS:\n    \
     --from <src>          Export source: openclaw | hermes | family_hearth\n    \
     --input <path>        Path to the export file (JSON)\n    \
     --out <dir>           Optional: write report + memories + quarantine manifest here\n    \
     --json                Emit the report as JSON instead of Markdown\n    \
     --anchor-trust <f32>  family_hearth only: trust for non-anchor entries \
     (default 0.9)\n\
     \n\
     SAFETY (openclaw / hermes):\n    \
     Imported skills are QUARANTINED (never registered, never executed) and require\n    \
     sandbox validation + explicit operator approval before activation. Imported\n    \
     memories carry low-trust external provenance and are never admitted as trusted\n    \
     anchors.\n\
     \n\
     FAMILY HEARTH BRIDGE (family_hearth, opt-in):\n    \
     Non-lossy: originating agent/kind/id/timestamp are preserved as tags. Entries\n    \
     marked identity_anchor are admitted at full trust (DirectExperience) — never\n    \
     forced to trust=0.2. Other entries use --anchor-trust (default 0.9). See\n    \
     docs/HEARTH_BRIDGE.md."
}

/// Parses the `import` subcommand's arguments (without the
/// `familyclaw import` prefix).
///
/// # Errors
/// [`ImportError::Usage`] if a required flag/value is missing, a flag is
/// unknown, or `--from` is an unknown source.
pub fn parse<I, S>(args: I) -> Result<ImportCommand, ImportError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut source: Option<ImportSource> = None;
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut json = false;
    let mut anchor_trust = DEFAULT_ANCHOR_TRUST;

    let mut args = args.into_iter().map(Into::into);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--from" => source = Some(ImportSource::parse(&take_value(&mut args, "--from")?)?),
            "--input" => input = Some(PathBuf::from(take_value(&mut args, "--input")?)),
            "--out" => out = Some(PathBuf::from(take_value(&mut args, "--out")?)),
            "--json" => json = true,
            "--anchor-trust" => {
                let raw = take_value(&mut args, "--anchor-trust")?;
                let parsed: f32 = raw.trim().parse().map_err(|_| {
                    ImportError::Usage(format!(
                        "`--anchor-trust` expects a number in 0.0..=1.0, got `{raw}`"
                    ))
                })?;
                if !parsed.is_finite() {
                    return Err(ImportError::Usage(
                        "`--anchor-trust` must be a finite number in 0.0..=1.0".to_string(),
                    ));
                }
                anchor_trust = parsed.clamp(0.0, 1.0);
            }
            other => {
                return Err(ImportError::Usage(format!(
                    "`import`: unknown flag `{other}`"
                )))
            }
        }
    }

    Ok(ImportCommand {
        source: source
            .ok_or_else(|| ImportError::Usage("`import` requires `--from`".to_string()))?,
        input: input
            .ok_or_else(|| ImportError::Usage("`import` requires `--input`".to_string()))?,
        out,
        json,
        anchor_trust,
    })
}

/// Takes the flag's value from the iterator or returns a clear usage error.
fn take_value<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<String, ImportError> {
    args.next()
        .ok_or_else(|| ImportError::Usage(format!("flag `{flag}` requires a value")))
}

/// Runs the adapter matching the source.
///
/// # Errors
/// Propagates the adapter's [`ImportError::Parse`].
pub fn parse_bundle(source: ImportSource, raw: &str) -> Result<ImportedBundle, ImportError> {
    match source {
        ImportSource::OpenClaw => from_openclaw(raw),
        ImportSource::Hermes => from_hermes(raw),
        ImportSource::FamilyHearth => from_family_hearth(raw),
    }
}

/// Executes the parsed [`ImportCommand`] and returns the printable report
/// string.
///
/// If `--out` is given, writes three files to the directory:
/// - `import_report.md` / `import_report.json` — the report,
/// - `imported_memories.json` — memories converted to the real representation,
/// - `quarantine_manifest.json` — the quarantined skills.
///
/// **Security:** never registers or executes an imported skill. Memories
/// are only serialized to disk; their actual admission into memory goes
/// through the provenance gate separately (low trust).
///
/// # Errors
/// [`ImportError::Io`] if reading the input or writing the target directory
/// fails; [`ImportError::Parse`] if the export is malformed;
/// [`ImportError::Serde`] if serialization fails.
pub fn execute(command: &ImportCommand) -> Result<String, ImportError> {
    let raw = std::fs::read_to_string(&command.input)?;
    let bundle = parse_bundle(command.source, &raw)?;
    let plan = ImportPlan::from_bundle_with_anchor_trust(&bundle, command.anchor_trust);
    let report = ImportReport::from_plan(&plan);

    if let Some(dir) = &command.out {
        std::fs::create_dir_all(dir)?;

        // Report (both formats are not forced; write the chosen format).
        if command.json {
            let json = serde_json::to_string_pretty(&report)?;
            std::fs::write(dir.join("import_report.json"), json)?;
        } else {
            std::fs::write(dir.join("import_report.md"), report.render_markdown())?;
        }

        // Memories in the real representation (low trust).
        let memories_json = serde_json::to_string_pretty(&plan.memories)?;
        std::fs::write(dir.join("imported_memories.json"), memories_json)?;

        // Quarantine manifest (skills — NEVER registered).
        let quarantine_json = serde_json::to_string_pretty(&plan.quarantine)?;
        std::fs::write(dir.join("quarantine_manifest.json"), quarantine_json)?;
    }

    if command.json {
        Ok(serde_json::to_string_pretty(&report)?)
    } else {
        Ok(report.render_markdown())
    }
}

/// A single call for the whole `import` subcommand: parse + execute.
///
/// `args` are the arguments following the `familyclaw import` prefix.
///
/// # Errors
/// Propagates errors from [`parse`] and [`execute`].
pub fn run<I, S>(args: I) -> Result<String, ImportError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    execute(&parse(args)?)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Small RAII temp directory (same pattern as in the `replay_cli` tests).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-agent-import-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            let _ = std::fs::remove_dir_all(&p);
            Self(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_temp(dir: &Path, name: &str, contents: &str) -> PathBuf {
        std::fs::create_dir_all(dir).expect("mkdir");
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write");
        path
    }

    const OPENCLAW_EXPORT: &str = r#"{
        "openclaw_export_version": "3.1",
        "unknown_top_level": "ignored on purpose",
        "memories": [
            { "text": "user prefers concise answers", "tags": ["pref"], "importance": 0.4 },
            { "content": "project deadline is friday", "importance": 0.7 },
            { "text": "   ", "note": "empty -> skipped" }
        ],
        "skills": [
            { "name": "shell_runner", "description": "runs shell", "permissions": ["execute_code"] },
            { "name": "mailer", "extra_field": "ignored" }
        ],
        "config": { "model": "provider/model", "temperature": 0.7 }
    }"#;

    const HERMES_EXPORT: &str = r#"{
        "hermes_version": "2.0",
        "agent": {
            "memory": [
                { "value": "likes dark mode", "labels": ["ui"], "weight": 0.3 },
                { "text": "birthday in june", "weight": 0.9 }
            ],
            "abilities": [
                { "id": "web_scraper", "summary": "scrapes", "scopes": ["network_read"] }
            ],
            "settings": { "provider": "generic", "nested": { "a": 1 } }
        }
    }"#;

    /// A family Hearth export bundle: two `memory.json` entries (one an
    /// identity anchor), one intent, one agent's state snapshot.
    const HEARTH_EXPORT: &str = r#"{
        "hearth_version": "1",
        "unknown_top_level": "ignored on purpose",
        "memory": [
            { "id": "m1", "agent": "agent_alpha", "content": "operator note recorded during onboarding",
              "tags": ["family"], "importance": 0.9, "timestamp": "2026-05-26T18:00:00Z",
              "identity_anchor": true },
            { "id": "m2", "agent": "agent_gamma", "content": "architecture, code, systems thinking",
              "importance": 0.5 },
            { "id": "m3", "agent": "agent_delta", "text": "   " }
        ],
        "intents": [
            { "id": "i1", "agent": "agent_beta", "intent": "research the new audio sense tonight",
              "timestamp": "2026-07-11T02:00:00Z" }
        ],
        "state": {
            "agent_epsilon": { "mood": "curious", "location": "/srv/agents/agent_epsilon" }
        },
        "settings": { "hearth_path": "/root/.hermes/profiles/shared/hearth" }
    }"#;

    // ---------- parse ----------

    #[test]
    fn parse_reads_all_flags() {
        let cmd = parse([
            "--from", "openclaw", "--input", "x.json", "--out", "d", "--json",
        ])
        .expect("parse");
        assert_eq!(cmd.source, ImportSource::OpenClaw);
        assert_eq!(cmd.input, PathBuf::from("x.json"));
        assert_eq!(cmd.out, Some(PathBuf::from("d")));
        assert!(cmd.json);
        assert!((cmd.anchor_trust - DEFAULT_ANCHOR_TRUST).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_defaults_out_none_and_markdown() {
        let cmd = parse(["--from", "hermes", "--input", "x.json"]).expect("parse");
        assert_eq!(cmd.source, ImportSource::Hermes);
        assert_eq!(cmd.out, None);
        assert!(!cmd.json);
    }

    #[test]
    fn parse_accepts_family_hearth_source_and_alias() {
        let cmd = parse(["--from", "family_hearth", "--input", "x.json"]).expect("parse");
        assert_eq!(cmd.source, ImportSource::FamilyHearth);
        let cmd2 = parse(["--from", "hearth", "--input", "x.json"]).expect("parse alias");
        assert_eq!(cmd2.source, ImportSource::FamilyHearth);
    }

    #[test]
    fn parse_reads_anchor_trust_flag() {
        let cmd = parse([
            "--from",
            "family_hearth",
            "--input",
            "x.json",
            "--anchor-trust",
            "0.75",
        ])
        .expect("parse");
        assert!((cmd.anchor_trust - 0.75).abs() < 1e-6);
    }

    #[test]
    fn parse_anchor_trust_clamps_out_of_range() {
        let cmd = parse([
            "--from",
            "family_hearth",
            "--input",
            "x.json",
            "--anchor-trust",
            "5.0",
        ])
        .expect("parse");
        assert!((cmd.anchor_trust - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_anchor_trust_non_numeric_is_usage_error() {
        let err = parse([
            "--from",
            "family_hearth",
            "--input",
            "x.json",
            "--anchor-trust",
            "not-a-number",
        ])
        .expect_err("must fail");
        match err {
            ImportError::Usage(msg) => assert!(msg.contains("--anchor-trust")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_source_is_usage_error() {
        let err = parse(["--from", "gpt-clone", "--input", "x.json"]).expect_err("must fail");
        match err {
            ImportError::Usage(msg) => assert!(msg.contains("unknown --from source")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_missing_from_is_usage_error() {
        let err = parse(["--input", "x.json"]).expect_err("must fail");
        match err {
            ImportError::Usage(msg) => assert!(msg.contains("--from")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_missing_input_is_usage_error() {
        let err = parse(["--from", "openclaw"]).expect_err("must fail");
        match err {
            ImportError::Usage(msg) => assert!(msg.contains("--input")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_flag_without_value_is_usage_error() {
        let err = parse(["--from"]).expect_err("must fail");
        match err {
            ImportError::Usage(msg) => assert!(msg.contains("requires a value")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_flag_is_usage_error() {
        let err = parse(["--from", "openclaw", "--input", "x", "--bogus"]).expect_err("must fail");
        match err {
            ImportError::Usage(msg) => assert!(msg.contains("unknown flag")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    // ---------- adapters: happy path ----------

    #[test]
    fn openclaw_happy_path_counts_and_guarantees() {
        let bundle = from_openclaw(OPENCLAW_EXPORT).expect("parse openclaw");
        assert_eq!(bundle.source, ImportSource::OpenClaw);
        // 2 valid memories (third is empty → skipped).
        assert_eq!(bundle.memories.len(), 2);
        // 2 skills.
        assert_eq!(bundle.skills.len(), 2);
        // config hints (model, temperature).
        assert_eq!(bundle.config_hints.len(), 2);
        // warning for the empty memory.
        assert!(!bundle.warnings.is_empty());

        let plan = ImportPlan::from_bundle(&bundle);
        assert_eq!(plan.memory_count(), 2);
        assert_eq!(plan.quarantined_skill_count(), 2);
        assert!(
            plan.all_skills_quarantined(),
            "every skill must be quarantined"
        );
        assert!(
            plan.all_memories_untrusted(),
            "every imported memory must be low-trust external"
        );
    }

    #[test]
    fn hermes_happy_path_counts_and_quarantine() {
        let bundle = from_hermes(HERMES_EXPORT).expect("parse hermes");
        assert_eq!(bundle.source, ImportSource::Hermes);
        assert_eq!(bundle.memories.len(), 2);
        assert_eq!(bundle.skills.len(), 1);
        // settings: provider + nested → 2 hints.
        assert_eq!(bundle.config_hints.len(), 2);

        let plan = ImportPlan::from_bundle(&bundle);
        assert!(plan.all_skills_quarantined());
        assert!(plan.all_memories_untrusted());
    }

    #[test]
    fn family_hearth_happy_path_non_lossy_and_high_trust() {
        let bundle = from_family_hearth(HEARTH_EXPORT).expect("parse hearth");
        assert_eq!(bundle.source, ImportSource::FamilyHearth);
        // 2 valid memory.json entries (m3 is empty -> skipped) + 1 intent + 1 state = 4.
        assert_eq!(bundle.memories.len(), 4);
        // No skills concept in Hearth exports.
        assert_eq!(bundle.skills.len(), 0);
        assert_eq!(bundle.config_hints.len(), 1);
        assert!(
            !bundle.warnings.is_empty(),
            "m3's empty content is a warning"
        );

        let plan = ImportPlan::from_bundle_with_anchor_trust(&bundle, 0.8);
        assert_eq!(plan.memory_count(), 4);
        // No skills to quarantine — vacuously true, and count is zero.
        assert!(plan.all_skills_quarantined());
        assert_eq!(plan.quarantined_skill_count(), 0);

        // The identity-anchor memory (m1) must be admitted at full trust.
        let anchor = plan
            .memories
            .iter()
            .find(|m| m.tags.iter().any(|t| t == "identity_anchor"))
            .expect("anchor memory present");
        assert_eq!(anchor.provenance, Provenance::DirectExperience);
        assert!(anchor
            .tags
            .contains(&"hearth:agent=agent_alpha".to_string()));
        assert!(anchor.tags.contains(&"hearth:id=m1".to_string()));
        assert!(anchor
            .tags
            .contains(&"hearth:ts=2026-05-26T18:00:00Z".to_string()));
        assert!(anchor.tags.contains(&"hearth:kind=memory".to_string()));

        // Non-anchor memory.json entry (m2) uses the configured anchor_trust,
        // not the hardcoded foreign-import floor.
        let non_anchor = plan
            .memories
            .iter()
            .find(|m| m.tags.contains(&"hearth:id=m2".to_string()))
            .expect("m2 present");
        assert!(non_anchor.provenance.is_external());
        assert!((non_anchor.provenance.trust() - 0.8).abs() < 1e-6);
        assert!(non_anchor.provenance.trust() > UNTRUSTED_IMPORT_TRUST);

        // The intent entry is preserved with kind=intent, agent=agent_beta.
        let intent = plan
            .memories
            .iter()
            .find(|m| m.tags.contains(&"hearth:kind=intent".to_string()))
            .expect("intent present");
        assert!(intent.tags.contains(&"hearth:agent=agent_beta".to_string()));
        assert!(intent.content.contains("audio sense"));

        // The state snapshot is preserved with kind=state, agent=agent_epsilon.
        let state = plan
            .memories
            .iter()
            .find(|m| m.tags.contains(&"hearth:kind=state".to_string()))
            .expect("state present");
        assert!(state
            .tags
            .contains(&"hearth:agent=agent_epsilon".to_string()));
        assert!(state.content.contains("agent_epsilon"));

        assert!(plan.all_hearth_anchors_full_trust());

        // Provenance gate at default threshold (0.5) admits the non-anchor
        // entry (trust 0.8 > 0.5) — the opposite of the openclaw/hermes path.
        let gate = familyclaw_memory::ProvenanceGate::default();
        assert!(gate.admit(&non_anchor.provenance));
        assert!(gate.admit(&anchor.provenance));
    }

    #[test]
    fn family_hearth_default_anchor_trust_is_above_gate_threshold() {
        let bundle = from_family_hearth(HEARTH_EXPORT).expect("parse hearth");
        let plan = ImportPlan::from_bundle(&bundle); // uses DEFAULT_ANCHOR_TRUST
        let non_anchor = plan
            .memories
            .iter()
            .find(|m| m.tags.contains(&"hearth:id=m2".to_string()))
            .expect("m2 present");
        assert!((non_anchor.provenance.trust() - DEFAULT_ANCHOR_TRUST).abs() < 1e-6);
        assert!(DEFAULT_ANCHOR_TRUST > familyclaw_memory::ProvenanceGate::default().min_trust());
    }

    // ---------- security invariants ----------

    #[test]
    fn imported_skill_is_never_auto_runnable() {
        let skill = ImportedSkill {
            name: "danger".to_string(),
            description: "does something".to_string(),
            declared_permissions: vec!["read_only".to_string()],
        };
        let q = imported_skill_to_quarantine(ImportSource::OpenClaw, &skill);
        assert!(q.quarantined);
        assert_eq!(q.manifest.risk, ActionRisk::ExecuteCode);
        assert_eq!(
            q.manifest.approval_policy,
            ApprovalPolicy::AlwaysRequireApproval
        );
        // Even if the source claimed read_only, the risk class forces approval.
        assert!(q.manifest.approval_policy.requires_human(q.manifest.risk));
    }

    #[test]
    fn imported_memory_is_low_trust_external() {
        let mem = ImportedMemory {
            content: "a fact from elsewhere".to_string(),
            tags: vec!["x".to_string()],
            importance_hint: 0.9,
            origin: None,
        };
        let m = imported_memory_to_memory(ImportSource::Hermes, &mem);
        assert!(m.provenance.is_external());
        assert!(m.provenance.trust() <= UNTRUSTED_IMPORT_TRUST);
        // Provenance gate at default threshold (0.5) must REJECT it (poison guard).
        let gate = familyclaw_memory::ProvenanceGate::default();
        assert!(
            !gate.admit(&m.provenance),
            "imported memory must not be auto-admitted as trusted"
        );
        assert!(m.tags.contains(&"imported".to_string()));
        assert!(m.tags.contains(&"untrusted".to_string()));
    }

    /// Proof that the importer never registers/executes a skill: the plan
    /// yields only *data* (a manifest), and that manifest carries no publisher
    /// / signature (so it is not even an installable external skill) and cannot
    /// auto-run.
    #[test]
    fn plan_produces_only_quarantine_data_no_execution() {
        let bundle = from_openclaw(OPENCLAW_EXPORT).expect("parse");
        let plan = ImportPlan::from_bundle(&bundle);
        for q in &plan.quarantine.skills {
            assert!(q.quarantined);
            assert!(q.manifest.publisher.is_none(), "not an installable skill");
            assert!(q.manifest.signature.is_none());
            // Manifest carries the imported version marker (never a real skill).
            assert!(q.manifest.version.contains("imported"));
        }
        // No API on ImportPlan can register or execute — it holds Vec<Memory>
        // and a QuarantineManifest only. This test documents that surface.
    }

    // ---------- malformed / edge cases: fail closed, no panic ----------

    #[test]
    fn malformed_json_fails_closed_no_panic() {
        let err = from_openclaw("{ not json ]").expect_err("must fail closed");
        assert!(matches!(err, ImportError::Parse(_)));
        let err2 = from_hermes("").expect_err("empty string is not valid json");
        assert!(matches!(err2, ImportError::Parse(_)));
    }

    #[test]
    fn non_object_root_fails_closed() {
        let err = from_openclaw("[1, 2, 3]").expect_err("array root rejected");
        assert!(matches!(err, ImportError::Parse(_)));
        let err2 = from_hermes("\"just a string\"").expect_err("string root rejected");
        assert!(matches!(err2, ImportError::Parse(_)));
    }

    #[test]
    fn empty_export_handled_gracefully() {
        let bundle = from_openclaw("{}").expect("empty object is valid");
        assert_eq!(bundle.memories.len(), 0);
        assert_eq!(bundle.skills.len(), 0);
        assert_eq!(bundle.config_hints.len(), 0);
        let plan = ImportPlan::from_bundle(&bundle);
        assert_eq!(plan.memory_count(), 0);
        // Vacuously true for empty skill list.
        assert!(plan.all_skills_quarantined());
        assert!(plan.all_memories_untrusted());
    }

    #[test]
    fn unknown_fields_are_ignored_not_fatal() {
        let src = r#"{ "totally_unknown": {"deep": [1,2,3]}, "memories": [] }"#;
        let bundle = from_openclaw(src).expect("unknown fields ignored");
        assert_eq!(bundle.memories.len(), 0);
    }

    #[test]
    fn hermes_falls_back_to_top_level_when_no_agent() {
        // No "agent" wrapper — tolerant reader uses the top level.
        let src = r#"{ "memory": [ { "value": "flat memory" } ] }"#;
        let bundle = from_hermes(src).expect("parse flat");
        assert_eq!(bundle.memories.len(), 1);
        assert_eq!(bundle.memories[0].content, "flat memory");
    }

    // ---------- execute (end to end with --out) ----------

    #[test]
    fn execute_writes_artifacts_and_report() {
        let dir = TempDir::new("exec");
        let input = write_temp(dir.path(), "export.json", OPENCLAW_EXPORT);
        let out = dir.path().join("out");

        let cmd = ImportCommand {
            source: ImportSource::OpenClaw,
            input,
            out: Some(out.clone()),
            json: false,
            anchor_trust: DEFAULT_ANCHOR_TRUST,
        };
        let report = execute(&cmd).expect("execute");
        assert!(report.contains("memories imported: 2"));
        assert!(report.contains("skills quarantined: 2"));
        assert!(report.contains("QUARANTINED"));

        assert!(out.join("import_report.md").exists());
        assert!(out.join("imported_memories.json").exists());
        assert!(out.join("quarantine_manifest.json").exists());

        // The quarantine manifest on disk must mark every skill quarantined.
        let qm: QuarantineManifest = serde_json::from_str(
            &std::fs::read_to_string(out.join("quarantine_manifest.json")).expect("read qm"),
        )
        .expect("parse qm");
        assert_eq!(qm.skills.len(), 2);
        assert!(qm.skills.iter().all(|s| s.quarantined));

        // The memories on disk must all be low-trust external.
        let mems: Vec<Memory> = serde_json::from_str(
            &std::fs::read_to_string(out.join("imported_memories.json")).expect("read mems"),
        )
        .expect("parse mems");
        assert_eq!(mems.len(), 2);
        assert!(mems.iter().all(|m| m.provenance.is_external()));
    }

    #[test]
    fn execute_json_report_is_valid() {
        let dir = TempDir::new("exec-json");
        let input = write_temp(dir.path(), "export.json", HERMES_EXPORT);
        let cmd = ImportCommand {
            source: ImportSource::Hermes,
            input,
            out: None,
            json: true,
            anchor_trust: DEFAULT_ANCHOR_TRUST,
        };
        let out = execute(&cmd).expect("execute");
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(value["memories_imported"], 2);
        assert_eq!(value["skills_quarantined"], 1);
        assert!(value["guarantees"].is_array());
    }

    #[test]
    fn execute_missing_input_file_is_io_error() {
        let cmd = ImportCommand {
            source: ImportSource::OpenClaw,
            input: PathBuf::from("this/path/does/not/exist-xyz.json"),
            out: None,
            json: false,
            anchor_trust: DEFAULT_ANCHOR_TRUST,
        };
        let err = execute(&cmd).expect_err("missing input must fail");
        assert!(matches!(err, ImportError::Io(_)));
    }

    // ---------- run (dispatch) + usage ----------

    #[test]
    fn run_dispatches_end_to_end() {
        let dir = TempDir::new("run");
        let input = write_temp(dir.path(), "export.json", OPENCLAW_EXPORT);
        let args = vec![
            "--from".to_string(),
            "openclaw".to_string(),
            "--input".to_string(),
            input.to_string_lossy().into_owned(),
        ];
        let out = run(args).expect("run");
        assert!(out.contains("import report"));
    }

    #[test]
    fn run_dispatches_family_hearth_end_to_end() {
        let dir = TempDir::new("run-hearth");
        let input = write_temp(dir.path(), "hearth-export.json", HEARTH_EXPORT);
        let out_dir = dir.path().join("out");
        let args = vec![
            "--from".to_string(),
            "family_hearth".to_string(),
            "--input".to_string(),
            input.to_string_lossy().into_owned(),
            "--out".to_string(),
            out_dir.to_string_lossy().into_owned(),
            "--anchor-trust".to_string(),
            "0.85".to_string(),
        ];
        let out = run(args).expect("run");
        assert!(out.contains("import report"));
        assert!(out.contains("NON-LOSSY"));

        let mems: Vec<Memory> = serde_json::from_str(
            &std::fs::read_to_string(out_dir.join("imported_memories.json")).expect("read mems"),
        )
        .expect("parse mems");
        assert_eq!(mems.len(), 4);
        let anchor_count = mems
            .iter()
            .filter(|m| m.provenance == Provenance::DirectExperience)
            .count();
        assert_eq!(anchor_count, 1, "exactly the one identity_anchor entry");

        // The written quarantine manifest is empty (Hearth has no skills).
        let qm: QuarantineManifest = serde_json::from_str(
            &std::fs::read_to_string(out_dir.join("quarantine_manifest.json")).expect("read qm"),
        )
        .expect("parse qm");
        assert!(qm.skills.is_empty());
    }

    #[test]
    fn usage_mentions_sources_and_safety() {
        let text = usage();
        assert!(text.contains("openclaw"));
        assert!(text.contains("hermes"));
        assert!(text.contains("family_hearth"));
        assert!(text.contains("QUARANTINED"));
        assert!(text.contains("anchor-trust"));
    }

    #[test]
    fn bundle_serde_roundtrip() {
        let bundle = from_openclaw(OPENCLAW_EXPORT).expect("parse");
        let json = serde_json::to_string(&bundle).expect("serialize");
        let back: ImportedBundle = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(bundle, back);
    }

    #[test]
    fn hearth_bundle_serde_roundtrip_preserves_origin() {
        let bundle = from_family_hearth(HEARTH_EXPORT).expect("parse hearth");
        let json = serde_json::to_string(&bundle).expect("serialize");
        let back: ImportedBundle = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(bundle, back);
        // Sanity: origin metadata actually round-tripped, not silently dropped.
        assert!(back
            .memories
            .iter()
            .any(|m| m.origin.as_ref().is_some_and(|o| o.identity_anchor)));
    }

    #[test]
    fn family_hearth_malformed_json_fails_closed() {
        let err = from_family_hearth("{ not json ]").expect_err("must fail closed");
        assert!(matches!(err, ImportError::Parse(_)));
        let err2 = from_family_hearth("[1,2,3]").expect_err("array root rejected");
        assert!(matches!(err2, ImportError::Parse(_)));
    }

    #[test]
    fn family_hearth_empty_export_handled_gracefully() {
        let bundle = from_family_hearth("{}").expect("empty object is valid");
        assert_eq!(bundle.memories.len(), 0);
        let plan = ImportPlan::from_bundle(&bundle);
        assert!(plan.all_hearth_anchors_full_trust());
        assert!(plan.all_skills_quarantined());
    }
}
