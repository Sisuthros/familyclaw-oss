//! Tarkastusloki (audit log) sandboxin kyvykkyystarkistuksille ja
//! suoritusten elinkaarelle.
//!
//! Containment-vaatimus #5 (audit logging) edellyttää että **jokainen
//! kyvykkyystarkistus** (myönnetty / evätty) sekä **jokaisen suorituksen
//! alku ja loppu** kirjataan muuttumattomaan, sarjallistuvaan lokiin. 698:n
//! incidentin analyysi (2604.23425) osoitti että ilman tarkastuslokia
//! karkaamisia ei voi havaita jälkikäteen.
//!
//! ## Suunnitteluperiaate: ei riko olemassa olevaa rajapintaa
//! [`CapabilitySet`](crate::CapabilitySet):in julkiset metodit pysyvät
//! ennallaan (ne ovat puhtaita, sivuvaikutuksettomia kyselyitä). Tarkastus
//! kytketään **valinnaisena** [`AuditedCapabilities`]-näkymän kautta: se
//! kietoo viittauksen [`CapabilitySet`]:iin ja viittauksen [`AuditLog`]:iin,
//! ja tarjoaa samat tarkistusmetodit jotka **lisäksi** kirjaavat tuloksen.
//! Kutsuja joka ei tarvitse tarkastusta voi käyttää [`CapabilitySet`]:iä
//! suoraan kuten ennenkin.
//!
//! Loki on **append-only**: julkinen API ei tarjoa muokkausta eikä poistoa,
//! vain lisäystä ja lukua.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_sandbox::{AuditLog, AuditedCapabilities, Capability, CapabilitySet};
//!
//! let caps = CapabilitySet::deny_all().with(Capability::network("api.example.com"));
//! let mut log = AuditLog::new();
//! {
//!     let mut audited = AuditedCapabilities::new(&caps, &mut log);
//!     assert!(audited.allows_network_host("api.example.com")); // myönnetty
//!     assert!(!audited.allows_network_host("evil.example.com")); // evätty
//! }
//!
//! assert_eq!(log.len(), 2);
//! assert_eq!(log.denied_count(), 1);
//! ```

use serde::{Deserialize, Serialize};

use crate::capability::CapabilitySet;

/// Kyvykkyystarkistuksen kohde — mitä pääsyä koodi yritti käyttää.
///
/// Geneerinen ja sarjallistuva, jotta loki voidaan kirjata durable-storeen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "target")]
#[non_exhaustive]
pub enum CapabilityCheck {
    /// Verkkopääsy nimettyyn isäntään.
    Network(String),
    /// Lukupääsy tiedostopolkuun.
    ReadPath(String),
    /// Ympäristömuuttujan luku.
    EnvVar(String),
}

impl CapabilityCheck {
    /// Verkkotarkistus annettuun isäntään.
    pub fn network(host: impl Into<String>) -> Self {
        Self::Network(host.into())
    }

    /// Polkutarkistus annettuun polkuun.
    pub fn read_path(path: impl Into<String>) -> Self {
        Self::ReadPath(path.into())
    }

    /// Ympäristömuuttujatarkistus annetulle nimelle.
    pub fn env_var(name: impl Into<String>) -> Self {
        Self::EnvVar(name.into())
    }
}

/// Yksittäinen merkintä tarkastuslokissa.
///
/// `#[non_exhaustive]` jotta uusia tapahtumatyyppejä voi lisätä rikkomatta
/// downstream-koodia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
#[non_exhaustive]
pub enum AuditEntry {
    /// Suoritus alkoi — kirjaa backendin nimi ja koodin koko tavuina.
    ExecutionStart {
        /// Backendin tunniste (esim. `"noop"`, `"wasmtime"`).
        backend: String,
        /// Ajettavan koodin koko tavuina.
        code_len: usize,
    },

    /// Kyvykkyystarkistus tehtiin.
    CapabilityCheck {
        /// Mitä pääsyä yritettiin.
        check: CapabilityCheck,
        /// Myönnettiinkö pääsy (`true`) vai evättiinkö (`false`).
        granted: bool,
    },

    /// Suoritus päättyi — kirjaa onnistuiko se ja kulutettu polttoaine.
    ExecutionEnd {
        /// Päättyikö suoritus onnistuneesti.
        success: bool,
        /// Kulutettu polttoaine, jos tiedossa.
        fuel_consumed: Option<u64>,
    },
}

/// Append-only-tarkastusloki.
///
/// Kirjaa kyvykkyystarkistukset ja suoritusten elinkaaren. Julkinen API
/// sallii vain lisäyksen ja luvun — ei muokkausta eikä poistoa — joten loki
/// on muuttumaton todiste containment-vaatimus #5:n mukaisesti.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    /// Luo tyhjän lokin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lisää merkinnän lokin loppuun (append-only).
    pub fn record(&mut self, entry: AuditEntry) {
        self.entries.push(entry);
    }

    /// Kirjaa suorituksen alku.
    pub fn record_execution_start(&mut self, backend: impl Into<String>, code_len: usize) {
        self.record(AuditEntry::ExecutionStart {
            backend: backend.into(),
            code_len,
        });
    }

    /// Kirjaa suorituksen loppu.
    pub fn record_execution_end(&mut self, success: bool, fuel_consumed: Option<u64>) {
        self.record(AuditEntry::ExecutionEnd {
            success,
            fuel_consumed,
        });
    }

    /// Kirjaa kyvykkyystarkistus tuloksineen.
    pub fn record_capability_check(&mut self, check: CapabilityCheck, granted: bool) {
        self.record(AuditEntry::CapabilityCheck { check, granted });
    }

    /// Kaikki merkinnät lisäysjärjestyksessä.
    #[must_use]
    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// Merkintöjen lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Onko loki tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Evättyjen kyvykkyystarkistusten lukumäärä.
    #[must_use]
    pub fn denied_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, AuditEntry::CapabilityCheck { granted: false, .. }))
            .count()
    }

    /// Myönnettyjen kyvykkyystarkistusten lukumäärä.
    #[must_use]
    pub fn granted_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, AuditEntry::CapabilityCheck { granted: true, .. }))
            .count()
    }
}

/// Tarkastava näkymä [`CapabilitySet`]:iin.
///
/// Kietoo viittauksen kyvykkyysjoukkoon ja muuttuvan viittauksen
/// [`AuditLog`]:iin. Tarjoaa samat tarkistusmetodit kuin [`CapabilitySet`],
/// mutta **kirjaa jokaisen tarkistuksen** lokiin. Tämä on valinnainen koukku:
/// olemassa olevien tyyppien julkinen API ei muutu.
///
/// Elinaika `'a` sitoo molemmat lainat samaan kestoon: näkymä ei voi elää
/// joukkoa tai lokia pidempään.
#[derive(Debug)]
pub struct AuditedCapabilities<'a> {
    caps: &'a CapabilitySet,
    log: &'a mut AuditLog,
}

impl<'a> AuditedCapabilities<'a> {
    /// Rakentaa tarkastavan näkymän kyvykkyysjoukolle ja lokille.
    pub fn new(caps: &'a CapabilitySet, log: &'a mut AuditLog) -> Self {
        Self { caps, log }
    }

    /// Onko verkkopääsy isäntään myönnetty — tarkistus kirjataan.
    pub fn allows_network_host(&mut self, host: &str) -> bool {
        let granted = self.caps.allows_network_host(host);
        self.log
            .record_capability_check(CapabilityCheck::network(host), granted);
        granted
    }

    /// Onko lukupääsy polkuun myönnetty — tarkistus kirjataan.
    pub fn allows_read_path(&mut self, path: &str) -> bool {
        let granted = self.caps.allows_read_path(path);
        self.log
            .record_capability_check(CapabilityCheck::read_path(path), granted);
        granted
    }

    /// Onko ympäristömuuttujan luku sallittu — tarkistus kirjataan.
    pub fn allows_env_var(&mut self, name: &str) -> bool {
        let granted = self.caps.allows_env_var(name);
        self.log
            .record_capability_check(CapabilityCheck::env_var(name), granted);
        granted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;

    #[test]
    fn new_log_is_empty() {
        let log = AuditLog::new();
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
        assert_eq!(log.denied_count(), 0);
        assert_eq!(log.granted_count(), 0);
    }

    #[test]
    fn record_is_append_only_and_ordered() {
        let mut log = AuditLog::new();
        log.record_execution_start("noop", 4);
        log.record_capability_check(CapabilityCheck::network("h"), true);
        log.record_execution_end(true, Some(7));

        let entries = log.entries();
        assert_eq!(entries.len(), 3);
        assert!(matches!(entries[0], AuditEntry::ExecutionStart { .. }));
        assert!(matches!(entries[1], AuditEntry::CapabilityCheck { .. }));
        assert!(matches!(entries[2], AuditEntry::ExecutionEnd { .. }));
    }

    #[test]
    fn audited_view_records_denied_capability() {
        // Tyhjä joukko: kaikki tarkistukset evätään.
        let caps = CapabilitySet::deny_all();
        let mut log = AuditLog::new();
        {
            let mut audited = AuditedCapabilities::new(&caps, &mut log);
            assert!(!audited.allows_network_host("evil.example.com"));
        }
        assert_eq!(log.len(), 1);
        assert_eq!(log.denied_count(), 1);
        assert_eq!(log.granted_count(), 0);
        assert_eq!(
            log.entries()[0],
            AuditEntry::CapabilityCheck {
                check: CapabilityCheck::network("evil.example.com"),
                granted: false,
            }
        );
    }

    #[test]
    fn audited_view_records_granted_capability() {
        let caps = CapabilitySet::deny_all().with(Capability::network("api.example.com"));
        let mut log = AuditLog::new();
        {
            let mut audited = AuditedCapabilities::new(&caps, &mut log);
            assert!(audited.allows_network_host("api.example.com"));
        }
        assert_eq!(log.granted_count(), 1);
        assert_eq!(log.denied_count(), 0);
    }

    #[test]
    fn audited_view_records_each_check_type() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::read_only_fs("/data"))
            .with(Capability::env_var("HOME"));
        let mut log = AuditLog::new();
        {
            let mut audited = AuditedCapabilities::new(&caps, &mut log);
            assert!(audited.allows_read_path("/data/file"));
            assert!(!audited.allows_read_path("/secret"));
            assert!(audited.allows_env_var("HOME"));
            assert!(!audited.allows_env_var("SECRET_KEY"));
            assert!(!audited.allows_network_host("h"));
        }
        assert_eq!(log.len(), 5);
        assert_eq!(log.granted_count(), 2);
        assert_eq!(log.denied_count(), 3);
    }

    #[test]
    fn audit_log_serde_roundtrip() {
        let mut log = AuditLog::new();
        log.record_execution_start("noop", 4);
        log.record_capability_check(CapabilityCheck::read_path("/data"), false);
        log.record_execution_end(false, None);

        let json = serde_json::to_string(&log).expect("serialize");
        let back: AuditLog = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(log, back);
    }

    #[test]
    fn underlying_capability_set_api_unchanged() {
        // Tarkastus ei muuta CapabilitySet:in käyttäytymistä: suora kysely
        // antaa saman tuloksen kuin tarkastettu näkymä.
        let caps = CapabilitySet::deny_all().with(Capability::network("h"));
        let direct = caps.allows_network_host("h");
        let mut log = AuditLog::new();
        let audited_result = {
            let mut audited = AuditedCapabilities::new(&caps, &mut log);
            audited.allows_network_host("h")
        };
        assert_eq!(direct, audited_result);
    }
}
