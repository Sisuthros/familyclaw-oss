//! Tiedoston unified-diff -soveltaminen allowlistatulle polulle (KERROS A).
//!
//! [`FilePatchApply`] soveltaa yhtenäisen diffin levylle samalla allowlist-
//! mallilla kuin [`super::file_write::FileWriteAllowlisted`].

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

const SKILL_UUID: uuid::Uuid = uuid::uuid!("44444444-4444-4444-8444-444444444444");

/// Syöte `file_patch_apply`-taidolle: kohdetiedosto ja unified-diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchApplyInput {
    /// Kohdetiedoston polku (allowlistattu).
    pub path: String,
    /// Yhtenäinen diff (unified format).
    pub patch: String,
}

/// Tulos `file_patch_apply`-taidolle: patchin sovelluksen todiste.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchApplyOutput {
    /// SHA-256-tiiviste kohdepolusta (ei paljasta polkua sellaisenaan).
    pub path_hash: String,
    /// `true` jos patch sovellettiin tiedostoon.
    pub applied: bool,
    /// Muutettujen rivien lukumäärä.
    pub lines_changed: u64,
}

/// Taito: soveltaa unified-diffin allowlistattuun tiedostoon.
#[derive(Debug, Clone)]
pub struct FilePatchApply {
    config: FileWriteConfig,
}

impl Default for FilePatchApply {
    fn default() -> Self {
        Self::new()
    }
}

impl FilePatchApply {
    /// Luo taidon oletuskonfiguraatiolla.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: FileWriteConfig::new(),
        }
    }

    /// Luo taidon annetulla kirjoituskonfiguraatiolla.
    #[must_use]
    pub fn with_config(config: FileWriteConfig) -> Self {
        Self { config }
    }

    /// Taidon kiinteä tunniste.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    fn resolve_allowed(&self, requested: &str) -> Result<PathBuf> {
        let roots = self.config.canonical_allow_roots();
        if roots.is_empty() {
            return Err(ActionError::PolicyDenied(
                "ei sallittuja juuria — patch hylätty (fail-closed)".to_string(),
            ));
        }
        let canonical = canonicalize_target(Path::new(requested))?;
        if path_is_under_any(&canonical, &roots) {
            Ok(canonical)
        } else {
            Err(ActionError::PolicyDenied(
                "patch-kohde on allowlistin ulkopuolella".to_string(),
            ))
        }
    }

    /// Soveltaa yksinkertaisen unified-diffin yhteen tiedostoon.
    pub fn apply_patch(original: &str, patch: &str) -> std::result::Result<String, String> {
        let mut lines: Vec<String> = original.lines().map(ToString::to_string).collect();
        let mut i = 0;
        let patch_lines: Vec<&str> = patch.lines().collect();
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
        Ok(lines.join("\n"))
    }
}

fn path_is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn canonicalize_target(path: &Path) -> Result<PathBuf> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(ActionError::PolicyDenied(
            "polku sisältää '..' — hylätty".to_string(),
        ));
    }
    if path.exists() {
        return std::fs::canonicalize(path)
            .map_err(|e| ActionError::PolicyDenied(format!("kanonisointi epäonnistui: {e}")));
    }
    let mut cur = path.to_path_buf();
    while !cur.exists() {
        if !cur.pop() {
            return Err(ActionError::PolicyDenied(
                "kohdepolkua ei voi ratkaista".to_string(),
            ));
        }
    }
    let base = std::fs::canonicalize(&cur)
        .map_err(|e| ActionError::PolicyDenied(format!("kanonisointi epäonnistui: {e}")))?;
    let suffix: PathBuf = path
        .strip_prefix(&cur)
        .unwrap_or(path)
        .components()
        .collect();
    Ok(base.join(suffix))
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

        let path = match self.resolve_allowed(&input.path) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ActionResult::failure(e.to_string(), request.now));
            }
        };

        let original = std::fs::read_to_string(&path).unwrap_or_default();
        let patched = match Self::apply_patch(&original, &input.patch) {
            Ok(s) => s,
            Err(e) => return Ok(ActionResult::failure(e, request.now)),
        };

        std::fs::write(&path, &patched).map_err(|e| ActionError::Proof(e.to_string()))?;

        let path_hash = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
        let lines_changed = patched.lines().count().abs_diff(original.lines().count()) as u64;

        let output = FilePatchApplyOutput {
            path_hash,
            applied: true,
            lines_changed,
        };

        Ok(ActionResult::success(
            "applied unified patch to allowlisted file",
            json!(output),
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
            description: "Soveltaa unified-diffin allowlistatulle tiedostolle.".to_string(),
            permissions: vec![SkillPermission::WriteLocalFiles],
            risk: ActionRisk::WriteLocal,
            approval_policy: ApprovalPolicy::AlwaysRequireApproval,
            input_hint: Some("{ path, patch }".to_string()),
            output_hint: Some("{ path_hash, applied, lines_changed }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "patch": { "type": "string" }
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
    use std::fs;

    #[test]
    fn apply_patch_adds_line() {
        let original = "fn main() {}\n";
        let patch = "--- a/file\n+++ b/file\n@@ -1,1 +1,2 @@\n fn main() {}\n+// logging\n";
        let out = FilePatchApply::apply_patch(original, patch).expect("apply");
        assert!(out.contains("// logging"));
    }

    #[tokio::test]
    async fn apply_to_allowlisted_file() {
        let dir = std::env::temp_dir().join(format!("fc_patch_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("test.txt");
        fs::write(&file, "hello\n").expect("write");

        let cfg = FileWriteConfig::new().allow_root(&dir);
        let skill = FilePatchApply::with_config(cfg);
        let req = ActionRequest::new(
            ActionId::new(),
            FilePatchApply::skill_id(),
            ActionTaskId::new(),
            json!({
                "path": file.to_string_lossy(),
                "patch": "--- a\n+++ b\n@@ -1 +1,2 @@\n hello\n+world\n"
            }),
            familyclaw_core::time::now(),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        let content = fs::read_to_string(&file).expect("read");
        assert!(content.contains("world"));
        let _ = fs::remove_dir_all(&dir);
    }

    use crate::ids::{ActionId, ActionTaskId};
}
