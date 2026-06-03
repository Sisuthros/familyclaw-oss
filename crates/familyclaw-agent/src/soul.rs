//! SOUL-lataus: geneerinen profiili-skeema (KERROS A, OSS).
//!
//! Tämä moduuli lataa olennon **identiteettiprofiilin** ajonaikaisesti
//! geneerisestä hakemistosta (`FAMILYCLAW_PROFILE_DIR`, design §1). Se EI
//! kovakoodaa yhdenkään perheenjäsenen sielua — se määrittää vain *muodon*,
//! jonka kuka tahansa voi täyttää omalle perheelleen.
//!
//! ## OSS-raja (KERROS A)
//! - Profiilin sisältö (SOUL.md, IDENTITY.md, WANTS.md) on KERROS B:tä ja
//!   ladataan ajonaikaisesti — sitä ei koskaan kovakoodata tähän repoon.
//! - Esimerkit käyttävät geneerisiä nimiä (`agent_a`, `agent_b`).
//!
//! ## Skeema
//! Profiilihakemisto on yksinkertainen: Markdown-tiedostoja, joiden
//! perusnimi (ilman päätettä) on osa-alueen avain. Tunnetut osat:
//!
//! | Tiedosto | Kenttä | Merkitys |
//! |----------|--------|----------|
//! | `SOUL.md` | [`Soul::essence`] | Olennon ydinkuvaus (kuka se on). |
//! | `IDENTITY.md` | [`Soul::identity`] | Pysyvät totuudet (ankkuroitava). |
//! | `WANTS.md` | [`Soul::wants`] | Olennon omat halut/tavoitteet. |
//!
//! Lisätiedostot säilytetään [`Soul::extra`]-kartassa avaimella =
//! perustiedostonimi pienennettynä. Vain `SOUL.md` on pakollinen.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use familyclaw_core::{FamilyClawError, Result};
use serde::{Deserialize, Serialize};

/// Ympäristömuuttuja, joka osoittaa profiilihakemistojen juureen.
///
/// Sama idea kuin Hermesin `HERMES_HOME` (design §1): alusta on geneerinen,
/// ja konkreettiset profiilit elävät tämän muuttujan osoittamassa paikassa
/// — eivät repossa.
pub const PROFILE_DIR_ENV: &str = "FAMILYCLAW_PROFILE_DIR";

/// Pakollisen ydintiedoston nimi.
const SOUL_FILE: &str = "SOUL.md";
/// Pysyvien totuuksien tiedoston nimi.
const IDENTITY_FILE: &str = "IDENTITY.md";
/// Halujen tiedoston nimi.
const WANTS_FILE: &str = "WANTS.md";

/// Olennon ladattu identiteettiprofiili (KERROS B -sisältö ajonaikaisesti).
///
/// `Soul` on **dataa**, ei käyttäytymistä: se kantaa profiilihakemistosta
/// luetut tekstit. Se on `serde`-sarjallistuva, jotta sen voi liittää
/// muistoon tai lähettää busin yli ilman lisämuunnoksia.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Soul {
    /// Ydinkuvaus (`SOUL.md`). Pakollinen — tyhjä sielu ei ole sielu.
    pub essence: String,

    /// Pysyvät totuudet (`IDENTITY.md`), jos annettu. Tämä on luonteva
    /// ankkuroitavaksi `familyclaw-security`-kerroksessa (λ=0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,

    /// Olennon omat halut/tavoitteet (`WANTS.md`), jos annettu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wants: Option<String>,

    /// Muut profiilitiedostot avaimella = perustiedostonimi pienennettynä
    /// (esim. `family` tiedostolle `FAMILY.md`). Mahdollistaa
    /// laajennukset rikkomatta skeemaa.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl Soul {
    /// Rakentaa sielun pelkästä ytimestä (ilman levyä). Käytännöllinen
    /// testeissä ja paljaalle rungolle.
    #[must_use]
    pub fn from_essence(essence: impl Into<String>) -> Self {
        Self {
            essence: essence.into(),
            ..Self::default()
        }
    }

    /// Onko sielu tyhjä (ei ydintä). Tyhjää sielua ei pidä ankkuroida.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.essence.trim().is_empty()
    }

    /// Lyhyt yhteenveto ankkurointia/lokitusta varten: ydin + identiteetti
    /// yhdistettynä. Tämä on se sisältö, jonka tiivisteen
    /// `familyclaw-security` ankkuroi tamper-vahdiksi.
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

/// Lukee yhden valinnaisen Markdown-tiedoston profiilihakemistosta.
///
/// Palauttaa `Ok(None)` jos tiedostoa ei ole, `Ok(Some(_))` jos se luettiin,
/// ja virheen vain todellisesta IO-ongelmasta (esim. lukuoikeus).
fn read_optional(dir: &Path, file: &str) -> Result<Option<String>> {
    let path = dir.join(file);
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(FamilyClawError::Io(err)),
    }
}

/// Lataa olennon sielun annetusta profiilihakemistosta.
///
/// `SOUL.md` on pakollinen; `IDENTITY.md` ja `WANTS.md` ovat valinnaisia.
/// Kaikki muut `*.md`-tiedostot luetaan [`Soul::extra`]-karttaan.
///
/// # Errors
/// - [`FamilyClawError::NotFound`] jos hakemistoa ei ole tai pakollinen
///   `SOUL.md` puuttuu (tai on tyhjä).
/// - [`FamilyClawError::Io`] jos tiedoston luku epäonnistuu muusta syystä.
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

    // Lue muut .md-tiedostot extra-karttaan (deterministinen järjestys
    // BTreeMapin ansiosta).
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
        // Avain = perustiedostonimi ilman päätettä, pienennettynä.
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

/// Ratkaisee yksittäisen agentin profiilihakemiston.
///
/// Etusija:
/// 1. eksplisiittinen `configured` (agentin `profile_dir`),
/// 2. `FAMILYCLAW_PROFILE_DIR/<agent_name>` jos ympäristömuuttuja on asetettu.
///
/// Palauttaa `None` jos kumpaakaan ei ole — silloin agentti ajaa paljaalla
/// rungolla ilman sielua (täysin kalibroimaton).
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

    /// Apuri: luo uniikki väliaikainen profiilihakemisto.
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
        // Eksplisiittinen polku ei kosketa ympäristömuuttujaa, joten tämä
        // testi on turvallinen ajaa rinnakkain env-testin kanssa.
        let explicit = PathBuf::from("explicit/agent_a");
        let resolved = resolve_profile_dir(Some(&explicit), "agent_a");
        assert_eq!(resolved, Some(explicit));
    }

    /// Yksi testi koko env-pohjaiselle reitille (asetettu + asettamaton),
    /// jotta rinnakkaiset testit eivät mutatoi samaa prosessin globaalia
    /// ympäristömuuttujaa ristiin (`set_var` ei ole säieturvallinen).
    #[test]
    fn resolve_profile_dir_env_fallback_and_unset() {
        let root = std::env::temp_dir().join("familyclaw-profiles-root");

        // 1. Env asetettuna → root/<agent_name>. (Edition 2021: set_var on
        // turvallinen funktio; tämä on ainoa testi joka mutatoi muuttujaa.)
        std::env::set_var(PROFILE_DIR_ENV, &root);
        let resolved = resolve_profile_dir(None, "agent_b");
        assert_eq!(resolved, Some(root.join("agent_b")));

        // 2. Env poistettuna → None (kun ei eksplisiittistä polkua).
        std::env::remove_var(PROFILE_DIR_ENV);
        assert!(resolve_profile_dir(None, "agent_c").is_none());
    }
}
