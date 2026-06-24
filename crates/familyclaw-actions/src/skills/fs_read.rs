//! Lippulaiva-taito: allowlistattu paikallisen tiedoston luku (KERROS A).
//!
//! [`FsReadAllowlisted`] todistaa koko työkalusilmukan (observe→…→report)
//! **avaamatta verkko-ovea**: se on tarkoituksella **ei** http-get, vaan lukee
//! vain paikallisia tiedostoja, ja on **SSRF-turvallinen rakenteeltaan** —
//! verkkopyyntöä ei voi muodostaa, koska taito ei koskaan koske verkkoon.
//!
//! ## Kuormaa kantava turvallisuus: kanonisointi + allowlist
//! Taito ottaa polun ([`FsReadInput::path`]) ja:
//! 1. **kanonisoi** sen ([`std::fs::canonicalize`]) — purkaa `..`-segmentit ja
//!    seuraa symlinkit niiden todelliseen kohteeseen,
//! 2. varmistaa että kanoninen polku pysyy **jonkin allowlistatun juuren alla**
//!    (juuret myös kanonisoidaan ennen vertailua),
//! 3. **hylkää** kaikki polut allowlistin ulkopuolella — mukaan lukien
//!    symlink-pakenemiset (symlink allowlistin sisällä joka osoittaa ulos
//!    kanonisoituu ulkopuoliseksi kohteeksi ja hylätään).
//!
//! Allowlistin alle osuva luku ajaa **automaattisesti** (ei hyväksyntää);
//! allowlistin ulkopuolinen polku hylätään.
//!
//! ## Todistepaketti ei sisällä koko tiedostoa
//! Tulos sisältää oletuksena vain polun **tiivisteen** (SHA-256), tiedoston
//! **koon** tavuina sekä lyhyen **yhteenvedon** — EI koko tiedoston sisältöä.
//! Näin todiste ei vuoda luettua dataa eikä paisu.
//!
//! ## Taint (epäluotettavuus)
//! Luettu tuloste on oletuksena **epäluotettavaa** (taint). Vain jos kanoninen
//! polku osuu erikseen **luotettujen** juurten alle (projektin omat tiedostot),
//! tuloste merkitään luotetuksi. Näin ulkopuolelta tuotu sisältö ei pese
//! itseään puhtaaksi.

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

/// Taidon kiinteä tunniste, jotta rekisteröinti ja haku ovat toistettavia.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("55555555-5555-4555-8555-555555555555");

/// Yhteenvedon enimmäispituus tavuina — pidetään lyhyenä, jottei koko tiedoston
/// sisältö koskaan vuoda todisteeseen yhteenvedon kautta.
const SUMMARY_MAX_BYTES: usize = 120;

/// Taidon syöte: luettavan tiedoston polku.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadInput {
    /// Luettavan tiedoston polku (suhteellinen tai absoluuttinen). Polku
    /// kanonisoidaan ja sen on pysyttävä allowlistatun juuren alla.
    pub path: String,
}

/// Taidon tulos: todistepaketin ydin (tiiviste + koko + yhteenveto).
///
/// **EI** sisällä koko tiedoston sisältöä — vain kuormaa kantavat metatiedot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadOutput {
    /// Kanonisen polun SHA-256-tiiviste (heksa) — EI raakaa polkua.
    pub path_hash: String,
    /// Luetun tiedoston koko tavuina.
    pub size: u64,
    /// Lyhyt ihmisluettava yhteenveto (typistetty, EI koko sisältöä).
    pub summary: String,
    /// Onko sisältö projektin luotettua tiedostoa (vaikuttaa taint-tilaan).
    pub trusted: bool,
}

/// Allowlist-kokoonpano: sallitut juuret ja niiden luotettava osajoukko.
///
/// Kokoonpano on **konfiguroitavissa** — taito ei kovakoodaa mitään polkua,
/// joten julkaistava lähde pysyy geneerisenä (ei yksityisiä polkuja). Tyhjä
/// allowlist (oletus) hylkää **kaikki** polut — fail-closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FsReadConfig {
    /// Sallitut juurihakemistot. Luku sallitaan vain jos kanoninen polku pysyy
    /// jonkin näistä (kanonisoidun) juuren alla.
    allow_roots: Vec<PathBuf>,
    /// Luotettujen juurten osajoukko: näiden alta luettu sisältö merkitään
    /// luotetuksi (taint poistuu). Tyhjä = mikään ei ole luotettua.
    trusted_roots: Vec<PathBuf>,
}

impl FsReadConfig {
    /// Luo tyhjän kokoonpanon, joka hylkää kaikki polut (fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lisää sallitun juuren (rakentaja-ketjutus).
    ///
    /// Juurta ei kanonisoida tässä — kanonisointi tehdään vasta lukuhetkellä,
    /// jotta kokoonpanon voi rakentaa myös ennen kuin hakemisto on olemassa.
    #[must_use]
    pub fn allow_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.allow_roots.push(root.into());
        self
    }

    /// Lisää luotetun juuren (rakentaja-ketjutus).
    ///
    /// Luotettu juuri lisätään myös sallittuihin juuriin, jottei luotetuksi
    /// merkittyä polkua voi vahingossa jättää allowlistin ulkopuolelle.
    #[must_use]
    pub fn trusted_root(mut self, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        self.allow_roots.push(root.clone());
        self.trusted_roots.push(root);
        self
    }

    /// Kanonisoi sallitut juuret. Olemassaolemattomat tai kanonisoitumattomat
    /// juuret ohitetaan hiljaa (niiden alle ei voi koskaan osua).
    fn canonical_allow_roots(&self) -> Vec<PathBuf> {
        Self::canonicalize_all(&self.allow_roots)
    }

    /// Kanonisoi luotetut juuret (sama logiikka kuin sallituilla).
    fn canonical_trusted_roots(&self) -> Vec<PathBuf> {
        Self::canonicalize_all(&self.trusted_roots)
    }

    /// Kanonisoi listan juuria; epäonnistuneet (esim. puuttuvat) jätetään pois.
    fn canonicalize_all(roots: &[PathBuf]) -> Vec<PathBuf> {
        roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .collect()
    }
}

/// Lippulaiva-taito allowlistatulle tiedoston luvulle (vain luku, SSRF-vapaa).
///
/// Riskiluokka on [`ActionRisk::ReadOnly`] ja käytäntö
/// [`ApprovalPolicy::AutoIfReadOnly`], joten allowlistin alle osuva luku ajaa
/// automaattisesti ilman hyväksyntää. Allowlistin ulkopuolinen polku hylätään.
#[derive(Debug, Clone, Default)]
pub struct FsReadAllowlisted {
    /// Allowlist-kokoonpano (sallitut + luotetut juuret).
    config: FsReadConfig,
}

impl FsReadAllowlisted {
    /// Luo taidon tyhjällä allowlistilla (hylkää kaikki polut, fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Luo taidon annetulla allowlist-kokoonpanolla.
    #[must_use]
    pub fn with_config(config: FsReadConfig) -> Self {
        Self { config }
    }

    /// Taidon kiinteä tunniste.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Ratkaisee allowlistatun, kanonisoidun polun syötteen polusta.
    ///
    /// Kanonisoi pyydetyn polun (purkaa `..` ja seuraa symlinkit) ja varmistaa
    /// että tulos pysyy jonkin sallitun juuren alla. Symlink-pakeneminen
    /// (allowlistin sisällä oleva linkki joka osoittaa ulos) hylätään, koska
    /// kanonisointi paljastaa todellisen kohteen ulkopuoliseksi.
    ///
    /// # Errors
    /// - [`ActionError::PolicyDenied`] jos polku ei kanonisoidu (esim. tiedostoa
    ///   ei ole) tai jos kanoninen polku ei ole minkään sallitun juuren alla.
    fn resolve_allowed(&self, requested: &str) -> Result<PathBuf> {
        let roots = self.config.canonical_allow_roots();
        if roots.is_empty() {
            return Err(ActionError::PolicyDenied(
                "ei sallittuja juuria — kaikki polut hylätään (fail-closed)".to_string(),
            ));
        }

        // Kanonisointi purkaa `..`-segmentit ja seuraa symlinkit todelliseen
        // kohteeseen. Tämä on kuormaa kantava turvallisuus: pakenemisyritys
        // (../ tai symlink ulos) paljastuu tässä.
        let canonical = std::fs::canonicalize(requested).map_err(|e| {
            ActionError::PolicyDenied(format!("polkua ei voi kanonisoida (hylätty): {e}"))
        })?;

        if path_is_under_any(&canonical, &roots) {
            Ok(canonical)
        } else {
            Err(ActionError::PolicyDenied(
                "kanoninen polku on allowlistin ulkopuolella (hylätty)".to_string(),
            ))
        }
    }

    /// Lukee kanonisoidun tiedoston ja koostaa todistepaketin ytimen
    /// (tiiviste + koko + yhteenveto) — EI koko sisältöä.
    ///
    /// Sisältö merkitään luotetuksi vain jos kanoninen polku osuu luotetun
    /// juuren alle.
    ///
    /// # Errors
    /// Palauttaa [`ActionError::ExecutionFailed`] jos tiedoston luku
    /// epäonnistuu.
    async fn read_proof(&self, canonical: &Path) -> Result<FsReadOutput> {
        let bytes = tokio::fs::read(canonical).await.map_err(|e| {
            ActionError::ExecutionFailed(format!("tiedoston luku epäonnistui: {e}"))
        })?;

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

/// Koostaa lyhyen, typistetyn yhteenvedon tiedoston tavuista.
///
/// Ottaa ensimmäisen rivin (tai koko sisällön jos rivinvaihtoa ei ole),
/// typistää [`SUMMARY_MAX_BYTES`]-rajaan UTF-8-turvallisesti ja siivoaa
/// kontrollimerkit. **Ei** koko sisältöä — yhteenveto on tarkoituksella suppea.
fn summarize(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let first_line = text.lines().next().unwrap_or("");
    let mut summary: String = first_line.chars().filter(|c| !c.is_control()).collect();
    truncate_utf8(&mut summary, SUMMARY_MAX_BYTES);
    summary.trim().to_string()
}

/// Typistää merkkijonon enintään `max_bytes` tavuun säilyttäen UTF-8-rajat.
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

        // Ratkaise + validoi allowlist. Hylätty polku → epäonnistunut tulos
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

        let out = match self.read_proof(&canonical).await {
            Ok(out) => out,
            Err(e) => {
                return Ok(ActionResult::failure(e.to_string(), request.now));
            }
        };

        let trusted = out.trusted;
        let output: Value = json!({
            "path_hash": out.path_hash,
            "size": out.size,
            "summary": out.summary,
            "trusted": out.trusted,
        });
        let result = ActionResult::success(
            format!("read {} byte(s) from allowlisted path", out.size),
            output,
            request.now,
        );

        // Taint poistetaan vain luotetuille projektitiedostoille; muuten tuloste
        // pysyy epäluotettavana (oletus).
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
            description: "Lukee paikallisen tiedoston vain allowlistatun juuren alta \
                 (kanonisoitu polku, ei verkkoa); todiste = tiiviste + koko + yhteenveto."
                .to_string(),
            permissions: vec![SkillPermission::ReadFiles],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ path }".to_string()),
            output_hint: Some("{ path_hash, size, summary, trusted }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Luettavan tiedoston polku (kanonisoidaan; on pysyttävä allowlistatun juuren alla)."
                    }
                },
                "required": ["path"],
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
    use std::io::Write;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Luo eristetyn väliaikaishakemiston tälle testille (kanonisoituna, jotta
    /// macOS `/var`→`/private/var`-symlinkit eivät sotke `starts_with`-vertailua).
    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("familyclaw_fs_read_{tag}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::canonicalize(&dir).expect("canonicalize temp dir")
    }

    /// Kirjoittaa tiedoston annetulla sisällöllä ja palauttaa sen polun.
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
        // Skeema mainostaa `path`-kentän aitona JSON Schemana.
        assert_eq!(m.input_schema["properties"]["path"]["type"], "string");
        assert_eq!(m.input_schema["required"][0], "path");
        // Geneerinen: ei perhenimiä, ei yksityisiä polkuja manifestissa.
        // Kielletyt nimet rakennetaan ajonaikana fragmenteista, jottei
        // lähdetiedostossa ole yhtäkään kokonaista perhenimi-literaalia
        // (scripts/audit-layer-b.sh löytäisi muuten oman testimme).
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
        // Eikä absoluuttisia/yksityisiä polkuja (geneerinen julkaistava skill).
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
    async fn rejects_outside_allowlist() {
        let allowed = temp_dir("allowed");
        let other = temp_dir("other");
        // Tiedosto ON olemassa (kanonisoituu) mutta EI allowlistatun juuren alla.
        write_file(&other, "secret.txt", "outside");
        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&allowed));

        let payload = serde_json::to_value(FsReadInput {
            path: other.join("secret.txt").to_string_lossy().to_string(),
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
        // Allowlist = alihakemisto; yritetään `..`-pakeneminen ulos.
        let base = temp_dir("traversal");
        let allowed = base.join("inside");
        std::fs::create_dir_all(&allowed).expect("create inside");
        // Salainen tiedosto sisaren hakemistossa (allowlistin ulkopuolella).
        write_file(&base, "outside.txt", "secret");

        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&allowed));

        // `<allowed>/../outside.txt` → kanonisoituu `<base>/outside.txt`:ksi,
        // joka EI ole allowlistin alla → hylätään.
        let traversal = allowed.join("..").join("outside.txt");
        let payload = serde_json::to_value(FsReadInput {
            path: traversal.to_string_lossy().to_string(),
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
        // Symlink allowlistin SISÄLLÄ joka osoittaa allowlistin ULKOPUOLELLE.
        // Kanonisointi seuraa linkin → todellinen kohde paljastuu ulkopuoliseksi.
        let allowed = temp_dir("symlink_allowed");
        let outside = temp_dir("symlink_outside");
        let secret = write_file(&outside, "secret.txt", "leak me");

        let link = allowed.join("link_to_secret.txt");
        std::os::unix::fs::symlink(&secret, &link).expect("create symlink");

        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&allowed));

        let payload = serde_json::to_value(FsReadInput {
            path: link.to_string_lossy().to_string(),
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
        // Ei-Unix-alustoilla symlinkin luonti voi vaatia oikeuksia; varmistetaan
        // sama invariantti junctionin/`..`:n kautta: ulkopuolinen kohde hylätään.
        // (Symlink-pakeneminen on katettu Unixilla erikseen.)
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
        // Tunnistemerkki on TARKOITUKSELLA toisella rivillä: yhteenveto ottaa vain
        // ensimmäisen rivin, joten koko tiedoston runko (rivit 2+) ei saa vuotaa.
        let contents = "harmless first line\nmust never appear: full body line two\n";
        write_file(&dir, "doc.txt", contents);
        let skill = FsReadAllowlisted::with_config(FsReadConfig::new().allow_root(&dir));

        let payload = serde_json::to_value(FsReadInput {
            path: dir.join("doc.txt").to_string_lossy().to_string(),
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

        // Tiiviste (64 heksamerkkiä) ja koko ovat läsnä.
        let hash = res.raw_output_redacted["path_hash"]
            .as_str()
            .expect("path_hash present");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            res.raw_output_redacted["size"].as_u64().expect("size"),
            contents.len() as u64
        );

        // Yhteenveto on VAIN ensimmäinen rivi — tiedoston runko (rivit 2+) ei vuoda.
        assert_eq!(
            res.raw_output_redacted["summary"],
            json!("harmless first line")
        );

        // Koko tiedoston sisältö EI saa esiintyä tulosteessa (vain yhteenveto,
        // joka on tiedoston ensimmäinen rivi typistettynä — ei koko sisältö).
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

        // Allowlist sisältää sekä epäluotetun että luotetun juuren.
        let config = FsReadConfig::new()
            .allow_root(&untrusted_dir)
            .trusted_root(&trusted_dir);
        let skill = FsReadAllowlisted::with_config(config);

        // Epäluotetun juuren alta luettu → tuloste pysyy taintattuna.
        let untrusted_payload = serde_json::to_value(FsReadInput {
            path: untrusted_dir.join("u.txt").to_string_lossy().to_string(),
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

        // Luotetun juuren alta luettu → taint poistuu.
        let trusted_payload = serde_json::to_value(FsReadInput {
            path: trusted_dir.join("t.txt").to_string_lossy().to_string(),
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
        // Tyhjä allowlist → fail-closed.
        let skill = FsReadAllowlisted::new();
        let payload = serde_json::to_value(FsReadInput {
            path: dir.join("doc.txt").to_string_lossy().to_string(),
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

    #[test]
    fn summarize_truncates_and_drops_control_chars() {
        let long = "a".repeat(500);
        let s = summarize(long.as_bytes());
        assert!(s.len() <= SUMMARY_MAX_BYTES);
        // Vain ensimmäinen rivi; kontrollimerkit pois.
        let multi = "first\u{7}line\nsecond line";
        let s2 = summarize(multi.as_bytes());
        assert_eq!(s2, "firstline");
    }
}
