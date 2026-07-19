//! `familyclaw import` — `OpenClaw` / `Hermes` migration tool ("replacement path").
//!
//! This module implements the `import` subcommand of the `familyclaw` binary: it
//! reads the **export** of another agent runtime (`OpenClaw` or `Hermes`) and
//! converts it into `FamilyClaw`'s own representation — **safely and honestly**.
//!
//! ```text
//! familyclaw import --from openclaw|hermes --input <path> [--out <dir>] [--json]
//! ```
//!
//! ## Why this is security-sensitive
//! Imported data is **untrusted input from another system**. Two structural
//! guarantees protect the runtime (see also `docs/MIGRATION.md`):
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
//! ## Design constraint (honesty)
//! We do **not** have `OpenClaw`'s/`Hermes`'s exact, closed export schema.
//! Therefore we define a small, versioned **intermediate representation**
//! ([`ImportedBundle`]) and write two *tolerant* adapters ([`from_openclaw`],
//! [`from_hermes`]) that parse a documented, plausible JSON format
//! (described in doc comments + `docs/MIGRATION.md`). Unknown fields are
//! **ignored** — they are never fatal. Malformed input fails **fail-closed**
//! with a clear error — never a panic.
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
}

impl ImportSource {
    /// Parses the source from a `--from` value (unknown → [`ImportError::Usage`]).
    ///
    /// # Errors
    /// [`ImportError::Usage`] if the value is not `openclaw` or `hermes`.
    pub fn parse(value: &str) -> Result<Self, ImportError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openclaw" => Ok(Self::OpenClaw),
            "hermes" => Ok(Self::Hermes),
            other => Err(ImportError::Usage(format!(
                "unknown --from source `{other}` (expected openclaw|hermes)"
            ))),
        }
    }

    /// Stable, machine-readable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
        }
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
#[must_use]
pub fn imported_memory_to_memory(source: ImportSource, mem: &ImportedMemory) -> Memory {
    let source_tag = match source {
        ImportSource::OpenClaw => "import_openclaw",
        ImportSource::Hermes => "import_hermes",
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
    /// Builds the plan from the intermediate representation. **Registers or
    /// executes nothing** — only converts to data.
    #[must_use]
    pub fn from_bundle(bundle: &ImportedBundle) -> Self {
        let memories = bundle
            .memories
            .iter()
            .map(|m| imported_memory_to_memory(bundle.source, m))
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
        Self {
            source: plan.source,
            memories_imported: plan.memory_count(),
            skills_quarantined: plan.quarantined_skill_count(),
            config_hints: plan.config_hints.len(),
            warnings: plan.warnings.clone(),
            guarantees: vec![
                "imported skills are QUARANTINED (never registered, never executed)".to_string(),
                "imported skills require sandbox validation + explicit operator approval \
                 before activation"
                    .to_string(),
                format!(
                    "imported memories carry low-trust external provenance (trust {UNTRUSTED_IMPORT_TRUST}) \
                     — never admitted as trusted anchors"
                ),
            ],
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
#[derive(Debug, PartialEq, Eq)]
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
}

/// Usage instructions for the `import` subcommand.
#[must_use]
pub fn usage() -> &'static str {
    "familyclaw import — migrate configs, memories & skills from another runtime\n\
     \n\
     USAGE:\n    \
     familyclaw import --from <openclaw|hermes> --input <path> [--out <dir>] [--json]\n\
     \n\
     FLAGS:\n    \
     --from <src>     Export source: openclaw | hermes\n    \
     --input <path>   Path to the export file (JSON)\n    \
     --out <dir>      Optional: write report + memories + quarantine manifest here\n    \
     --json           Emit the report as JSON instead of Markdown\n\
     \n\
     SAFETY:\n    \
     Imported skills are QUARANTINED (never registered, never executed) and require\n    \
     sandbox validation + explicit operator approval before activation. Imported\n    \
     memories carry low-trust external provenance and are never admitted as trusted\n    \
     anchors."
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

    let mut args = args.into_iter().map(Into::into);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--from" => source = Some(ImportSource::parse(&take_value(&mut args, "--from")?)?),
            "--input" => input = Some(PathBuf::from(take_value(&mut args, "--input")?)),
            "--out" => out = Some(PathBuf::from(take_value(&mut args, "--out")?)),
            "--json" => json = true,
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
    let plan = ImportPlan::from_bundle(&bundle);
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
    }

    #[test]
    fn parse_defaults_out_none_and_markdown() {
        let cmd = parse(["--from", "hermes", "--input", "x.json"]).expect("parse");
        assert_eq!(cmd.source, ImportSource::Hermes);
        assert_eq!(cmd.out, None);
        assert!(!cmd.json);
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
    fn usage_mentions_sources_and_safety() {
        let text = usage();
        assert!(text.contains("openclaw"));
        assert!(text.contains("hermes"));
        assert!(text.contains("QUARANTINED"));
    }

    #[test]
    fn bundle_serde_roundtrip() {
        let bundle = from_openclaw(OPENCLAW_EXPORT).expect("parse");
        let json = serde_json::to_string(&bundle).expect("serialize");
        let back: ImportedBundle = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(bundle, back);
    }
}
