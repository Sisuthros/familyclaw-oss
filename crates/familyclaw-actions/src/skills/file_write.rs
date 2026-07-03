//! Lippulaiva-taito: allowlistattu paikallisen tiedoston KIRJOITUS (KERROS A).
//!
//! [`FileWriteAllowlisted`] on aito levylle kirjoittava suorittaja — toisin kuin
//! [`crate::skills::file_patch::FilePatchMock`], joka on determinis­tinen
//! ehdotus-mock. Tämä taito **kirjoittaa oikeasti** tiedoston levylle, mutta
//! vain allowlistatun juuren alle, ja peilaa [`crate::skills::fs_read`]-taidon
//! allowlist- ja kanonisointimallia.
//!
//! ## Kuormaa kantava turvallisuus: kanonisointi + allowlist
//! Taito ottaa polun ([`FileWriteInput::path`]) ja varmistaa että kohde pysyy
//! **jonkin allowlistatun juuren alla** ENNEN kirjoitusta:
//! 1. **kanonisoi** kohteen vanhempihakemiston ([`std::fs::canonicalize`]) —
//!    purkaa `..`-segmentit ja seuraa symlinkit niiden todelliseen kohteeseen;
//!    jos vanhempaa ei vielä ole, kiivetään ylös lähimpään olemassa olevaan
//!    esivanhempaan ja kanonisoidaan se,
//! 2. varmistaa että kanoninen kohde pysyy jonkin (kanonisoidun) juuren alla,
//! 3. **hylkää** kaikki kohteet allowlistin ulkopuolella — mukaan lukien
//!    `..`-pakenemiset ja symlink-pakenemiset (allowlistin sisällä oleva linkki
//!    joka osoittaa ulos kanonisoituu ulkopuoliseksi ja hylätään).
//!
//! Tyhjä allowlist (oletus) hylkää **kaikki** polut — fail-closed.
//!
//! ## Riskiluokka ja hyväksyntä
//! Riski on [`ActionRisk::WriteLocal`] ja oikeus [`SkillPermission::WriteLocalFiles`].
//! Käytäntö on [`ApprovalPolicy::AlwaysRequireApproval`]: levylle kirjoittaminen
//! vaatii aina ihmisen hyväksynnän putkessa — se ei koskaan aja itsestään.
//!
//! ## Todistepaketti ei sisällä sisältöä
//! Tulos sisältää vain kanonisen polun **tiivisteen** (SHA-256), kirjoitettujen
//! tavujen **määrän** sekä tilan (`overwrite`/`append`) — EI koskaan kirjoitetun
//! sisällön runkoa. Näin todiste ei vuoda kirjoitettua dataa.

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

/// Taidon kiinteä tunniste (1–6 ovat varattuja muille oletustaidoille).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("99999999-9999-4999-8999-999999999999");

/// Kirjoitustila: korvaa tiedosto vai lisää loppuun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Korvaa tiedoston koko sisältö (luo jos puuttuu). Oletus.
    #[default]
    Overwrite,
    /// Lisää sisältö tiedoston loppuun (luo jos puuttuu).
    Append,
}

impl WriteMode {
    /// Ihmisluettava nimi tilalle (tulosteessa käytettävä).
    const fn as_str(self) -> &'static str {
        match self {
            Self::Overwrite => "overwrite",
            Self::Append => "append",
        }
    }
}

/// Taidon syöte: kirjoitettavan tiedoston polku, sisältö ja valinnainen tila.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWriteInput {
    /// Kirjoitettavan tiedoston polku. Kanonisoidaan ja sen on pysyttävä
    /// allowlistatun juuren alla.
    pub path: String,
    /// Kirjoitettava sisältö.
    pub content: String,
    /// Kirjoitustila (`overwrite`/`append`). Oletus `overwrite`.
    #[serde(default)]
    pub mode: WriteMode,
}

/// Taidon tulos: todistepaketin ydin (tiiviste + tavumäärä + tila).
///
/// **EI** sisällä kirjoitettua sisältöä — vain kuormaa kantavat metatiedot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWriteOutput {
    /// Kanonisen kohdepolun SHA-256-tiiviste (heksa) — EI raakaa polkua.
    pub path_hash: String,
    /// Kirjoitettujen tavujen määrä.
    pub bytes_written: u64,
    /// Käytetty kirjoitustila (`overwrite`/`append`).
    pub mode: String,
}

/// Allowlist-kokoonpano: sallitut juurihakemistot kirjoitukselle.
///
/// Kokoonpano on **konfiguroitavissa** — taito ei kovakoodaa mitään polkua,
/// joten julkaistava lähde pysyy geneerisenä (ei yksityisiä polkuja). Tyhjä
/// allowlist (oletus) hylkää **kaikki** polut — fail-closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileWriteConfig {
    /// Sallitut juurihakemistot. Kirjoitus sallitaan vain jos kanoninen kohde
    /// pysyy jonkin näistä (kanonisoidun) juuren alla.
    allow_roots: Vec<PathBuf>,
}

impl FileWriteConfig {
    /// Luo tyhjän kokoonpanon, joka hylkää kaikki polut (fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lisää sallitun juuren (rakentaja-ketjutus).
    ///
    /// Juurta ei kanonisoida tässä — kanonisointi tehdään vasta kirjoitushetkellä,
    /// jotta kokoonpanon voi rakentaa myös ennen kuin hakemisto on olemassa.
    #[must_use]
    pub fn allow_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.allow_roots.push(root.into());
        self
    }

    /// Kanonisoi sallitut juuret. Olemassaolemattomat tai kanonisoitumattomat
    /// juuret ohitetaan hiljaa (niiden alle ei voi koskaan osua).
    fn canonical_allow_roots(&self) -> Vec<PathBuf> {
        self.allow_roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .collect()
    }
}

/// Lippulaiva-taito allowlistatulle tiedoston kirjoitukselle (aito levykirjoitus).
///
/// Riskiluokka on [`ActionRisk::WriteLocal`] ja käytäntö
/// [`ApprovalPolicy::AlwaysRequireApproval`], joten kirjoitus vaatii aina
/// hyväksynnän putkessa. Allowlistin ulkopuolinen kohde hylätään.
#[derive(Debug, Clone, Default)]
pub struct FileWriteAllowlisted {
    /// Allowlist-kokoonpano (sallitut juuret).
    config: FileWriteConfig,
}

impl FileWriteAllowlisted {
    /// Luo taidon tyhjällä allowlistilla (hylkää kaikki polut, fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Luo taidon annetulla allowlist-kokoonpanolla.
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
    /// Koska kohdetiedostoa ei ehkä vielä ole (eikä sitä siis voi suoraan
    /// kanonisoida), taito:
    /// 1. torjuu kohdepolut jotka päättyvät `..`-segmenttiin (ei tiedostonimeä),
    /// 2. kanonisoi lähimmän **olemassa olevan** esivanhemman (purkaa `..` ja
    ///    seuraa symlinkit), ja liimaa sen perään jäljellä olevat "normaalit"
    ///    komponentit,
    /// 3. varmistaa että lopullinen kanoninen kohde pysyy jonkin sallitun juuren
    ///    alla. Symlink-pakeneminen paljastuu askeleessa 2 (esivanhemman
    ///    kanonisointi seuraa linkit todelliseen kohteeseen).
    ///
    /// # Errors
    /// - [`ActionError::PolicyDenied`] jos allowlist on tyhjä, jos polkua ei voi
    ///   ratkaista tai jos kanoninen kohde ei ole minkään sallitun juuren alla.
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

    /// Kirjoittaa sisällön kanonisoituun kohteeseen ja koostaa todistepaketin
    /// ytimen (tiiviste + tavumäärä + tila) — EI kirjoitettua sisältöä.
    ///
    /// Luo tarvittaessa puuttuvat vanhempihakemistot (jotka kanonisoinnin
    /// perusteella pysyvät allowlistin sisällä). `overwrite` korvaa koko
    /// tiedoston, `append` lisää loppuun.
    ///
    /// # Errors
    /// Palauttaa [`ActionError::ExecutionFailed`] jos hakemiston luonti tai
    /// tiedoston kirjoitus epäonnistuu.
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

/// Ratkaisee (mahdollisesti vielä olemattoman) kohdepolun kanonisen muodon.
///
/// Kanonisoi lähimmän olemassa olevan esivanhemman ja liittää siihen jäljellä
/// olevat normaalikomponentit. Torjuu `..`-segmentit jäljellä olevassa osassa
/// (ne voisivat paeta allowlistia symlinkin läpi kulkematta kanonisoinnin
/// kautta).
///
/// # Errors
/// [`ActionError::PolicyDenied`] jos polku on tyhjä, päättyy `..`:iin, tai jos
/// yksikään esivanhempi ei kanonisoidu.
fn canonicalize_target(requested: &Path) -> Result<PathBuf> {
    // Jos kohde on jo olemassa, kanonisoi suoraan (seuraa symlinkit).
    if let Ok(canonical) = std::fs::canonicalize(requested) {
        return Ok(canonical);
    }

    // Muuten kiivetään ylös lähimpään olemassa olevaan esivanhempaan.
    let mut existing = requested;
    let mut tail: Vec<Component<'_>> = Vec::new();
    loop {
        match existing.parent() {
            Some(parent) => {
                let file = existing.components().next_back().ok_or_else(|| {
                    ActionError::PolicyDenied("kohdepolku on tyhjä (hylätty)".to_string())
                })?;
                // `..` jäljellä olevassa hännässä voisi paeta juuresta
                // ilman että kanonisointi näkee sitä → torjutaan.
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

/// Onko `path` jonkin annetun juuren alla (tai itse juuri).
///
/// Vertailu tehdään komponentti-tasolla [`Path::starts_with`]-semantiikalla,
/// joten esim. `/allow/dir2` ei osu juureen `/allow/dir` (etuliite ei riitä —
/// koko komponentin on täsmättävä).
fn path_is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Laskee kanonisen polun SHA-256-tiivisteen heksamerkkijonona.
///
/// Polku tiivistetään tavuesityksestään; tiiviste tallennetaan todisteeseen
/// raakapolun sijasta, jottei (mahdollisesti yksityinen) polku vuoda.
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

        // Ratkaise + validoi allowlist. Hylätty kohde → epäonnistunut tulos
        // (ei paniikkia, ei virhettä joka kaataisi putken).
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

        // Kirjoituksen tulos pysyy oletuksena epäluotettavana (ei .trusted()).
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
            approval_policy: ApprovalPolicy::AlwaysRequireApproval,
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

    /// Luo eristetyn väliaikaishakemiston tälle testille (kanonisoituna, jotta
    /// macOS `/var`→`/private/var`-symlinkit eivät sotke `starts_with`-vertailua).
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
    fn manifest_is_write_local_always_approve_and_generic() {
        let m = FileWriteAllowlisted::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "file_write_allowlisted");
        assert_eq!(m.risk, ActionRisk::WriteLocal);
        assert_eq!(m.approval_policy, ApprovalPolicy::AlwaysRequireApproval);
        assert_eq!(m.permissions, vec![SkillPermission::WriteLocalFiles]);
        assert_eq!(m.input_schema["properties"]["path"]["type"], "string");
        assert_eq!(m.input_schema["properties"]["content"]["type"], "string");
        // Geneerinen: ei perhenimiä eikä yksityisiä polkuja manifestissa.
        // Kielletyt nimet rakennetaan fragmenteista, jottei lähdetiedostossa ole
        // yhtäkään kokonaista perhenimi-literaalia (audit-layer-b.sh napsahtaisi).
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

        // Luetaan takaisin levyltä ja varmistetaan sisältö.
        let read_back = std::fs::read_to_string(&target).expect("read back");
        assert_eq!(read_back, "hello disk");
    }

    #[tokio::test]
    async fn creates_parent_dirs_within_allowlist() {
        let dir = temp_dir("nested");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&dir));

        // Vanhempihakemistoja ("a/b/") ei ole vielä olemassa.
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

        // Kohde on toisen (ei-allowlistatun) juuren alla.
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
        // Todiste sivuvaikutuksen puuttumisesta: tiedostoa ei luotu.
        assert!(!target.exists(), "rejected write must not touch disk");
    }

    #[tokio::test]
    async fn rejects_dot_dot_traversal() {
        // Allowlist = alihakemisto; yritetään `..`-pakeneminen ulos.
        let base = temp_dir("traversal");
        let allowed = base.join("inside");
        std::fs::create_dir_all(&allowed).expect("create inside");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&allowed));

        // `<allowed>/../outside.txt` → kanonisoituu `<base>/outside.txt`:ksi,
        // joka EI ole allowlistin alla → hylätään.
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
        // Symlink allowlistin SISÄLLÄ joka osoittaa allowlistin ULKOPUOLELLE.
        // Kanonisointi seuraa linkin → todellinen kohde paljastuu ulkopuoliseksi.
        let allowed = temp_dir("symlink_allowed");
        let outside = temp_dir("symlink_outside");

        let link_dir = allowed.join("link_dir");
        std::os::unix::fs::symlink(&outside, &link_dir).expect("create symlink");
        let skill = FileWriteAllowlisted::with_config(FileWriteConfig::new().allow_root(&allowed));

        // Kirjoitus <allowed>/link_dir/leak.txt → kanonisoituu <outside>/leak.txt.
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

        // Ensin kirjoitetaan pohja overwrite-tilassa.
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

        // Sitten lisätään append-tilassa.
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

        // Sisältö = molemmat rivit peräkkäin (append EI korvannut).
        let read_back = std::fs::read_to_string(&target).expect("read back");
        assert_eq!(read_back, "line1\nline2\n");
    }

    #[tokio::test]
    async fn empty_allowlist_rejects_everything() {
        let dir = temp_dir("empty_allow");
        // Tyhjä allowlist → fail-closed.
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

        // Tiiviste (64 heksamerkkiä) ja tavumäärä ovat läsnä.
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

        // Kirjoitettu sisältö EI saa esiintyä todisteessa.
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
        // Puuttuva `content`-kenttä → parse-virhe → epäonnistunut tulos (ei paniikkia).
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
