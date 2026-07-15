//! Aito taito: unified-diffin SOVELTAMINEN allowlistatulle tiedostolle (KERROS A).
//!
//! [`FilePatchApply`] on `file_patch`-provider-taidon **aito toteutus** — se
//! korvaa aiemman deterministisen ehdotus-mockin. Taito **kirjoittaa oikeasti**
//! sovelletun patchin levylle, mutta vain **allowlistatun juuren alle**, ja
//! peilaa [`super::file_write::FileWriteAllowlisted`]-taidon tarkkaa
//! turvallisuusmallia:
//!
//! ## Kuormaa kantava turvallisuus: kanonisointi + allowlist
//! Ennen kirjoitusta kohde **kanonisoidaan** ([`std::fs::canonicalize`], joka
//! purkaa `..`-segmentit ja seuraa symlinkit todelliseen kohteeseen) ja
//! varmistetaan että se pysyy jonkin (kanonisoidun) allowlistatun juuren alla.
//! Kaikki muut kohteet — `..`-pakenemiset ja symlink-pakenemiset — **hylätään**
//! ennen kirjoitusta. Tyhjä allowlist (oletus) hylkää **kaikki** polut
//! (fail-closed).
//!
//! ## Riskiluokka ja hyväksyntä
//! Riski on [`ActionRisk::WriteLocal`] ja käytäntö
//! [`ApprovalPolicy::AlwaysRequireApproval`], joten patchin soveltaminen
//! pysähtyy **aina** ihmisen hyväksyntään ennen suoritusta — putki johtaa
//! vaatimuksen manifestista, ei payloadista, joten payloadiin upotettu
//! kehotehyökkäys ei voi ohittaa porttia.
//!
//! ## Todistepaketti ei sisällä sisältöä
//! Tulos sisältää vain kanonisen polun **tiivisteen** (SHA-256), sovelluslipun
//! sekä muutettujen rivien **määrän** — EI koskaan tiedoston tai patchin
//! sisältöä. Näin todiste ei vuoda kirjoitettua dataa.

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

/// Taidon kiinteä tunniste (jaettu aiemman `file_patch`-mockin kanssa, jotta
/// rekisteröinti ja haku pysyvät taaksepäin-yhteensopivina).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("44444444-4444-4444-8444-444444444444");

/// Syöte `file_patch_apply`-taidolle: kohdetiedosto ja unified-diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchApplyInput {
    /// Kohdetiedoston polku. Kanonisoidaan ja sen on pysyttävä allowlistatun
    /// juuren alla.
    pub path: String,
    /// Yhtenäinen diff (unified format).
    pub patch: String,
}

/// Tulos `file_patch_apply`-taidolle: patchin sovelluksen todiste.
///
/// **EI** sisällä tiedoston eikä patchin sisältöä — vain kuormaa kantavat
/// metatiedot (tiiviste + sovelluslippu + rivimäärä).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchApplyOutput {
    /// Kanonisen kohdepolun SHA-256-tiiviste (heksa) — EI raakaa polkua.
    pub path_hash: String,
    /// `true` jos patch sovellettiin tiedostoon.
    pub applied: bool,
    /// Muutettujen rivien lukumäärä (|uusi − vanha|).
    pub lines_changed: u64,
}

/// Aito taito: soveltaa unified-diffin allowlistattuun tiedostoon (levykirjoitus).
///
/// Riskiluokka on [`ActionRisk::WriteLocal`] ja käytäntö
/// [`ApprovalPolicy::AlwaysRequireApproval`]: soveltaminen pysähtyy aina
/// hyväksyntään; allowlistin ulkopuolinen kohde hylätään.
#[derive(Debug, Clone, Default)]
pub struct FilePatchApply {
    /// Allowlist-kokoonpano (sallitut juuret) — jaettu `file_write`-mallin kanssa.
    config: FileWriteConfig,
}

impl FilePatchApply {
    /// Luo taidon tyhjällä allowlistilla (hylkää kaikki polut, fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Luo taidon annetulla kirjoituskonfiguraatiolla (sallitut juuret).
    #[must_use]
    pub fn with_config(config: FileWriteConfig) -> Self {
        Self { config }
    }

    /// Taidon kiinteä tunniste.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Ratkaisee allowlistatun, kanonisoidun kohdepolun syötteen polusta.
    ///
    /// # Errors
    /// [`ActionError::PolicyDenied`] jos allowlist on tyhjä, polkua ei voi
    /// ratkaista, tai kanoninen kohde ei ole minkään sallitun juuren alla.
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
                "patch-kohde on allowlistin ulkopuolella (hylätty)".to_string(),
            ))
        }
    }

    /// Soveltaa yksinkertaisen unified-diffin yhteen tiedostoon (puhdas logiikka).
    ///
    /// Käsittelee `@@`-hunk-otsakkeet ja niiden sisällä `+` (lisää), `-` (poista)
    /// ja ` ` (konteksti) rivit. Ei tarvitse tarkkaa rivinumerointia — KERROS A
    /// -tasolla riittää deterministinen, sisältöpohjainen soveltaminen.
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

/// Onko `path` jonkin annetun juuren alla (tai itse juuri).
fn path_is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Ratkaisee (mahdollisesti vielä olemattoman) kohdepolun kanonisen muodon.
///
/// Kanonisoi lähimmän olemassa olevan esivanhemman (purkaa `..` ja seuraa
/// symlinkit) ja liittää siihen jäljellä olevat normaalikomponentit. Torjuu
/// `..`-segmentit jäljellä olevassa hännässä (ne voisivat paeta juuresta
/// kulkematta kanonisoinnin kautta).
///
/// # Errors
/// [`ActionError::PolicyDenied`] jos polku on tyhjä, päättyy `..`:iin, tai jos
/// yksikään esivanhempi ei kanonisoidu.
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

/// Laskee kanonisen polun SHA-256-tiivisteen heksamerkkijonona (raakapolun
/// sijasta, jottei mahdollisesti yksityinen polku vuoda todisteeseen).
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

        // Ratkaise + validoi allowlist. Hylätty kohde → epäonnistunut tulos
        // (ei virhettä joka kaataisi putken, sama malli kuin file_write).
        let canonical = match self.resolve_allowed(&input.path) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("path rejected: {e}"),
                    request.now,
                ));
            }
        };

        // Lue nykyinen sisältö (puuttuva tiedosto → tyhjä pohja).
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

        // Tulos pysyy oletuksena epäluotettavana (ei .trusted()).
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
            description:
                "Soveltaa unified-diffin allowlistatulle tiedostolle (kanonisoitu kohde); \
                 todiste = polkutiiviste + sovelluslippu + muutettujen rivien määrä, ei sisältöä."
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
                        "description": "Kohdetiedoston polku (kanonisoidaan; on pysyttävä allowlistatun juuren alla)."
                    },
                    "patch": {
                        "type": "string",
                        "description": "Sovellettava yhtenäinen diff (unified format)."
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

    /// Luo eristetyn väliaikaishakemiston (kanonisoituna, jotta macOS
    /// `/var`→`/private/var`-symlinkit eivät sotke `starts_with`-vertailua).
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
        // AlwaysRequireApproval → policy pakottaa hyväksynnän paikallisellekin
        // kirjoitukselle.
        assert_eq!(
            required_approval(m.risk, m.approval_policy),
            ApprovalRequirement::RequireApproval
        );
        // Geneerinen: ei yksityisiä polkuja manifestissa.
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
        // Sivuvaikutuksen puuttuminen: tiedostoa ei muutettu.
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

        // Tyhjä allowlist → fail-closed.
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
        // <allowed>/../outside.txt → kanonisoituu <base>/outside.txt (allowlistin
        // ulkopuolella) → hylätään.
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
        // Symlink allowlistin SISÄLLÄ joka osoittaa ULKOPUOLELLE — kanonisointi
        // seuraa linkin ja paljastaa todellisen kohteen ulkopuoliseksi.
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

        // Tiiviste (64 heksamerkkiä) läsnä, raakaa polkua ei.
        let hash = res.raw_output_redacted["path_hash"]
            .as_str()
            .expect("path_hash present");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // Patchin/tiedoston sisältö EI saa esiintyä todisteessa.
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
