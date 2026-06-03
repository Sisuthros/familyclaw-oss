//! Kyvykkyysmalli (capability model) sandboxille.
//!
//! Sandboxissa ajettava koodi saa **vain ne oikeudet jotka sille
//! eksplisiittisesti myönnetään**. Oletuksena koodilla ei ole verkkoa eikä
//! tiedostojärjestelmäpääsyä — "deny by default". Tämä on KERROS A:n
//! turvaperiaate (design §2 turva): epäluotettu koodi ei pääse käsiksi
//! verkkoon, salaisuuksiin tai mielivaltaisiin polkuihin.
//!
//! Malli on tarkoituksella **deklaratiivinen tieto** — varsinainen
//! pakottaminen tapahtuu ajonaikaisessa backendissä (esim. wasmtime-WASI).
//! Tämä erottelu pitää kyvykkyyslogiikan testattavana ilman raskasta
//! wasmtime-riippuvuutta.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Yksittäinen kyvykkyys jonka sandboxissa ajettava koodi voi saada.
///
/// Kyvykkyydet ovat additiivisia: koodi saa täsmälleen ne joukot jotka
/// [`CapabilitySet`]:iin on lisätty, ei mitään muuta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum Capability {
    /// Lukuoikeus yhteen tiedostojärjestelmäpolkuun (ja sen alipuuhun).
    ///
    /// Polku tulkitaan etuliitteenä: pääsy sallitaan tiedostoon `p` jos
    /// jokin myönnetty `ReadOnlyFs`-polku on `p`:n esi-isä tai sama polku.
    ReadOnlyFs {
        /// Sallittu juuripolku (etuliite).
        path: PathBuf,
    },

    /// Verkkopääsy nimettyyn isäntään (host). Oletuksena verkkoa EI ole;
    /// tämä on harkittu poikkeus. Tyhjä `host` ei ole sallittu.
    Network {
        /// Sallittu isäntänimi tai osoite (esim. `"api.example.com"`).
        host: String,
    },

    /// Ympäristömuuttujan lukuoikeus nimen perusteella.
    EnvVar {
        /// Sallitun ympäristömuuttujan nimi.
        name: String,
    },
}

impl Capability {
    /// Rakentaa [`Capability::ReadOnlyFs`]-kyvyn annetusta polusta.
    #[must_use]
    pub fn read_only_fs(path: impl Into<PathBuf>) -> Self {
        Self::ReadOnlyFs { path: path.into() }
    }

    /// Rakentaa [`Capability::Network`]-kyvyn annetusta isännästä.
    #[must_use]
    pub fn network(host: impl Into<String>) -> Self {
        Self::Network { host: host.into() }
    }

    /// Rakentaa [`Capability::EnvVar`]-kyvyn annetusta muuttujanimestä.
    #[must_use]
    pub fn env_var(name: impl Into<String>) -> Self {
        Self::EnvVar { name: name.into() }
    }

    /// Onko tämä kyvykkyys hyvinmuodostettu (ei tyhjiä pakollisia kenttiä).
    ///
    /// Käytetään [`CapabilitySet::validate`]:ssa. Erityisesti tyhjä host /
    /// env-nimi tai tyhjä polku katsotaan virheelliseksi, koska ne johtaisivat
    /// epämääräiseen tai liian laajaan pääsyyn.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::ReadOnlyFs { path } => !path.as_os_str().is_empty(),
            Self::Network { host } => !host.trim().is_empty(),
            Self::EnvVar { name } => !name.trim().is_empty(),
        }
    }
}

/// Joukko kyvykkyyksiä jotka sandbox myöntää ajettavalle koodille.
///
/// Oletuksena ([`CapabilitySet::deny_all`] / [`Default`]) joukko on tyhjä:
/// ei verkkoa, ei tiedostoja, ei ympäristömuuttujia. Kyvykkyyksiä lisätään
/// eksplisiittisesti builder-tyylillä.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet {
    /// Myönnetyt kyvykkyydet. Järjestys ei ole merkitsevä.
    capabilities: Vec<Capability>,
}

impl CapabilitySet {
    /// Tyhjä joukko — "deny all". Tämä on turvallinen oletus.
    #[must_use]
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Rakentaa joukon valmiista kyvykkyyksien iteraattorista.
    pub fn from_iter_caps(caps: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            capabilities: caps.into_iter().collect(),
        }
    }

    /// Lisää kyvykkyyden (builder-tyyli). Duplikaatteja ei lisätä uudelleen.
    #[must_use]
    pub fn with(mut self, capability: Capability) -> Self {
        self.grant(capability);
        self
    }

    /// Lisää kyvykkyyden paikan päällä. Jos identtinen kyvykkyys on jo
    /// joukossa, ei lisätä duplikaattia.
    pub fn grant(&mut self, capability: Capability) {
        if !self.capabilities.contains(&capability) {
            self.capabilities.push(capability);
        }
    }

    /// Onko joukko tyhjä (täysi "deny all").
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }

    /// Myönnettyjen kyvykkyyksien lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    /// Iteroi myönnetyt kyvykkyydet.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.capabilities.iter()
    }

    /// Onko verkkopääsyä myönnetty lainkaan.
    #[must_use]
    pub fn allows_any_network(&self) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, Capability::Network { .. }))
    }

    /// Onko verkkopääsy annettuun isäntään myönnetty.
    #[must_use]
    pub fn allows_network_host(&self, host: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, Capability::Network { host: h } if h == host))
    }

    /// Onko lukupääsy annettuun polkuun myönnetty.
    ///
    /// Pääsy sallitaan jos jokin myönnetty [`Capability::ReadOnlyFs`]-polku on
    /// pyydetyn polun etuliite (esi-isä tai sama polku). Vertailu tehdään
    /// komponenttitasolla, joten `/data` EI salli `/data2`:ta vaikka se onkin
    /// merkkijono-etuliite.
    #[must_use]
    pub fn allows_read_path(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        self.capabilities.iter().any(|c| match c {
            Capability::ReadOnlyFs { path: root } => path_is_within(root, path),
            _ => false,
        })
    }

    /// Onko annettu ympäristömuuttuja sallittu luettavaksi.
    #[must_use]
    pub fn allows_env_var(&self, name: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, Capability::EnvVar { name: n } if n == name))
    }

    /// Validoi koko kyvykkyysjoukon.
    ///
    /// # Errors
    /// [`crate::SandboxError::Capability`] jos jokin kyvykkyys on huonosti
    /// muodostettu (esim. tyhjä host, tyhjä polku tai tyhjä env-nimi).
    pub fn validate(&self) -> crate::Result<()> {
        for cap in &self.capabilities {
            if !cap.is_well_formed() {
                return Err(crate::SandboxError::capability(format!(
                    "malformed capability: {cap:?}"
                )));
            }
        }
        Ok(())
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self::from_iter_caps(iter)
    }
}

impl<'a> IntoIterator for &'a CapabilitySet {
    type Item = &'a Capability;
    type IntoIter = std::slice::Iter<'a, Capability>;

    fn into_iter(self) -> Self::IntoIter {
        self.capabilities.iter()
    }
}

/// Onko `candidate` polku `root`:in alla (sama tai alipuu), komponenttitasolla.
///
/// Komponenttivertailu estää väärät osumat kuten `/data` vs `/data2`.
fn path_is_within(root: &Path, candidate: &Path) -> bool {
    let mut root_components = root.components();
    let mut cand_components = candidate.components();
    loop {
        match root_components.next() {
            // Root loppui ilman eroavaisuutta → candidate on rootin alla.
            None => return true,
            Some(rc) => match cand_components.next() {
                // Candidate loppui ennen rootia → ei voi olla sen alla.
                None => return false,
                Some(cc) => {
                    if rc != cc {
                        return false;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_all_is_empty_and_denies_everything() {
        let caps = CapabilitySet::deny_all();
        assert!(caps.is_empty());
        assert_eq!(caps.len(), 0);
        assert!(!caps.allows_any_network());
        assert!(!caps.allows_network_host("example.com"));
        assert!(!caps.allows_read_path("/etc/passwd"));
        assert!(!caps.allows_env_var("PATH"));
    }

    #[test]
    fn default_equals_deny_all() {
        assert_eq!(CapabilitySet::default(), CapabilitySet::deny_all());
    }

    #[test]
    fn grant_network_host_is_specific() {
        let caps = CapabilitySet::deny_all().with(Capability::network("api.example.com"));
        assert!(caps.allows_any_network());
        assert!(caps.allows_network_host("api.example.com"));
        assert!(!caps.allows_network_host("evil.example.com"));
    }

    #[test]
    fn grant_is_idempotent() {
        let mut caps = CapabilitySet::deny_all();
        caps.grant(Capability::network("h"));
        caps.grant(Capability::network("h"));
        assert_eq!(caps.len(), 1);
    }

    #[test]
    fn read_path_prefix_matching_component_level() {
        let caps = CapabilitySet::deny_all().with(Capability::read_only_fs("/data"));
        assert!(caps.allows_read_path("/data"));
        assert!(caps.allows_read_path("/data/file.txt"));
        assert!(caps.allows_read_path("/data/nested/deep.bin"));
        // Komponenttitaso: /data2 EI ole /data:n alla vaikka merkkijonona on.
        assert!(!caps.allows_read_path("/data2"));
        assert!(!caps.allows_read_path("/data2/secret"));
        // Eri juuri ei salli.
        assert!(!caps.allows_read_path("/etc/passwd"));
        // Lyhyempi polku ei ole rootin alla.
        assert!(!caps.allows_read_path("/"));
    }

    #[test]
    fn multiple_read_roots_each_allow_their_subtree() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::read_only_fs("/a"))
            .with(Capability::read_only_fs("/b/c"));
        assert!(caps.allows_read_path("/a/x"));
        assert!(caps.allows_read_path("/b/c/y"));
        assert!(!caps.allows_read_path("/b/d"));
    }

    #[test]
    fn env_var_is_specific() {
        let caps = CapabilitySet::deny_all().with(Capability::env_var("HOME"));
        assert!(caps.allows_env_var("HOME"));
        assert!(!caps.allows_env_var("SECRET_KEY"));
    }

    #[test]
    fn well_formed_checks_reject_empty() {
        assert!(!Capability::read_only_fs("").is_well_formed());
        assert!(!Capability::network("   ").is_well_formed());
        assert!(!Capability::env_var("").is_well_formed());
        assert!(Capability::read_only_fs("/x").is_well_formed());
        assert!(Capability::network("h").is_well_formed());
        assert!(Capability::env_var("X").is_well_formed());
    }

    #[test]
    fn validate_rejects_malformed_capability() {
        let caps = CapabilitySet::deny_all().with(Capability::network("   "));
        let err = caps.validate().expect_err("blank host must fail");
        assert!(err.to_string().contains("malformed capability"));
    }

    #[test]
    fn validate_accepts_well_formed_set() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::read_only_fs("/data"))
            .with(Capability::network("api.example.com"))
            .with(Capability::env_var("HOME"));
        assert!(caps.validate().is_ok());
    }

    #[test]
    fn from_iter_collects_capabilities() {
        let caps: CapabilitySet = vec![
            Capability::network("a"),
            Capability::read_only_fs("/r"),
        ]
        .into_iter()
        .collect();
        assert_eq!(caps.len(), 2);
        assert!(caps.allows_network_host("a"));
        assert!(caps.allows_read_path("/r/file"));
    }

    #[test]
    fn iter_yields_all_capabilities() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::network("a"))
            .with(Capability::env_var("X"));
        let count = caps.iter().count();
        assert_eq!(count, 2);
        let ref_count = (&caps).into_iter().count();
        assert_eq!(ref_count, 2);
    }

    #[test]
    fn serde_roundtrip_preserves_set() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::read_only_fs("/data"))
            .with(Capability::network("h"))
            .with(Capability::env_var("HOME"));
        let json = serde_json::to_string(&caps).expect("serialize");
        let back: CapabilitySet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(caps, back);
    }
}
