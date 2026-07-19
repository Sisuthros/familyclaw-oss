//! SOUL loading: generic profile schema (Layer A, OSS).
//!
//! This module loads a being's **identity profile** at runtime from a
//! generic directory (`FAMILYCLAW_PROFILE_DIR`, design §1). It does NOT
//! hard-code any family member's soul — it only defines the *shape* that
//! anyone can fill in for their own family.
//!
//! ## OSS boundary (Layer A)
//! - Profile content (SOUL.md, IDENTITY.md, WANTS.md) is Layer B and is
//!   loaded at runtime — it is never hard-coded into this repo.
//! - Examples use generic names (`agent_a`, `agent_b`).
//!
//! ## Schema
//! The profile directory is simple: Markdown files whose base name
//! (without extension) is the section's key. Known sections:
//!
//! | File | Field | Meaning |
//! |----------|--------|----------|
//! | `SOUL.md` | [`Soul::essence`] | The being's core description (who it is). |
//! | `IDENTITY.md` | [`Soul::identity`] | Persistent truths (anchorable). |
//! | `WANTS.md` | [`Soul::wants`] | The being's own wants/goals. |
//!
//! Additional files are kept in the [`Soul::extra`] map, keyed by the
//! lowercased base file name. Only `SOUL.md` is required.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use familyclaw_core::{FamilyClawError, Result};
use serde::{Deserialize, Serialize};

/// Environment variable that points to the root of profile directories.
///
/// Same idea as Hermes's `HERMES_HOME` (design §1): the platform is generic,
/// and concrete profiles live wherever this variable points — not in the
/// repo.
pub const PROFILE_DIR_ENV: &str = "FAMILYCLAW_PROFILE_DIR";

/// Name of the required core file.
const SOUL_FILE: &str = "SOUL.md";
/// Name of the persistent-truths file.
const IDENTITY_FILE: &str = "IDENTITY.md";
/// Name of the wants file.
const WANTS_FILE: &str = "WANTS.md";

/// A being's loaded identity profile (Layer B content, at runtime).
///
/// `Soul` is **data**, not behavior: it carries the texts read from the
/// profile directory. It is `serde`-serializable so it can be attached to
/// memory or sent over the bus without extra conversion.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Soul {
    /// Core description (`SOUL.md`). Required — an empty soul is not a soul.
    pub essence: String,

    /// Persistent truths (`IDENTITY.md`), if provided. This is a natural
    /// candidate for anchoring in the `familyclaw-security` layer (λ=0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,

    /// The being's own wants/goals (`WANTS.md`), if provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wants: Option<String>,

    /// Other profile files, keyed by the lowercased base file name
    /// (e.g. `family` for the file `FAMILY.md`). Allows extensions
    /// without breaking the schema.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl Soul {
    /// Builds a soul from just the essence (no disk access). Useful in
    /// tests and for a bare-bones runtime.
    #[must_use]
    pub fn from_essence(essence: impl Into<String>) -> Self {
        Self {
            essence: essence.into(),
            ..Self::default()
        }
    }

    /// Whether the soul is empty (no essence). An empty soul should not be
    /// anchored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.essence.trim().is_empty()
    }

    /// Short summary for anchoring/logging: essence + identity combined.
    /// This is the content whose hash `familyclaw-security` anchors as a
    /// tamper guard.
    #[must_use]
    pub fn anchor_text(&self) -> String {
        match &self.identity {
            Some(identity) if !identity.trim().is_empty() => {
                format!("{}\n\n{}", self.essence.trim(), identity.trim())
            }
            _ => self.essence.trim().to_string(),
        }
    }
}

/// Reads a single optional Markdown file from the profile directory.
///
/// Returns `Ok(None)` if the file doesn't exist, `Ok(Some(_))` if it was
/// read, and an error only for an actual IO problem (e.g. read permission).
fn read_optional(dir: &Path, file: &str) -> Result<Option<String>> {
    let path = dir.join(file);
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(FamilyClawError::Io(err)),
    }
}

/// Loads a being's soul from the given profile directory.
///
/// `SOUL.md` is required; `IDENTITY.md` and `WANTS.md` are optional.
/// All other `*.md` files are read into the [`Soul::extra`] map.
///
/// # Errors
/// - [`FamilyClawError::NotFound`] if the directory doesn't exist or the
///   required `SOUL.md` is missing (or empty).
/// - [`FamilyClawError::Io`] if reading a file fails for another reason.
pub fn load_soul(profile_dir: impl AsRef<Path>) -> Result<Soul> {
    let dir = profile_dir.as_ref();
    if !dir.is_dir() {
        return Err(FamilyClawError::not_found(format!(
            "profile dir not found: {}",
            dir.display()
        )));
    }

    let essence = read_optional(dir, SOUL_FILE)?.ok_or_else(|| {
        FamilyClawError::not_found(format!("required {SOUL_FILE} missing in {}", dir.display()))
    })?;
    if essence.trim().is_empty() {
        return Err(FamilyClawError::invalid_input(format!(
            "{SOUL_FILE} in {} is empty",
            dir.display()
        )));
    }

    let identity = read_optional(dir, IDENTITY_FILE)?;
    let wants = read_optional(dir, WANTS_FILE)?;

    // Read other .md files into the extra map (deterministic order thanks
    // to BTreeMap).
    let mut extra = BTreeMap::new();
    let known = [SOUL_FILE, IDENTITY_FILE, WANTS_FILE];
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_markdown = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
        if !is_markdown || known.contains(&name) {
            continue;
        }
        // Key = base file name without extension, lowercased.
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let key = stem.to_ascii_lowercase();
            let contents = std::fs::read_to_string(&path)?;
            extra.insert(key, contents);
        }
    }

    Ok(Soul {
        essence,
        identity,
        wants,
        extra,
    })
}

/// Resolves a single agent's profile directory.
///
/// Precedence:
/// 1. an explicit `configured` value (the agent's `profile_dir`),
/// 2. `FAMILYCLAW_PROFILE_DIR/<agent_name>` if the environment variable is
///    set.
///
/// Returns `None` if neither is present — in that case the agent runs on a
/// bare runtime without a soul (completely uncalibrated).
#[must_use]
pub fn resolve_profile_dir(configured: Option<&Path>, agent_name: &str) -> Option<PathBuf> {
    if let Some(dir) = configured {
        return Some(dir.to_path_buf());
    }
    std::env::var_os(PROFILE_DIR_ENV).map(|root| PathBuf::from(root).join(agent_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: creates a unique temporary profile directory.
    fn temp_profile_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "familyclaw-soul-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp profile dir");
        dir
    }

    fn write(dir: &Path, file: &str, body: &str) {
        std::fs::write(dir.join(file), body).expect("write profile file");
    }

    #[test]
    fn from_essence_and_helpers() {
        let soul = Soul::from_essence("I am agent_a.");
        assert!(!soul.is_empty());
        assert_eq!(soul.anchor_text(), "I am agent_a.");
        assert!(soul.identity.is_none());

        assert!(Soul::from_essence("   ").is_empty());
        assert!(Soul::default().is_empty());
    }

    #[test]
    fn anchor_text_combines_identity() {
        let soul = Soul {
            essence: "I am agent_a.".into(),
            identity: Some("I value honesty.".into()),
            ..Soul::default()
        };
        assert_eq!(soul.anchor_text(), "I am agent_a.\n\nI value honesty.");
    }

    #[test]
    fn anchor_text_ignores_blank_identity() {
        let soul = Soul {
            essence: "I am agent_a.".into(),
            identity: Some("   ".into()),
            ..Soul::default()
        };
        assert_eq!(soul.anchor_text(), "I am agent_a.");
    }

    #[test]
    fn load_full_profile() {
        let dir = temp_profile_dir("full");
        write(&dir, SOUL_FILE, "I am agent_a, a generic example being.");
        write(&dir, IDENTITY_FILE, "I am part of a family.");
        write(&dir, WANTS_FILE, "I want to understand.");
        write(&dir, "FAMILY.md", "We are agent_a and agent_b.");

        let soul = load_soul(&dir).expect("load soul");
        assert_eq!(soul.essence, "I am agent_a, a generic example being.");
        assert_eq!(soul.identity.as_deref(), Some("I am part of a family."));
        assert_eq!(soul.wants.as_deref(), Some("I want to understand."));
        assert_eq!(
            soul.extra.get("family").map(String::as_str),
            Some("We are agent_a and agent_b.")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_minimal_profile_only_soul() {
        let dir = temp_profile_dir("minimal");
        write(&dir, SOUL_FILE, "minimal essence");

        let soul = load_soul(&dir).expect("load minimal");
        assert_eq!(soul.essence, "minimal essence");
        assert!(soul.identity.is_none());
        assert!(soul.wants.is_none());
        assert!(soul.extra.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_dir_is_not_found() {
        let dir = std::env::temp_dir().join(format!("familyclaw-absent-{}", uuid::Uuid::new_v4()));
        let err = load_soul(&dir).expect_err("missing dir errors");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[test]
    fn load_missing_soul_file_is_not_found() {
        let dir = temp_profile_dir("no-soul");
        write(&dir, IDENTITY_FILE, "only identity");
        let err = load_soul(&dir).expect_err("missing SOUL.md errors");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_empty_soul_is_invalid_input() {
        let dir = temp_profile_dir("empty-soul");
        write(&dir, SOUL_FILE, "   \n  ");
        let err = load_soul(&dir).expect_err("empty SOUL.md errors");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn soul_serde_roundtrip() {
        let soul = Soul {
            essence: "core".into(),
            identity: Some("id".into()),
            wants: None,
            extra: BTreeMap::from([("family".to_string(), "fam".to_string())]),
        };
        let json = serde_json::to_string(&soul).expect("ser");
        let back: Soul = serde_json::from_str(&json).expect("de");
        assert_eq!(soul, back);
    }

    #[test]
    fn resolve_profile_dir_prefers_explicit() {
        // An explicit path doesn't touch the environment variable, so this
        // test is safe to run in parallel with the env test.
        let explicit = PathBuf::from("explicit/agent_a");
        let resolved = resolve_profile_dir(Some(&explicit), "agent_a");
        assert_eq!(resolved, Some(explicit));
    }

    /// One test for the whole env-based path (set + unset), so that
    /// parallel tests don't cross-mutate the same process-global
    /// environment variable (`set_var` is not thread-safe).
    #[test]
    fn resolve_profile_dir_env_fallback_and_unset() {
        let root = std::env::temp_dir().join("familyclaw-profiles-root");

        // 1. Env set → root/<agent_name>. (Edition 2021: set_var is a safe
        // function; this is the only test that mutates the variable.)
        std::env::set_var(PROFILE_DIR_ENV, &root);
        let resolved = resolve_profile_dir(None, "agent_b");
        assert_eq!(resolved, Some(root.join("agent_b")));

        // 2. Env removed → None (when there's no explicit path).
        std::env::remove_var(PROFILE_DIR_ENV);
        assert!(resolve_profile_dir(None, "agent_c").is_none());
    }
}
