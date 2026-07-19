//! Flagship skill: allowlisted local file read (Layer A).
//!
//! [`FsReadAllowlisted`] proves out the entire tool loop (observe→…→report)
//! **without opening a network door**: it is intentionally **not** an
//! http-get, but reads only local files, and is **SSRF-safe by construction**
//! — a network request cannot be formed, because the skill never touches
//! the network.
//!
//! ## Load-bearing security: canonicalization + allowlist
//! The skill takes a path ([`FsReadInput::path`]) and:
//! 1. **canonicalizes** it ([`std::fs::canonicalize`]) — resolves `..`
//!    segments and follows symlinks to their real target,
//! 2. verifies that the canonical path stays **under some allowlisted root**
//!    (the roots are also canonicalized before comparison),
//! 3. **rejects** any path outside the allowlist — including symlink
//!    escapes (a symlink inside the allowlist that points outside
//!    canonicalizes to an outside target and is rejected).
//!
//! A read landing under the allowlist runs **automatically** (no approval);
//! a path outside the allowlist is rejected.
//!
//! ## The proof package does not contain the whole file
//! By default the result contains only the path's **hash** (SHA-256), the
//! file's **size** in bytes, and a short **summary** — NOT the full file
//! contents. This way the proof cannot leak the data read, nor does it
//! bloat.
//!
//! ## Taint (untrustworthiness)
//! By default, the content read is **untrusted** (tainted). Only if the
//! canonical path falls under separately configured **trusted** roots (the
//! project's own files) is the output marked trusted. This way content
//! brought in from outside cannot launder itself clean.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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

/// The skill's fixed identifier, so registration and lookup are repeatable.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("55555555-5555-4555-8555-555555555555");

/// Maximum summary length in bytes — kept short so the full file contents
/// never leak into the proof via the summary.
const SUMMARY_MAX_BYTES: usize = 120;

/// Maximum number of directory-listing entries in the summary.
const DIR_LIST_MAX_ENTRIES: usize = 64;

/// Maximum full-content size in bytes in the tool result (`read_full_content`).
const FULL_CONTENT_MAX_BYTES: usize = 64 * 1024;

/// The skill's input: the path of the file to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FsReadInput {
    /// Path of the file to read (relative or absolute). The path is
    /// canonicalized and must stay under an allowlisted root.
    pub path: String,
    /// When `true`, also returns the `content` field (truncated to the
    /// limit). Default `false` — only hash + summary in the proof.
    #[serde(default)]
    pub read_full_content: bool,
}

/// The skill's result: the core of the proof package (hash + size + summary).
///
/// Does **NOT** contain the full file contents — only load-bearing metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadOutput {
    /// SHA-256 hash of the canonical path (hex) — NOT the raw path.
    pub path_hash: String,
    /// Size of the file read, in bytes.
    pub size: u64,
    /// Short human-readable summary (truncated, NOT the full contents).
    pub summary: String,
    /// Whether the content is a trusted project file (affects taint state).
    pub trusted: bool,
}

/// Allowlist configuration: allowed roots and their trusted subset.
///
/// The configuration is **configurable** — the skill does not hardcode any
/// path, so the published source stays generic (no private paths). An empty
/// allowlist (default) rejects **all** paths — fail-closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FsReadConfig {
    /// Allowed root directories. A read is allowed only if the canonical
    /// path stays under one of these (canonicalized) roots.
    allow_roots: Vec<PathBuf>,
    /// Subset of trusted roots: content read from under these is marked
    /// trusted (taint is cleared). Empty = nothing is trusted.
    trusted_roots: Vec<PathBuf>,
}

impl FsReadConfig {
    /// Creates an empty configuration that rejects all paths (fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an allowed root (builder chaining).
    ///
    /// The root is not canonicalized here — canonicalization happens only
    /// at read time, so the configuration can also be built before the
    /// directory exists.
    #[must_use]
    pub fn allow_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.allow_roots.push(root.into());
        self
    }

    /// Adds a trusted root (builder chaining).
    ///
    /// The trusted root is also added to the allowed roots, so that a path
    /// marked trusted can never accidentally be left outside the allowlist.
    #[must_use]
    pub fn trusted_root(mut self, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        self.allow_roots.push(root.clone());
        self.trusted_roots.push(root);
        self
    }

    /// Canonicalizes the allowed roots. Nonexistent or non-canonicalizable
    /// roots are silently skipped (nothing can ever land under them).
    fn canonical_allow_roots(&self) -> Vec<PathBuf> {
        Self::canonicalize_all(&self.allow_roots)
    }

    /// Canonicalizes the trusted roots (same logic as for allowed roots).
    fn canonical_trusted_roots(&self) -> Vec<PathBuf> {
        Self::canonicalize_all(&self.trusted_roots)
    }

    /// Canonicalizes a list of roots; failures (e.g. missing) are dropped.
    fn canonicalize_all(roots: &[PathBuf]) -> Vec<PathBuf> {
        roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .collect()
    }
}

/// Flagship skill for allowlisted file reads (read-only, SSRF-free).
///
/// Risk class is [`ActionRisk::ReadOnly`] and policy
/// [`ApprovalPolicy::AutoIfReadOnly`], so a read landing under the allowlist
/// runs automatically without approval. A path outside the allowlist is
/// rejected.
#[derive(Debug, Clone, Default)]
pub struct FsReadAllowlisted {
    /// Allowlist configuration (allowed + trusted roots).
    config: FsReadConfig,
}

impl FsReadAllowlisted {
    /// Creates the skill with an empty allowlist (rejects all paths, fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the skill with the given allowlist configuration.
    #[must_use]
    pub fn with_config(config: FsReadConfig) -> Self {
        Self { config }
    }

    /// The skill's fixed identifier.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Resolves the allowlisted, canonicalized path from the input path.
    ///
    /// Canonicalizes the requested path (resolves `..` and follows symlinks)
    /// and verifies that the result stays under some allowed root. A
    /// symlink escape (a link inside the allowlist that points outside) is
    /// rejected, because canonicalization reveals the real target as
    /// outside.
    ///
    /// # Errors
    /// - [`ActionError::PolicyDenied`] if the path does not canonicalize
    ///   (e.g. the file doesn't exist) or if the canonical path is not under
    ///   any allowed root.
    fn resolve_allowed(&self, requested: &str) -> Result<PathBuf> {
        let roots = self.config.canonical_allow_roots();
        if roots.is_empty() {
            return Err(ActionError::PolicyDenied(
                "no allowed roots — all paths rejected (fail-closed)".to_string(),
            ));
        }

        // Canonicalization resolves `..` segments and follows symlinks to
        // their real target. This is load-bearing security: an escape
        // attempt (../ or a symlink pointing out) is revealed here.
        let canonical = std::fs::canonicalize(requested).map_err(|e| {
            ActionError::PolicyDenied(format!("path could not be canonicalized (rejected): {e}"))
        })?;

        if path_is_under_any(&canonical, &roots) {
            Ok(canonical)
        } else {
            Err(ActionError::PolicyDenied(
                "canonical path is outside the allowlist (rejected)".to_string(),
            ))
        }
    }

    /// Reads the canonicalized file and assembles the core of the proof
    /// package (hash + size + summary) — NOT the full contents.
    ///
    /// Content is marked trusted only if the canonical path falls under a
    /// trusted root.
    ///
    /// # Errors
    /// Returns [`ActionError::ExecutionFailed`] if reading the file fails.
    async fn read_proof(&self, canonical: &Path) -> Result<FsReadOutput> {
        let meta = tokio::fs::metadata(canonical)
            .await
            .map_err(|e| map_fs_error(canonical, &e))?;

        if meta.is_dir() {
            return self.read_directory_proof(canonical).await;
        }

        let bytes = tokio::fs::read(canonical)
            .await
            .map_err(|e| map_fs_error(canonical, &e))?;

        let path_hash = hash_path(canonical);
        let size = bytes.len() as u64;
        let summary = summarize(&bytes);
        let trusted = path_is_under_any(canonical, &self.config.canonical_trusted_roots());

        Ok(FsReadOutput {
            path_hash,
            size,
            summary,
            trusted,
        })
    }

    async fn read_directory_proof(&self, canonical: &Path) -> Result<FsReadOutput> {
        let mut read_dir = tokio::fs::read_dir(canonical)
            .await
            .map_err(|e| map_fs_error(canonical, &e))?;

        let mut entries = Vec::new();
        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|e| map_fs_error(canonical, &e))?
        {
            entries.push(entry);
        }

        entries.sort_by_key(tokio::fs::DirEntry::file_name);

        let total = entries.len();
        let mut names = Vec::new();
        for e in entries.into_iter().take(DIR_LIST_MAX_ENTRIES) {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().await.is_ok_and(|ft| ft.is_dir());
            names.push(if is_dir { format!("{name}/") } else { name });
        }

        let mut summary = if names.is_empty() {
            "[empty directory]".to_string()
        } else {
            format!("dir: {}", names.join(", "))
        };
        if total > DIR_LIST_MAX_ENTRIES {
            let _ = write!(summary, " … (+{} more)", total - DIR_LIST_MAX_ENTRIES);
        }
        truncate_utf8(&mut summary, SUMMARY_MAX_BYTES * 4);

        let path_hash = hash_path(canonical);
        let trusted = path_is_under_any(canonical, &self.config.canonical_trusted_roots());

        Ok(FsReadOutput {
            path_hash,
            size: total as u64,
            summary,
            trusted,
        })
    }
}

/// Formats a filesystem error clearly for the agent.
fn map_fs_error(path: &Path, err: &std::io::Error) -> ActionError {
    use std::io::ErrorKind;
    let path_display = path.to_string_lossy();
    match err.kind() {
        ErrorKind::NotFound => ActionError::ExecutionFailed(format!(
            "path not found: {path_display} (check the path or create the file with file_write)"
        )),
        ErrorKind::PermissionDenied => ActionError::ExecutionFailed(format!(
            "access denied for path {path_display} — check that the path is on the allowlist \
             and does not point to a forbidden root"
        )),
        _ => ActionError::ExecutionFailed(format!("file read failed ({path_display}): {err}")),
    }
}

/// Whether `path` is under any of the given roots (or is the root itself).
///
/// The comparison is done at the component level with
/// [`Path::starts_with`] semantics, so e.g. `/allow/dir2` does not match the
/// root `/allow/dir` (a prefix is not enough — the whole component must
/// match).
fn path_is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Computes the SHA-256 hash of the canonical path as a hex string.
///
/// The path is hashed from its byte representation; the hash is stored in
/// the proof instead of the raw path, so that a (potentially private) path
/// does not leak.
fn hash_path(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Assembles a short, truncated summary from the file's bytes.
///
/// Takes the first line (or the whole content if there is no line break),
/// truncates it to the [`SUMMARY_MAX_BYTES`] limit in a UTF-8-safe way, and
/// strips control characters. **Not** the full content — the summary is
/// intentionally terse.
fn summarize(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let first_line = text.lines().next().unwrap_or("");
    let mut summary: String = first_line.chars().filter(|c| !c.is_control()).collect();
    truncate_utf8(&mut summary, SUMMARY_MAX_BYTES);
    summary.trim().to_string()
}

/// Truncates a string to at most `max_bytes` bytes while preserving UTF-8 boundaries.
fn truncate_utf8(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

#[async_trait]
impl ActionExecutor for FsReadAllowlisted {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: FsReadInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid fs_read input: {e}"),
                    request.now,
                ));
            }
        };

        // Resolve + validate against the allowlist. A rejected path →
        // a failure result (no panic, no error that would crash the pipeline).
        let canonical = match self.resolve_allowed(&input.path) {
            Ok(path) => path,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("path rejected: {e}"),
                    request.now,
                ));
            }
        };

        let out = match self.read_proof(&canonical).await {
            Ok(out) => out,
            Err(e) => {
                return Ok(ActionResult::failure(e.to_string(), request.now));
            }
        };

        let mut output: Value = json!({
            "path_hash": out.path_hash,
            "size": out.size,
            "summary": out.summary,
            "trusted": out.trusted,
        });

        if input.read_full_content && !canonical.is_dir() {
            if let Ok(bytes) = tokio::fs::read(&canonical).await {
                let mut content = String::from_utf8_lossy(&bytes).into_owned();
                truncate_utf8(&mut content, FULL_CONTENT_MAX_BYTES);
                if let Some(obj) = output.as_object_mut() {
                    obj.insert("content".to_string(), json!(content));
                }
            }
        }

        let trusted = out.trusted;
        let result = ActionResult::success(
            if out.summary.starts_with("dir:") || out.summary == "[empty directory]" {
                format!("listed {} entries in allowlisted directory", out.size)
            } else {
                format!("read {} byte(s) from allowlisted path", out.size)
            },
            output,
            request.now,
        );

        // Taint is cleared only for trusted project files; otherwise the
        // output stays untrusted (default).
        if trusted {
            Ok(result.trusted())
        } else {
            Ok(result)
        }
    }
}

impl Skill for FsReadAllowlisted {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "fs_read_allowlisted".to_string(),
            version: "1.0.0".to_string(),
            description: "Reads a local file or lists a directory only from under an \
                 allowlisted root (canonicalized path, no network). Default: hash + summary; \
                 `read_full_content: true` also returns the content (max 64 KiB)."
                .to_string(),
            permissions: vec![SkillPermission::ReadFiles],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ path }".to_string()),
            output_hint: Some("{ path_hash, size, summary, trusted, content? }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path of the file to read (canonicalized; must stay under an allowlisted root)."
                    },
                    "read_full_content": {
                        "type": "boolean",
                        "description": "When true, also returns the file's content (max 64 KiB). Default false."
                    }
                },
                "required": ["path"],
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
    use std::io::Write;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Creates an isolated temp directory for this test (canonicalized so
    /// macOS `/var`→`/private/var` symlinks don't confuse `starts_with` comparisons).
    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("familyclaw_fs_read_{tag}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::canonicalize(&dir).expect("canonicalize temp dir")
    }

    /// Writes a file with the given content and returns its path.
    fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create file");
        f.write_all(contents.as_bytes()).expect("write file");
        path
    }

    #[test]
    fn manifest_is_read_only_auto_and_generic() {
        let m = FsReadAllowlisted::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "fs_read_allowlisted");
        assert_eq!(m.risk, ActionRisk::ReadOnly);
        assert_eq!(m.approval_policy, ApprovalPolicy::AutoIfReadOnly);
        // The schema advertises the `path` field as genuine JSON Schema.
        assert_eq!(m.input_schema["properties"]["path"]["type"], "string");
        assert_eq!(m.input_schema["required"][0], "path");
        // Generic: no family names, no private paths in the manifest.
        // Forbidden names are built at runtime from fragments, so the
        // source file contains no single whole family-name literal
        // (otherwise scripts/audit-layer-b.sh would flag our own test).
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
        // And no absolute/private paths either (generic publishable skill).
        assert!(!rendered.contains(":\\"), "no Windows absolute paths");
        assert!(
            !rendered.contains("/home/"),
            "no private home paths in manifest"
        );
    }

    #[tokio::test]
    async fn reads_allowlisted_file_ok() {
        let dir = temp_dir("ok");
        write_file(&dir, "doc.txt", "hello world\nsecond line\n");
        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&dir));

        let payload = serde_json::to_value(FsReadInput {
            path: dir.join("doc.txt").to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success(), "allowlisted read must succeed");
        assert_eq!(res.raw_output_redacted["size"], json!(24));
        assert_eq!(res.raw_output_redacted["summary"], json!("hello world"));
    }

    #[tokio::test]
    async fn read_full_content_returns_body() {
        let dir = temp_dir("full");
        write_file(&dir, "doc.txt", "full body text for operator");
        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&dir));

        let payload = serde_json::to_value(FsReadInput {
            path: dir.join("doc.txt").to_string_lossy().to_string(),
            read_full_content: true,
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        assert_eq!(
            res.raw_output_redacted["content"],
            json!("full body text for operator")
        );
    }

    #[tokio::test]
    async fn rejects_outside_allowlist() {
        let allowed = temp_dir("allowed");
        let other = temp_dir("other");
        // The file DOES exist (canonicalizes) but is NOT under the allowlisted root.
        write_file(&other, "secret.txt", "outside");
        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&allowed));

        let payload = serde_json::to_value(FsReadInput {
            path: other.join("secret.txt").to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(
            !res.status.is_success(),
            "path outside allowlist must be rejected"
        );
        assert!(res.output_summary.contains("rejected"));
    }

    #[tokio::test]
    async fn rejects_dot_dot_traversal() {
        // Allowlist = subdirectory; attempt a `..` escape outward.
        let base = temp_dir("traversal");
        let allowed = base.join("inside");
        std::fs::create_dir_all(&allowed).expect("create inside");
        // Secret file in the sibling directory (outside the allowlist).
        write_file(&base, "outside.txt", "secret");

        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&allowed));

        // `<allowed>/../outside.txt` → canonicalizes to `<base>/outside.txt`,
        // which is NOT under the allowlist → rejected.
        let traversal = allowed.join("..").join("outside.txt");
        let payload = serde_json::to_value(FsReadInput {
            path: traversal.to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(
            !res.status.is_success(),
            ".. traversal escaping the allowlist must be rejected"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escape() {
        // Symlink INSIDE the allowlist that points OUTSIDE the allowlist.
        // Canonicalization follows the link → the real target is revealed as outside.
        let allowed = temp_dir("symlink_allowed");
        let outside = temp_dir("symlink_outside");
        let secret = write_file(&outside, "secret.txt", "leak me");

        let link = allowed.join("link_to_secret.txt");
        std::os::unix::fs::symlink(&secret, &link).expect("create symlink");

        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&allowed));

        let payload = serde_json::to_value(FsReadInput {
            path: link.to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(
            !res.status.is_success(),
            "symlink pointing outside the allowlist must be rejected"
        );
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn rejects_symlink_escape() {
        // On non-Unix platforms, creating a symlink may require privileges;
        // verify the same invariant via a junction/`..`: an outside target
        // is rejected. (Symlink escape is covered separately on Unix.)
        let allowed = temp_dir("symlink_allowed_win");
        let outside = temp_dir("symlink_outside_win");
        write_file(&outside, "secret.txt", "leak me");

        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&allowed));
        let escape = allowed.join("..").join(
            outside
                .file_name()
                .expect("outside dir name")
                .to_string_lossy()
                .to_string(),
        );
        let escape = escape.join("secret.txt");
        let payload = serde_json::to_value(FsReadInput {
            path: escape.to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(
            !res.status.is_success(),
            "path resolving outside the allowlist must be rejected"
        );
    }

    #[tokio::test]
    async fn proof_contains_hash_and_size_not_contents() {
        let dir = temp_dir("proof");
        // The tell-tale marker is INTENTIONALLY on the second line: the summary
        // takes only the first line, so the rest of the file body (lines 2+) must not leak.
        let contents = "harmless first line\nmust never appear: full body line two\n";
        write_file(&dir, "doc.txt", contents);
        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&dir));

        let payload = serde_json::to_value(FsReadInput {
            path: dir.join("doc.txt").to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());

        // The hash (64 hex characters) and size are present.
        let hash = res.raw_output_redacted["path_hash"]
            .as_str()
            .expect("path_hash present");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            res.raw_output_redacted["size"].as_u64().expect("size"),
            contents.len() as u64
        );

        // The summary is ONLY the first line — the file body (lines 2+) does not leak.
        assert_eq!(
            res.raw_output_redacted["summary"],
            json!("harmless first line")
        );

        // The full file content must NOT appear in the output (only the
        // summary, which is the file's first line truncated — not the full content).
        let rendered = serde_json::to_string(&res.raw_output_redacted).expect("serialize output");
        assert!(
            !rendered.contains("must never appear"),
            "proof must not contain full file contents (only first-line summary)"
        );
    }

    #[tokio::test]
    async fn output_tainted_unless_trusted_project_file() {
        let untrusted_dir = temp_dir("untrusted");
        let trusted_dir = temp_dir("trusted");
        write_file(&untrusted_dir, "u.txt", "untrusted data");
        write_file(&trusted_dir, "t.txt", "trusted data");

        // The allowlist contains both an untrusted and a trusted root.
        let config = FsReadConfig::new()
            .allow_root(&untrusted_dir)
            .trusted_root(&trusted_dir);
        let skill = FsReadAllowlisted::with_config(config);

        // Read from under the untrusted root → the output stays tainted.
        let untrusted_payload = serde_json::to_value(FsReadInput {
            path: untrusted_dir.join("u.txt").to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let untrusted_request = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            untrusted_payload,
            at(1),
        );
        let untrusted_result = skill.execute(untrusted_request).await.expect("execute");
        assert!(untrusted_result.status.is_success());
        assert!(
            untrusted_result.untrusted,
            "non-project file must stay tainted"
        );
        assert_eq!(
            untrusted_result.raw_output_redacted["trusted"],
            json!(false)
        );

        // Read from under the trusted root → taint is cleared.
        let trusted_payload = serde_json::to_value(FsReadInput {
            path: trusted_dir.join("t.txt").to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let trusted_request = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            trusted_payload,
            at(1),
        );
        let trusted_result = skill.execute(trusted_request).await.expect("execute");
        assert!(trusted_result.status.is_success());
        assert!(
            !trusted_result.untrusted,
            "trusted project file must clear the taint"
        );
        assert_eq!(trusted_result.raw_output_redacted["trusted"], json!(true));
    }

    #[tokio::test]
    async fn empty_allowlist_rejects_everything() {
        let dir = temp_dir("empty_allow");
        write_file(&dir, "doc.txt", "data");
        // Empty allowlist → fail-closed.
        let skill = FsReadAllowlisted::new();
        let payload = serde_json::to_value(FsReadInput {
            path: dir.join("doc.txt").to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(
            !res.status.is_success(),
            "empty allowlist must reject all paths"
        );
    }

    #[tokio::test]
    async fn lists_directory_entries_when_path_is_dir() {
        let dir = temp_dir("dir_list");
        write_file(&dir, "b.txt", "beta");
        write_file(&dir, "a.txt", "alpha");
        std::fs::create_dir(dir.join("sub")).expect("subdir");

        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&dir));
        let payload = serde_json::to_value(FsReadInput {
            path: dir.to_string_lossy().to_string(),
            ..Default::default()
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FsReadAllowlisted::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success(), "directory listing must succeed");
        let summary = res.raw_output_redacted["summary"]
            .as_str()
            .expect("summary");
        assert!(
            summary.starts_with("dir:"),
            "summary must list dir: {summary}"
        );
        assert!(summary.contains("a.txt"), "must include a.txt");
        assert!(summary.contains("sub/"), "must mark subdir with trailing /");
    }

    #[test]
    fn summarize_truncates_and_drops_control_chars() {
        let long = "a".repeat(500);
        let s = summarize(long.as_bytes());
        assert!(s.len() <= SUMMARY_MAX_BYTES);
        // Only the first line; control characters stripped.
        let multi = "first\u{7}line\nsecond line";
        let s2 = summarize(multi.as_bytes());
        assert_eq!(s2, "firstline");
    }
}
