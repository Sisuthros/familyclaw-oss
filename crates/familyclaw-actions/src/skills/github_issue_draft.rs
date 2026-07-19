//! Genuine skill: producing a GitHub issue draft and (optionally) persisting
//! it as an allowlisted **draft artifact** to disk (Layer A).
//!
//! [`GithubIssueDraftMock`] converts a free-form bug report
//! ([`GithubIssueDraftInput::bug_report`]) into a structured issue draft
//! (title + body). This is the **genuine implementation** of the
//! `github_issue_draft` provider skill, which **makes no network call and
//! requires no credentials**:
//!
//! - **Without `out_path`** the skill produces a deterministic draft (proposal
//!   only) — the same contract as before, backward-compatible.
//! - **With `out_path`** the skill **actually writes** the draft artifact
//!   (Markdown) to disk, but only **under an allowlisted root**, mirroring
//!   the canonicalization and allowlist pattern of
//!   [`super::file_write::FileWriteAllowlisted`]. This produces a genuine,
//!   redactable side effect **without** an external API or credential
//!   (Layer A — generic placeholder repo).
//!
//! ## Risk class and approval
//! The risk is [`ActionRisk::WriteExternal`] and the policy is
//! [`ApprovalPolicy::RequireApproval`], so producing/persisting the draft
//! **always** pauses for human approval before execution — the pipeline
//! derives the requirement from the manifest, not the payload.
//!
//! ## The proof bundle contains no content
//! When the artifact is written, the result contains only the **hash**
//! (SHA-256) of the canonical path and the **count** of bytes written — NOT
//! the draft's body. An empty allowlist (default) rejects **all** write paths
//! (fail-closed); producing the draft without `out_path` still works.

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

use super::file_write::FileWriteConfig;
use super::Skill;

/// Generic target repo (Layer A — not a real repo).
pub const EXAMPLE_REPO: &str = "example-org/example-repo";

/// Fixed identifier for the skill, so registration and lookup are reproducible.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("11111111-1111-4111-8111-111111111111");

/// Skill input: free-form bug report and optional output path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIssueDraftInput {
    /// The user-written free-form bug report.
    pub bug_report: String,
    /// Optional allowlisted path to which the draft artifact is written.
    /// If `None`, the skill only produces the draft (no side effect).
    #[serde(default)]
    pub out_path: Option<String>,
}

/// Skill result: the issue draft (not published to any external system).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIssueDraftOutput {
    /// Proposed issue title.
    pub title: String,
    /// Proposed issue body (Markdown).
    pub body: String,
    /// Target repo the issue is proposed for (generic).
    pub repo: String,
}

/// Genuine skill: GitHub issue draft + optional allowlisted disk persistence.
///
/// The risk class is [`ActionRisk::WriteExternal`] and the policy is
/// [`ApprovalPolicy::RequireApproval`], so execution always requires approval.
#[derive(Debug, Clone, Default)]
pub struct GithubIssueDraftMock {
    /// Allowlist configuration for writing the artifact (empty = fail-closed).
    config: FileWriteConfig,
}

impl GithubIssueDraftMock {
    /// Creates the skill with an empty allowlist (artifact write is
    /// fail-closed; producing the draft without `out_path` still works).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the skill with the given allowlist configuration (allowed roots).
    #[must_use]
    pub fn with_config(config: FileWriteConfig) -> Self {
        Self { config }
    }

    /// The skill's fixed identifier.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Builds an issue draft from a bug report (pure logic).
    ///
    /// The title is derived from the report's first non-empty line
    /// (truncated), and the body contains the original report plus a
    /// generic repro-steps template.
    #[must_use]
    pub fn draft(input: &GithubIssueDraftInput) -> GithubIssueDraftOutput {
        let first_line = input
            .bug_report
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("Bug report");
        let title: String = first_line.chars().take(72).collect();

        let body = format!(
            "## Yhteenveto\n{}\n\n## Toistaminen\n1. (täydennä vaiheet)\n\n## Odotettu vs. todellinen\n- Odotettu: (täydennä)\n- Todellinen: (täydennä)\n\n_Luonnos — odottaa hyväksyntää ennen julkaisua repoon {EXAMPLE_REPO}._",
            input.bug_report.trim()
        );

        GithubIssueDraftOutput {
            title,
            body,
            repo: EXAMPLE_REPO.to_string(),
        }
    }

    /// Renders the draft as a persistable Markdown artifact.
    #[must_use]
    fn render_artifact(draft: &GithubIssueDraftOutput) -> String {
        format!(
            "# {}\n\n> repo: {}\n\n{}\n",
            draft.title, draft.repo, draft.body
        )
    }

    /// Resolves the allowlisted, canonicalized target path from the input path.
    ///
    /// # Errors
    /// [`ActionError::PolicyDenied`] if the allowlist is empty, the path
    /// cannot be resolved, or the canonical target is not under any allowed root.
    fn resolve_allowed(&self, requested: &str) -> Result<PathBuf> {
        let roots = self.config.canonical_allow_roots();
        if roots.is_empty() {
            return Err(ActionError::PolicyDenied(
                "ei sallittuja juuria — artefaktia ei kirjoiteta (fail-closed)".to_string(),
            ));
        }
        let canonical = canonicalize_target(Path::new(requested))?;
        if path_is_under_any(&canonical, &roots) {
            Ok(canonical)
        } else {
            Err(ActionError::PolicyDenied(
                "artefaktin kohde on allowlistin ulkopuolella (hylätty)".to_string(),
            ))
        }
    }
}

/// Is `path` under any of the given roots (or the root itself)?
fn path_is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Resolves the canonical form of a (possibly not yet existing) target path.
///
/// Canonicalizes the nearest existing ancestor (resolves `..`, follows
/// symlinks) and appends the remaining normal components. Rejects trailing
/// `..` segments.
///
/// # Errors
/// [`ActionError::PolicyDenied`] if the path is empty, ends in `..`, or if no
/// ancestor can be canonicalized.
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
                    ActionError::PolicyDenied("kohdepolku on tyhjä (hylätty)".to_string())
                })?;
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

/// Computes the SHA-256 hash of the canonical path as a hex string (instead
/// of the raw path, so a potentially private path does not leak into the proof).
fn hash_path(path: &Path) -> String {
    format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()))
}

#[async_trait]
impl ActionExecutor for GithubIssueDraftMock {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        // Parse the input; an invalid input produces a Failed result (not an error).
        let input: GithubIssueDraftInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid github_issue_draft input: {e}"),
                    request.now,
                ));
            }
        };

        let draft = Self::draft(&input);

        // No output path → draft only (backward-compatible).
        let Some(out_path) = input.out_path.as_deref() else {
            let output: Value = json!({
                "draft": { "title": draft.title, "body": draft.body, "repo": draft.repo },
                "published": false,
                "artifact_written": false,
            });
            return Ok(ActionResult::success(
                format!("drafted github issue '{}' for {EXAMPLE_REPO}", draft.title),
                output,
                request.now,
            ));
        };

        // Output path given → a genuine allowlisted disk write.
        let canonical = match self.resolve_allowed(out_path) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("path rejected: {e}"),
                    request.now,
                ));
            }
        };

        let artifact = Self::render_artifact(&draft);
        if let Some(parent) = canonical.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Ok(ActionResult::failure(
                    format!("artifact parent dir creation failed: {e}"),
                    request.now,
                ));
            }
        }
        if let Err(e) = tokio::fs::write(&canonical, artifact.as_bytes()).await {
            return Ok(ActionResult::failure(
                format!("artifact write failed: {e}"),
                request.now,
            ));
        }

        // Proof: only the path hash + byte count — NOT the draft body.
        let output: Value = json!({
            "draft": { "title": draft.title, "repo": draft.repo },
            "published": false,
            "artifact_written": true,
            "path_hash": hash_path(&canonical),
            "bytes_written": artifact.len() as u64,
        });

        Ok(ActionResult::success(
            format!(
                "drafted github issue '{}' and wrote artifact ({} bytes) to allowlisted path",
                draft.title,
                artifact.len()
            ),
            output,
            request.now,
        ))
    }
}

impl Skill for GithubIssueDraftMock {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "github_issue_draft".to_string(),
            version: "2.0.0".to_string(),
            description: "Luonnostelee GitHub-issuen bugiraportista ja voi tallentaa luonnoksen \
                 allowlistattuun artefaktiin levylle (ei verkkokutsua, ei tunnuksia; \
                 todiste = polkutiiviste + tavumäärä, ei runkoa)."
                .to_string(),
            permissions: vec![SkillPermission::WriteExternal],
            risk: ActionRisk::WriteExternal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("{ bug_report: string, out_path?: string }".to_string()),
            output_hint: Some(
                "{ draft, published, artifact_written, path_hash?, bytes_written? }".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "bug_report": {
                        "type": "string",
                        "description": "Käyttäjän kirjoittama vapaamuotoinen bugiraportti."
                    },
                    "out_path": {
                        "type": "string",
                        "description": "Valinnainen allowlistattu polku johon luonnos-artefakti kirjoitetaan."
                    }
                },
                "required": ["bug_report"],
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

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "familyclaw_issue_draft_{tag}_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::canonicalize(&dir).expect("canonicalize temp dir")
    }

    fn make_request(payload: Value) -> ActionRequest {
        ActionRequest::new(
            ActionId::new(),
            GithubIssueDraftMock::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        )
    }

    #[test]
    fn manifest_is_write_external_and_requires_approval() {
        let m = GithubIssueDraftMock::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "github_issue_draft");
        assert_eq!(m.risk, ActionRisk::WriteExternal);
        assert!(m.permissions.contains(&SkillPermission::WriteExternal));
        assert!(m.approval_policy.can_require_approval());
        assert_eq!(m.input_schema["type"], "object");
        assert_eq!(m.input_schema["required"][0], "bug_report");
    }

    #[test]
    fn draft_builds_title_and_body() {
        let input = GithubIssueDraftInput {
            bug_report: "Crash on startup\nIt explodes when config is empty".to_string(),
            out_path: None,
        };
        let out = GithubIssueDraftMock::draft(&input);
        assert_eq!(out.title, "Crash on startup");
        assert!(out.body.contains("It explodes"));
        assert_eq!(out.repo, EXAMPLE_REPO);
    }

    #[tokio::test]
    async fn happy_path_without_out_path_produces_unpublished_draft() {
        let skill = GithubIssueDraftMock::new();
        let res = skill
            .execute(make_request(
                json!({ "bug_report": "Login button does nothing" }),
            ))
            .await
            .expect("execute");
        assert!(res.status.is_success());
        assert_eq!(res.raw_output_redacted["published"], json!(false));
        assert_eq!(res.raw_output_redacted["artifact_written"], json!(false));
        assert_eq!(
            res.raw_output_redacted["draft"]["repo"],
            json!(EXAMPLE_REPO)
        );
    }

    #[tokio::test]
    async fn writes_artifact_to_allowlisted_path_and_reads_back() {
        let dir = temp_dir("ok");
        let target = dir.join("issue.md");
        let skill = GithubIssueDraftMock::with_config(FileWriteConfig::new().allow_root(&dir));
        let res = skill
            .execute(make_request(json!({
                "bug_report": "Crash on startup",
                "out_path": target.to_string_lossy(),
            })))
            .await
            .expect("execute");
        assert!(
            res.status.is_success(),
            "allowlisted artifact write must succeed"
        );
        assert_eq!(res.raw_output_redacted["artifact_written"], json!(true));

        let written = std::fs::read_to_string(&target).expect("read back");
        assert!(
            written.contains("Crash on startup"),
            "artifact must land on disk"
        );
        assert!(written.contains(EXAMPLE_REPO));
    }

    #[tokio::test]
    async fn out_path_outside_allowlist_is_rejected() {
        let allowed = temp_dir("allowed");
        let other = temp_dir("other");
        let target = other.join("leak.md");
        let skill = GithubIssueDraftMock::with_config(FileWriteConfig::new().allow_root(&allowed));
        let res = skill
            .execute(make_request(json!({
                "bug_report": "x",
                "out_path": target.to_string_lossy(),
            })))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            "out_path outside allowlist must be rejected"
        );
        assert!(res.output_summary.contains("rejected"));
        assert!(!target.exists(), "rejected artifact must not touch disk");
    }

    #[tokio::test]
    async fn empty_allowlist_rejects_artifact_write() {
        let dir = temp_dir("empty");
        let target = dir.join("issue.md");
        // Empty allowlist → fail-closed for artifact write.
        let skill = GithubIssueDraftMock::new();
        let res = skill
            .execute(make_request(json!({
                "bug_report": "x",
                "out_path": target.to_string_lossy(),
            })))
            .await
            .expect("execute");
        assert!(
            !res.status.is_success(),
            "empty allowlist must reject artifact write"
        );
        assert!(!target.exists(), "fail-closed artifact must not touch disk");
    }

    #[tokio::test]
    async fn rejects_dot_dot_traversal() {
        let base = temp_dir("traversal");
        let allowed = base.join("inside");
        std::fs::create_dir_all(&allowed).expect("create inside");
        let skill = GithubIssueDraftMock::with_config(FileWriteConfig::new().allow_root(&allowed));
        let traversal = allowed.join("..").join("outside.md");
        let res = skill
            .execute(make_request(json!({
                "bug_report": "x",
                "out_path": traversal.to_string_lossy(),
            })))
            .await
            .expect("execute");
        assert!(!res.status.is_success(), ".. traversal must be rejected");
        assert!(
            !base.join("outside.md").exists(),
            "traversal must not touch disk"
        );
    }

    #[tokio::test]
    async fn proof_records_hash_and_bytes_not_body() {
        let dir = temp_dir("proof");
        let target = dir.join("issue.md");
        let skill = GithubIssueDraftMock::with_config(FileWriteConfig::new().allow_root(&dir));
        // The title is derived from the FIRST line (it is allowed to appear);
        // the sensitive body content is on a different line and must not
        // leak into the proof.
        let res = skill
            .execute(make_request(json!({
                "bug_report": "Short title line\nmust-never-appear-in-proof-body",
                "out_path": target.to_string_lossy(),
            })))
            .await
            .expect("execute");
        assert!(res.status.is_success());

        let hash = res.raw_output_redacted["path_hash"]
            .as_str()
            .expect("path_hash present");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(res.raw_output_redacted["bytes_written"].as_u64().is_some());

        // When the artifact is written, the proof output does not contain the body.
        let out = &res.raw_output_redacted;
        assert!(
            out["draft"].get("body").is_none(),
            "written-artifact proof must omit body"
        );
        let rendered = serde_json::to_string(out).expect("serialize");
        assert!(
            !rendered.contains("must-never-appear-in-proof-body"),
            "proof must not contain the report body when artifact is written"
        );
    }

    #[tokio::test]
    async fn invalid_input_fails_gracefully() {
        let skill = GithubIssueDraftMock::new();
        let res = skill
            .execute(make_request(json!({ "wrong": "shape" })))
            .await
            .expect("execute");
        assert!(!res.status.is_success());
        assert!(res
            .output_summary
            .contains("invalid github_issue_draft input"));
    }
}
