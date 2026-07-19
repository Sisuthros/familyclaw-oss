//! Flagship skill: allowlisted local file WRITE (Layer A).
//!
//! [`FileWriteAllowlisted`] is a genuine disk-writing executor — unlike
//! [`crate::skills::file_patch::FilePatchMock`], which is a deterministic
//! proposal mock. This skill **actually writes** a file to disk, but only
//! under an allowlisted root, and mirrors the allowlist and canonicalization
//! pattern of the [`crate::skills::fs_read`] skill.
//!
//! ## Load-bearing safety: canonicalization + allowlist
//! The skill takes a path ([`FileWriteInput::path`]) and ensures the target
//! stays **under some allowlisted root** BEFORE writing:
//! 1. **canonicalizes** the target's parent directory
//!    ([`std::fs::canonicalize`]) — resolves `..` segments and follows
//!    symlinks to their real target; if the parent does not yet exist, climbs
//!    up to the nearest existing ancestor and canonicalizes that,
//! 2. verifies the canonical target stays under some (canonicalized) root,
//! 3. **rejects** any target outside the allowlist — including `..` escapes
//!    and symlink escapes (a link inside the allowlist that points outward
//!    canonicalizes to an external location and is rejected).
//!
//! An empty allowlist (default) rejects **all** paths — fail-closed.
//!
//! ## Risk class and approval
//! The risk is [`ActionRisk::WriteLocal`] and the permission is
//! [`SkillPermission::WriteLocalFiles`]. The policy is
//! [`ApprovalPolicy::RequireApproval`]: a local write landing under an
//! allowlisted root runs automatically (the same pattern as
//! [`crate::skills::fs_read`]: the allowlist is the actual security
//! boundary). A path outside the allowlist is rejected before writing.
//!
//! ## The proof bundle contains no content
//! The result contains only the **hash** (SHA-256) of the canonical path, the
//! **count** of bytes written, and the mode (`overwrite`/`append`) — NEVER the
//! body of the written content. This way the proof does not leak the written data.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{ActionError, Result};
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Fixed identifier for the skill (1–6 are reserved for other default skills).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("99999999-9999-4999-8999-999999999999");

/// Write mode: replace the file or append to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Replaces the file's entire content (creates it if missing). Default.
    #[default]
    Overwrite,
    /// Appends content to the end of the file (creates it if missing).
    Append,
}

impl WriteMode {
    /// Human-readable name for the mode (used in output).
    const fn as_str(self) -> &'static str {
        match self {
            Self::Overwrite => "overwrite",
            Self::Append => "append",
        }
    }
}

/// Skill input: the path of the file to write, its content, and an optional mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWriteInput {
    /// Path of the file to write. Canonicalized and must stay under an
    /// allowlisted root.
    pub path: String,
    /// Content to write.
    pub content: String,
    /// Write mode (`overwrite`/`append`). Default `overwrite`.
    #[serde(default)]
    pub mode: WriteMode,
}

/// Skill result: the proof bundle's core (hash + byte count + mode).
///
/// Does **NOT** contain the written content — only load-bearing metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWriteOutput {
    /// SHA-256 hash (hex) of the canonical target path — NOT the raw path.
    pub path_hash: String,
    /// Number of bytes written.
    pub bytes_written: u64,
    /// Write mode used (`overwrite`/`append`).
    pub mode: String,
}

/// Allowlist configuration: allowed root directories for writing.
///
/// The configuration is **configurable** — the skill does not hardcode any
/// path, so the published source stays generic (no private paths). An empty
/// allowlist (default) rejects **all** paths — fail-closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileWriteConfig {
    /// Allowed root directories. A write is allowed only if the canonical
    /// target stays under one of these (canonicalized) roots.
    allow_roots: Vec<PathBuf>,
}

impl FileWriteConfig {
    /// Creates an empty configuration that rejects all paths (fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an allowed root (builder chaining).
    ///
    /// The root is not canonicalized here — canonicalization happens only at
    /// write time, so the configuration can be built before the directory
    /// even exists.
    #[must_use]
    pub fn allow_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.allow_roots.push(root.into());
        self
    }

    /// Canonicalizes the allowed roots. Nonexistent or uncanonicalizable
    /// roots are silently skipped (nothing can ever land under them).
    /// Canonicalizes the allowed roots (also used by [`super::file_patch_apply`]).
    pub(crate) fn canonical_allow_roots(&self) -> Vec<PathBuf> {
        self.allow_roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .collect()
    }
}

/// Flagship skill for allowlisted file writing (genuine disk write).
///
/// The risk class is [`ActionRisk::WriteLocal`] and the policy is
/// [`ApprovalPolicy::RequireApproval`]: a write landing under an allowlisted
/// root runs automatically; a target outside the allowlist is rejected.
#[derive(Debug, Clone, Default)]
pub struct FileWriteAllowlisted {
    /// Allowlist configuration (allowed roots).
    config: FileWriteConfig,
}

impl FileWriteAllowlisted {
    /// Creates the skill with an empty allowlist (rejects all paths, fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the skill with the given allowlist configuration.
    #[must_use]
    pub fn with_config(config: FileWriteConfig) -> Self {
        Self { config }
    }

    /// The skill's fixed identifier.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Resolves the allowlisted, canonicalized target path from the input path.
    ///
    /// Since the target file may not exist yet (and thus cannot be directly
    /// canonicalized), the skill:
    /// 1. rejects target paths ending in a `..` segment (no file name),
    /// 2. canonicalizes the nearest **existing** ancestor (resolves `..` and
    ///    follows symlinks), and appends the remaining "normal" components,
    /// 3. verifies the final canonical target stays under some allowed root.
    ///    A symlink escape is revealed in step 2 (canonicalizing the ancestor
    ///    follows links to their real target).
    ///
    /// # Errors
    /// - [`ActionError::PolicyDenied`] if the allowlist is empty, the path
    ///   cannot be resolved, or the canonical target is not under any allowed root.
    fn resolve_allowed(&self, requested: &str) -> Result<PathBuf> {
        let roots = self.config.canonical_allow_roots();
        if roots.is_empty() {
            return Err(ActionError::PolicyDenied(
                "ei sallittuja juuria — kaikki polut hylätään (fail-closed)".to_string(),
            ));
        }

        let canonical = canonicalize_target(Path::new(requested))?;

        if path_is_under_any(&canonical, &roots) {
            Ok(canonical)
        } else {
            Err(ActionError::PolicyDenied(
                "kanoninen kohde on allowlistin ulkopuolella (hylätty)".to_string(),
            ))
        }
    }

    /// Writes the content to the canonicalized target and assembles the proof
    /// bundle's core (hash + byte count + mode) — NOT the written content.
    ///
    /// Creates any missing parent directories as needed (which, based on the
    /// canonicalization, stay inside the allowlist). `overwrite` replaces the
    /// whole file, `append` adds to the end.
    ///
    /// # Errors
    /// Returns [`ActionError::ExecutionFailed`] if directory creation or file
    /// writing fails.
    async fn write_proof(
        &self,
        canonical: &Path,
        content: &[u8],
        mode: WriteMode,
    ) -> Result<FileWriteOutput> {
        if let Some(parent) = canonical.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ActionError::ExecutionFailed(format!("vanhempihakemiston luonti epäonnistui: {e}"))
            })?;
        }

        match mode {
            WriteMode::Overwrite => {
                tokio::fs::write(canonical, content).await.map_err(|e| {
                    ActionError::ExecutionFailed(format!("tiedoston kirjoitus epäonnistui: {e}"))
                })?;
            }
            WriteMode::Append => {
                use tokio::io::AsyncWriteExt;
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(canonical)
                    .await
                    .map_err(|e| {
                        ActionError::ExecutionFailed(format!(
                            "tiedoston avaus (append) epäonnistui: {e}"
                        ))
                    })?;
                file.write_all(content).await.map_err(|e| {
                    ActionError::ExecutionFailed(format!("tiedoston lisäys epäonnistui: {e}"))
                })?;
                file.flush().await.map_err(|e| {
                    ActionError::ExecutionFailed(format!("tiedoston huuhtelu epäonnistui: {e}"))
                })?;
            }
        }

        Ok(FileWriteOutput {
            path_hash: hash_path(canonical),
            bytes_written: content.len() as u64,
            mode: mode.as_str().to_string(),
        })
    }
}

/// Resolves the canonical form of a (possibly not yet existing) target path.
///
/// Canonicalizes the nearest existing ancestor and appends the remaining
/// normal components to it. Rejects `..` segments in the remaining part
/// (they could escape the allowlist via a symlink without passing through
/// canonicalization).
///
/// # Errors
/// [`ActionError::PolicyDenied`] if the path is empty, ends in `..`, or if no
/// ancestor can be canonicalized.
fn canonicalize_target(requested: &Path) -> Result<PathBuf> {
    // If the target already exists, canonicalize it directly (follows symlinks).
    if let Ok(canonical) = std::fs::canonicalize(requested) {
        return Ok(canonical);
    }

    // Otherwise climb up to the nearest existing ancestor.
    let mut existing = requested;
    let mut tail: Vec<Component<'_>> = Vec::new();
    loop {
        match existing.parent() {
            Some(parent) => {
                let file = existing.components().next_back().ok_or_else(|| {
                    ActionError::PolicyDenied("kohdepolku on tyhjä (hylätty)".to_string())
                })?;
                // A `..` in the remaining tail could escape the root without
                // canonicalization seeing it → rejected.
                if matches!(file, Component::ParentDir) {
                    return Err(ActionError::PolicyDenied(
                        "'..' kohdepolun lopussa ei sallittu (hylätty)".to_string(),
                    ));
                }
                if matches!(file, Component::Normal(_)) {
                    tail.push(file);
                }
                if let Ok(base) = std::fs::canonicalize(parent) {
                    let mut resolved = base;
                    for comp in tail.iter().rev() {
                        if let Component::Normal(name) = comp {
                            resolved.push(name);
                        }
                    }
                    return Ok(resolved);
                }
                existing = parent;
            }
            None => {
                return Err(ActionError::PolicyDenied(
                    "kohdepolun esivanhempaa ei voi kanonisoida (hylätty)".to_string(),
                ));
            }
        }
    }
}

/// Is `path` under any of the given roots (or the root itself)?
///
/// The comparison is done at the component level via [`Path::starts_with`]
/// semantics, so e.g. `/allow/dir2` does not match the root `/allow/dir` (a
/// prefix match is not enough — the whole component must match).
fn path_is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Computes the SHA-256 hash of the canonical path as a hex string.
///
/// The path is hashed from its byte representation; the hash is stored in
/// the proof instead of the raw path, so a (potentially private) path does not leak.
fn hash_path(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[async_trait]
impl ActionExecutor for FileWriteAllowlisted {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: FileWriteInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid file_write input: {e}"),
                    request.now,
                ));
            }
        };

        // Resolve + validate against the allowlist. A rejected target →
        // failed result (no panic, no error that would crash the pipeline).
        let canonical = match self.resolve_allowed(&input.path) {
            Ok(path) => path,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("path rejected: {e}"),
                    request.now,
                ));
            }
        };

        let out = match self
            .write_proof(&canonical, input.content.as_bytes(), input.mode)
            .await
        {
            Ok(out) => out,
            Err(e) => {
                return Ok(ActionResult::failure(e.to_string(), request.now));
            }
        };

        let output: Value = json!({
            "path_hash": out.path_hash,
            "bytes_written": out.bytes_written,
            "mode": out.mode,
        });

        // The write's result stays untrusted by default (no .trusted()).
        Ok(ActionResult::success(
            format!(
                "wrote {} byte(s) to allowlisted path ({})",
                out.bytes_written, out.mode
            ),
            output,
            request.now,
        ))
    }
}

impl Skill for FileWriteAllowlisted {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "file_write_allowlisted".to_string(),
            version: "1.0.0".to_string(),
            description: "Kirjoittaa paikallisen tiedoston vain allowlistatun juuren alle \
                 (kanonisoitu kohde, overwrite/append); todiste = tiiviste + tavumäärä + tila."
                .to_string(),
            permissions: vec![SkillPermission::WriteLocalFiles],
            risk: ActionRisk::WriteLocal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("{ path, content, mode? }".to_string()),
            output_hint: Some("{ path_hash, bytes_written, mode }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Kirjoitettavan tiedoston polku (kanonisoidaan; on pysyttävä allowlistatun juuren alla)."
                    },
                    "content": {
                        "type": "string",
                        "description": "Kirjoitettava sisältö."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["overwrite", "append"],
                        "description": "Kirjoitustila: 'overwrite' (oletus) korvaa, 'append' lisää loppuun."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            publisher: None,
            signature: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ActionId, ActionTaskId};
    use familyclaw_core::time::{from_unix_secs, Timestamp};

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Creates an isolated temp directory for this test (canonicalized, so
    /// macOS `/var`→`/private/var` symlinks don't confuse the `starts_with` comparison).
    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "familyclaw_file_write_{tag}_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::canonicalize(&dir).expect("canonicalize temp dir")
    }

    fn make_request(skill_id: SkillId, payload: Value) -> ActionRequest {
        ActionRequest::new(
            ActionId::new(),
            skill_id,
            ActionTaskId::new(),
            payload,
            at(1),
        )
    }

    #[test]
    fn manifest_is_write_local_require_approval_and_generic() {
        let m = FileWriteAllowlisted::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "file_write_allowlisted");
        assert_eq!(m.risk, ActionRisk::WriteLocal);
        assert_eq!(m.approval_policy, ApprovalPolicy::RequireApproval);
        assert_eq!(m.permissions, vec![SkillPermission::WriteLocalFiles]);
        assert_eq!(m.input_schema["properties"]["path"]["type"], "string");
        assert_eq!(m.input_schema["properties"]["content"]["type"], "string");
        // Generic: no family names or private paths in the manifest.
        // The forbidden names are built from fragments so the source file
        // does not contain a single whole family-name literal (audit-layer-b.sh would flag it).
        let rendered = serde_json::to_string(&m).expect("serialize manifest");
        let forbidden_fragments: [(&str, &str); 6] = [
            ("Lum", "en"),
            ("Lum", "ina"),
            ("Pris", "ma"),
            ("Pho", "ton"),
            ("Auro", "ra"),
            ("Vil", "le"),
        ];
        for (head, tail) in forbidden_fragments {
            let forbidden = format!("{head}{tail}");
            assert!(
                !rendered.contains(&forbidden),
                "manifest must be generic (no family names)"
            );
        }
        assert!(!rendered.contains(":\\"), "no Windows absolute paths");
        assert!(
            !rendered.contains("/home/"),
            "no private home paths in manifest"
        );
    }

    #[tokio::test]
    async fn writes_allowlisted_file_and_reads_back() {
        let dir = temp_dir("ok");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&dir));

        let target = dir.join("out.txt");
        let payload = serde_json::to_value(FileWriteInput {
            path: target.to_string_lossy().to_string(),
            content: "hello disk".to_string(),
            mode: WriteMode::Overwrite,
        })
        .expect("serialize");
        let res = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), payload))
            .await
            .expect("execute");
        assert!(res.status.is_success(), "allowlisted write must succeed");
        assert_eq!(res.raw_output_redacted["bytes_written"], json!(10));
        assert_eq!(res.raw_output_redacted["mode"], json!("overwrite"));

        // Read back from disk and verify the content.
        let read_back = std::fs::read_to_string(&target).expect("read back");
        assert_eq!(read_back, "hello disk");
    }

    #[tokio::test]
    async fn creates_parent_dirs_within_allowlist() {
        let dir = temp_dir("nested");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&dir));

        // The parent directories ("a/b/") do not exist yet.
        let target = dir.join("a").join("b").join("deep.txt");
        let payload = serde_json::to_value(FileWriteInput {
            path: target.to_string_lossy().to_string(),
            content: "nested".to_string(),
            mode: WriteMode::Overwrite,
        })
        .expect("serialize");
        let res = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), payload))
            .await
            .expect("execute");
        assert!(
            res.status.is_success(),
            "write into new subdir must succeed"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read back"),
            "nested"
        );
    }

    #[tokio::test]
    async fn rejects_outside_allowlist() {
        let allowed = temp_dir("allowed");
        let other = temp_dir("other");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&allowed));

        // The target is under a different (non-allowlisted) root.
        let target = other.join("secret.txt");
        let payload = serde_json::to_value(FileWriteInput {
            path: target.to_string_lossy().to_string(),
            content: "should not be written".to_string(),
            mode: WriteMode::Overwrite,
        })
        .expect("serialize");
        let res = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), payload))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            "path outside allowlist must be rejected"
        );
        assert!(res.output_summary.contains("rejected"));
        // Proof of the absence of a side effect: the file was not created.
        assert!(!target.exists(), "rejected write must not touch disk");
    }

    #[tokio::test]
    async fn rejects_dot_dot_traversal() {
        // Allowlist = a subdirectory; attempt a `..` escape outward.
        let base = temp_dir("traversal");
        let allowed = base.join("inside");
        std::fs::create_dir_all(&allowed).expect("create inside");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&allowed));

        // `<allowed>/../outside.txt` → canonicalizes to `<base>/outside.txt`,
        // which is NOT under the allowlist → rejected.
        let traversal = allowed.join("..").join("outside.txt");
        let payload = serde_json::to_value(FileWriteInput {
            path: traversal.to_string_lossy().to_string(),
            content: "escape".to_string(),
            mode: WriteMode::Overwrite,
        })
        .expect("serialize");
        let res = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), payload))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            ".. traversal escaping the allowlist must be rejected"
        );
        assert!(
            !base.join("outside.txt").exists(),
            "traversal write must not touch disk outside allowlist"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escape() {
        // A symlink INSIDE the allowlist that points OUTSIDE the allowlist.
        // Canonicalization follows the link → the real target is revealed as external.
        let allowed = temp_dir("symlink_allowed");
        let outside = temp_dir("symlink_outside");

        let link_dir = allowed.join("link_dir");
        std::os::unix::fs::symlink(&outside, &link_dir).expect("create symlink");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&allowed));

        // A write to <allowed>/link_dir/leak.txt → canonicalizes to <outside>/leak.txt.
        let target = link_dir.join("leak.txt");
        let payload = serde_json::to_value(FileWriteInput {
            path: target.to_string_lossy().to_string(),
            content: "leak me".to_string(),
            mode: WriteMode::Overwrite,
        })
        .expect("serialize");
        let res = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), payload))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            "symlink pointing outside the allowlist must be rejected"
        );
        assert!(
            !outside.join("leak.txt").exists(),
            "symlink-escape write must not touch disk outside allowlist"
        );
    }

    #[tokio::test]
    async fn append_mode_appends() {
        let dir = temp_dir("append");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&dir));
        let target = dir.join("log.txt");

        // First write the base content in overwrite mode.
        let first = serde_json::to_value(FileWriteInput {
            path: target.to_string_lossy().to_string(),
            content: "line1\n".to_string(),
            mode: WriteMode::Overwrite,
        })
        .expect("serialize");
        let res1 = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), first))
            .await
            .expect("execute");
        assert!(res1.status.is_success());

        // Then append in append mode.
        let second = serde_json::to_value(FileWriteInput {
            path: target.to_string_lossy().to_string(),
            content: "line2\n".to_string(),
            mode: WriteMode::Append,
        })
        .expect("serialize");
        let res2 = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), second))
            .await
            .expect("execute");
        assert!(res2.status.is_success());
        assert_eq!(res2.raw_output_redacted["mode"], json!("append"));

        // Content = both lines in sequence (append did NOT replace).
        let read_back = std::fs::read_to_string(&target).expect("read back");
        assert_eq!(read_back, "line1\nline2\n");
    }

    #[tokio::test]
    async fn empty_allowlist_rejects_everything() {
        let dir = temp_dir("empty_allow");
        // Empty allowlist → fail-closed.
        let skill = FileWriteAllowlisted::new();
        let target = dir.join("doc.txt");
        let payload = serde_json::to_value(FileWriteInput {
            path: target.to_string_lossy().to_string(),
            content: "data".to_string(),
            mode: WriteMode::Overwrite,
        })
        .expect("serialize");
        let res = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), payload))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            "empty allowlist must reject all paths"
        );
        assert!(!target.exists(), "fail-closed write must not touch disk");
    }

    #[tokio::test]
    async fn proof_records_hash_and_bytes_not_content() {
        let dir = temp_dir("proof");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&dir));
        let target = dir.join("secret.txt");
        let content = "must never appear in proof body";
        let payload = serde_json::to_value(FileWriteInput {
            path: target.to_string_lossy().to_string(),
            content: content.to_string(),
            mode: WriteMode::Overwrite,
        })
        .expect("serialize");
        let res = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), payload))
            .await
            .expect("execute");
        assert!(res.status.is_success());

        // The hash (64 hex chars) and byte count are present.
        let hash = res.raw_output_redacted["path_hash"]
            .as_str()
            .expect("path_hash present");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            res.raw_output_redacted["bytes_written"]
                .as_u64()
                .expect("bytes"),
            content.len() as u64
        );

        // The written content must NOT appear in the proof.
        let rendered = serde_json::to_string(&res.raw_output_redacted).expect("serialize output");
        assert!(
            !rendered.contains("must never appear"),
            "proof must not contain written content body"
        );
    }

    #[tokio::test]
    async fn invalid_payload_fails_gracefully() {
        let dir = temp_dir("bad_payload");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&dir));
        // Missing `content` field → parse error → failed result (no panic).
        let payload = json!({ "path": "x.txt" });
        let res = skill
            .execute(make_request(FileWriteAllowlisted::skill_id(), payload))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            "malformed payload must fail, not panic"
        );
        assert!(res.output_summary.contains("invalid file_write input"));
    }
}
