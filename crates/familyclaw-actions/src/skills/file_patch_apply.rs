//! Real skill: unified-diff APPLICATION to an allowlisted file (Layer A).
//!
//! [`FilePatchApply`] is the **real implementation** of the `file_patch`-provider
//! skill — it replaces the earlier deterministic proposal mock. The skill **actually
//! writes** the applied patch to disk, but only **under an allowlisted root**, and
//! mirrors the exact security model of the
//! [`super::file_write::FileWriteAllowlisted`] skill:
//!
//! ## Load-bearing security: canonicalization + allowlist
//! Before writing, the target is **canonicalized** ([`std::fs::canonicalize`], which
//! resolves `..` segments and follows symlinks to the real target) and it is
//! verified that it remains under some (canonicalized) allowlisted root.
//! Every other target — `..` escapes and symlink escapes — is **rejected**
//! before writing. An empty allowlist (default) rejects **all** paths
//! (fail-closed).
//!
//! ## Risk class and approval
//! The risk is [`ActionRisk::WriteLocal`] and the policy is
//! [`ApprovalPolicy::AlwaysRequireApproval`], so applying a patch **always**
//! stops for human approval before execution — the pipeline derives the
//! requirement from the manifest, not the payload, so a prompt injection
//! embedded in the payload cannot bypass the gate.
//!
//! ## The evidence package does not contain content
//! The result contains only a **hash** of the canonical path (SHA-256), the
//! applied flag, and the **count** of changed lines — NEVER the content of the
//! file or the patch. This way the evidence does not leak the written data.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::{ActionError, Result};
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::file_write::FileWriteConfig;
use super::Skill;

/// Fixed identifier of the skill (shared with the earlier `file_patch` mock, so
/// registration and lookup remain backward-compatible).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("44444444-4444-4444-8444-444444444444");

/// Input for the `file_patch_apply` skill: target file and unified diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchApplyInput {
    /// Path of the target file. Will be canonicalized and must remain under the
    /// allowlisted root.
    pub path: String,
    /// Unified diff (unified format).
    pub patch: String,
}

/// Result for the `file_patch_apply` skill: proof that the patch was applied.
///
/// **Does NOT** contain the content of the file or the patch — only the
/// load-bearing metadata (hash + applied flag + line count).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchApplyOutput {
    /// SHA-256 hash of the canonical target path (hex) — NOT the raw path.
    pub path_hash: String,
    /// `true` if the patch was applied to the file.
    pub applied: bool,
    /// Number of changed lines (|new − old|).
    pub lines_changed: u64,
}

/// Real skill: applies a unified diff to an allowlisted file (disk write).
///
/// The risk class is [`ActionRisk::WriteLocal`] and the policy is
/// [`ApprovalPolicy::AlwaysRequireApproval`]: applying always stops for
/// approval; a target outside the allowlist is rejected.
#[derive(Debug, Clone, Default)]
pub struct FilePatchApply {
    /// Allowlist configuration (allowed roots) — shared with the `file_write` model.
    config: FileWriteConfig,
}

impl FilePatchApply {
    /// Creates the skill with an empty allowlist (rejects all paths, fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the skill with the given write configuration (allowed roots).
    #[must_use]
    pub fn with_config(config: FileWriteConfig) -> Self {
        Self { config }
    }

    /// Fixed identifier of the skill.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Resolves the allowlisted, canonicalized target path from the input path.
    ///
    /// # Errors
    /// [`ActionError::PolicyDenied`] if the allowlist is empty, the path cannot
    /// be resolved, or the canonical target is not under any allowed root.
    fn resolve_allowed(&self, requested: &str) -> Result<PathBuf> {
        let roots = self.config.canonical_allow_roots();
        if roots.is_empty() {
            return Err(ActionError::PolicyDenied(
                "no allowed roots — patch rejected (fail-closed)".to_string(),
            ));
        }
        let canonical = canonicalize_target(Path::new(requested))?;
        if path_is_under_any(&canonical, &roots) {
            Ok(canonical)
        } else {
            Err(ActionError::PolicyDenied(
                "patch target is outside the allowlist (rejected)".to_string(),
            ))
        }
    }

    /// Applies a simple unified diff to a single file (pure logic).
    ///
    /// Handles `@@` hunk headers and, within them, `+` (add), `-` (remove),
    /// and ` ` (context) lines. Does not need exact line numbering — at the
    /// Layer A level, deterministic, content-based application is sufficient.
    #[must_use]
    pub fn apply_patch(original: &str, patch: &str) -> String {
        let mut lines: Vec<String> = original.lines().map(ToString::to_string).collect();
        let patch_lines: Vec<&str> = patch.lines().collect();
        let mut i = 0;
        while i < patch_lines.len() {
            let line = patch_lines[i];
            if line.starts_with("@@") {
                i += 1;
                while i < patch_lines.len() {
                    let pl = patch_lines[i];
                    if pl.starts_with("@@") {
                        break;
                    }
                    if let Some(rest) = pl.strip_prefix('+') {
                        lines.push(rest.to_string());
                    } else if let Some(rest) = pl.strip_prefix('-') {
                        if let Some(pos) = lines.iter().position(|l| l == rest) {
                            lines.remove(pos);
                        }
                    } else if let Some(rest) = pl.strip_prefix(' ') {
                        if !lines.contains(&rest.to_string()) {
                            lines.push(rest.to_string());
                        }
                    }
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        lines.join("\n")
    }
}

/// Whether `path` is under some given root (or is the root itself).
fn path_is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Resolves the canonical form of the (possibly not-yet-existing) target path.
///
/// Canonicalizes the nearest existing ancestor (resolves `..` and follows
/// symlinks) and appends the remaining normal components to it. Rejects
/// `..` segments in the remaining tail (they could escape the root without
/// going through canonicalization).
///
/// # Errors
/// [`ActionError::PolicyDenied`] if the path is empty, ends in `..`, or if
/// no ancestor can be canonicalized.
fn canonicalize_target(requested: &Path) -> Result<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(requested) {
        return Ok(canonical);
    }

    let mut existing = requested;
    let mut tail: Vec<Component<'_>> = Vec::new();
    loop {
        match existing.parent() {
            Some(parent) => {
                let file = existing.components().next_back().ok_or_else(|| {
                    ActionError::PolicyDenied("target path is empty (rejected)".to_string())
                })?;
                if matches!(file, Component::ParentDir) {
                    return Err(ActionError::PolicyDenied(
                        "'..' at the end of the target path is not allowed (rejected)".to_string(),
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
                    "cannot canonicalize an ancestor of the target path (rejected)".to_string(),
                ));
            }
        }
    }
}

/// Computes the SHA-256 hash of the canonical path as a hex string (instead of
/// the raw path, so that a potentially private path does not leak into the evidence).
fn hash_path(path: &Path) -> String {
    format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()))
}

#[async_trait]
impl ActionExecutor for FilePatchApply {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: FilePatchApplyInput = match serde_json::from_value(request.payload.clone()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid file_patch_apply input: {e}"),
                    request.now,
                ));
            }
        };

        // Resolve + validate the allowlist. A rejected target → a failed result
        // (not an error that would crash the pipeline, same pattern as file_write).
        let canonical = match self.resolve_allowed(&input.path) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("path rejected: {e}"),
                    request.now,
                ));
            }
        };

        // Read the current content (missing file → empty base).
        let original = match tokio::fs::read_to_string(&canonical).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("read of allowlisted file failed: {e}"),
                    request.now,
                ));
            }
        };

        let patched = Self::apply_patch(&original, &input.patch);
        let lines_changed = patched.lines().count().abs_diff(original.lines().count()) as u64;

        if let Err(e) = tokio::fs::write(&canonical, &patched).await {
            return Ok(ActionResult::failure(
                format!("patch write failed: {e}"),
                request.now,
            ));
        }

        let output = json!({
            "path_hash": hash_path(&canonical),
            "applied": true,
            "lines_changed": lines_changed,
        });

        // The result remains untrusted by default (no .trusted()).
        Ok(ActionResult::success(
            format!("applied unified patch to allowlisted file ({lines_changed} line(s) changed)"),
            output,
            request.now,
        ))
    }
}

impl Skill for FilePatchApply {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "file_patch_apply".to_string(),
            version: "2.0.0".to_string(),
            description: "Applies a unified diff to an allowlisted file (canonicalized target); \
                 proof = path hash + applied flag + number of changed lines, no content."
                .to_string(),
            permissions: vec![SkillPermission::WriteLocalFiles],
            risk: ActionRisk::WriteLocal,
            approval_policy: ApprovalPolicy::AlwaysRequireApproval,
            input_hint: Some("{ path, patch }".to_string()),
            output_hint: Some("{ path_hash, applied, lines_changed }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path of the target file (will be canonicalized; must remain under the allowlisted root)."
                    },
                    "patch": {
                        "type": "string",
                        "description": "The unified diff to apply (unified format)."
                    }
                },
                "required": ["path", "patch"],
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
    use crate::policy::{required_approval, ApprovalRequirement};
    use familyclaw_core::time::{from_unix_secs, Timestamp};

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Creates an isolated temp directory (canonicalized, so that macOS
    /// `/var`→`/private/var` symlinks do not mess up the `starts_with` comparison).
    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "familyclaw_file_patch_{tag}_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::canonicalize(&dir).expect("canonicalize temp dir")
    }

    fn make_request(payload: serde_json::Value) -> ActionRequest {
        ActionRequest::new(
            ActionId::new(),
            FilePatchApply::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        )
    }

    #[test]
    fn manifest_is_write_local_always_require_approval_and_generic() {
        let m = FilePatchApply::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "file_patch_apply");
        assert_eq!(m.risk, ActionRisk::WriteLocal);
        assert_eq!(m.approval_policy, ApprovalPolicy::AlwaysRequireApproval);
        assert_eq!(m.permissions, vec![SkillPermission::WriteLocalFiles]);
        // AlwaysRequireApproval → the policy enforces approval even for a local
        // write.
        assert_eq!(
            required_approval(m.risk, m.approval_policy),
            ApprovalRequirement::RequireApproval
        );
        // Generic: no private paths in the manifest.
        let rendered = serde_json::to_string(&m).expect("serialize manifest");
        assert!(!rendered.contains(":\\"), "no Windows absolute paths");
        assert!(!rendered.contains("/home/"), "no private home paths");
    }

    #[test]
    fn apply_patch_adds_line() {
        let original = "fn main() {}";
        let patch = "--- a/file\n+++ b/file\n@@ -1,1 +1,2 @@\n fn main() {}\n+// logging\n";
        let out = FilePatchApply::apply_patch(original, patch);
        assert!(out.contains("// logging"));
    }

    #[tokio::test]
    async fn applies_patch_to_allowlisted_file_and_reads_back() {
        let dir = temp_dir("ok");
        let file = dir.join("test.txt");
        std::fs::write(&file, "hello\n").expect("seed file");

        let skill = FilePatchApply::with_config(FileWriteConfig::new().allow_root(&dir));
        let res = skill
            .execute(make_request(json!({
                "path": file.to_string_lossy(),
                "patch": "--- a\n+++ b\n@@ -1 +1,2 @@\n hello\n+world\n"
            })))
            .await
            .expect("execute");
        assert!(res.status.is_success(), "allowlisted patch must succeed");
        assert_eq!(res.raw_output_redacted["applied"], json!(true));

        let content = std::fs::read_to_string(&file).expect("read back");
        assert!(content.contains("world"), "patch must land on disk");
    }

    #[tokio::test]
    async fn rejects_outside_allowlist() {
        let allowed = temp_dir("allowed");
        let other = temp_dir("other");
        let target = other.join("secret.txt");
        std::fs::write(&target, "seed\n").expect("seed");

        let skill = FilePatchApply::with_config(FileWriteConfig::new().allow_root(&allowed));
        let res = skill
            .execute(make_request(json!({
                "path": target.to_string_lossy(),
                "patch": "@@ -1 +1,2 @@\n seed\n+leak\n"
            })))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            "outside allowlist must be rejected"
        );
        assert!(res.output_summary.contains("rejected"));
        // Absence of side effect: the file was not modified.
        assert_eq!(
            std::fs::read_to_string(&target).expect("read"),
            "seed\n",
            "rejected patch must not touch disk"
        );
    }

    #[tokio::test]
    async fn empty_allowlist_rejects_everything() {
        let dir = temp_dir("empty");
        let file = dir.join("doc.txt");
        std::fs::write(&file, "x\n").expect("seed");

        // Empty allowlist → fail-closed.
        let skill = FilePatchApply::new();
        let res = skill
            .execute(make_request(json!({
                "path": file.to_string_lossy(),
                "patch": "@@ -1 +1,2 @@\n x\n+y\n"
            })))
            .await
            .expect("execute");
        assert!(!res.status.is_success(), "empty allowlist must reject all");
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            "x\n",
            "fail-closed patch must not touch disk"
        );
    }

    #[tokio::test]
    async fn rejects_dot_dot_traversal() {
        let base = temp_dir("traversal");
        let allowed = base.join("inside");
        std::fs::create_dir_all(&allowed).expect("create inside");
        std::fs::write(base.join("outside.txt"), "orig\n").expect("seed outside");

        let skill = FilePatchApply::with_config(FileWriteConfig::new().allow_root(&allowed));
        // <allowed>/../outside.txt → canonicalizes to <base>/outside.txt (outside
        // the allowlist) → rejected.
        let traversal = allowed.join("..").join("outside.txt");
        let res = skill
            .execute(make_request(json!({
                "path": traversal.to_string_lossy(),
                "patch": "@@ -1 +1,2 @@\n orig\n+escape\n"
            })))
            .await
            .expect("execute");
        assert!(!res.status.is_success(), ".. traversal must be rejected");
        assert_eq!(
            std::fs::read_to_string(base.join("outside.txt")).expect("read"),
            "orig\n",
            "traversal patch must not touch disk outside allowlist"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escape() {
        // A symlink INSIDE the allowlist that points OUTSIDE — canonicalization
        // follows the link and reveals the real target as being outside.
        let allowed = temp_dir("symlink_allowed");
        let outside = temp_dir("symlink_outside");
        std::fs::write(outside.join("leak.txt"), "orig\n").expect("seed");

        let link_dir = allowed.join("link_dir");
        std::os::unix::fs::symlink(&outside, &link_dir).expect("create symlink");

        let skill = FilePatchApply::with_config(FileWriteConfig::new().allow_root(&allowed));
        let target = link_dir.join("leak.txt");
        let res = skill
            .execute(make_request(json!({
                "path": target.to_string_lossy(),
                "patch": "@@ -1 +1,2 @@\n orig\n+leaked\n"
            })))
            .await
            .expect("execute");
        assert!(!res.status.is_success(), "symlink escape must be rejected");
        assert_eq!(
            std::fs::read_to_string(outside.join("leak.txt")).expect("read"),
            "orig\n",
            "symlink-escape patch must not touch disk outside allowlist"
        );
    }

    #[tokio::test]
    async fn proof_records_hash_and_count_not_content() {
        let dir = temp_dir("proof");
        let file = dir.join("secret.txt");
        std::fs::write(&file, "seed\n").expect("seed");

        let skill = FilePatchApply::with_config(FileWriteConfig::new().allow_root(&dir));
        let res = skill
            .execute(make_request(json!({
                "path": file.to_string_lossy(),
                "patch": "@@ -1 +1,2 @@\n seed\n+must-never-appear-in-proof\n"
            })))
            .await
            .expect("execute");
        assert!(res.status.is_success());

        // Hash (64 hex characters) present, raw path not.
        let hash = res.raw_output_redacted["path_hash"]
            .as_str()
            .expect("path_hash present");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // The content of the patch/file must NOT appear in the evidence.
        let rendered = serde_json::to_string(&res.raw_output_redacted).expect("serialize");
        assert!(
            !rendered.contains("must-never-appear-in-proof"),
            "proof must not contain patched content body"
        );
        assert!(!rendered.contains("secret.txt"), "proof must not leak path");
    }

    #[tokio::test]
    async fn invalid_payload_fails_gracefully() {
        let dir = temp_dir("bad");
        let skill = FilePatchApply::with_config(FileWriteConfig::new().allow_root(&dir));
        let res = skill
            .execute(make_request(json!({ "path": "x.txt" })))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            "malformed payload must fail, not panic"
        );
        assert!(res
            .output_summary
            .contains("invalid file_patch_apply input"));
    }
}
