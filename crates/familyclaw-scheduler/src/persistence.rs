//! Perhe-agency-tilan **config-persistenssi** (Phase 4, D5).
//!
//! Ajastettujen tehtävien kill-switch-tila ([`crate::ScheduledTask::enabled`])
//! pitää säilyä yli prosessin restartin: jos operaattori pysäytti tehtävän, sen
//! pitää pysyä pysäytettynä myös uudelleenkäynnistyksen jälkeen. Tämä moduuli
//! tallentaa tilan **erilliseen config-tiedostoon** (esim.
//! `<data_dir>/agency.json`), EI durable-replay-journaliin — roadmap D5 varoittaa
//! nimenomaan timer-/agency-tilan sekoittamisesta replay-substraattiin
//! (eri elinkaari, eri omistajuus).
//!
//! Muoto on tarkoituksella minimaalinen: vain **disabloitujen tehtävien
//! id-lista** UUID-merkkijonoina. Käyttöön otetut tehtävät ovat oletus, joten
//! niitä ei tarvitse listata → tiedosto pysyy pienenä ja diffattavana.
//!
//! ```json
//! { "disabled": ["d4ea3c1c-0000-4000-8000-647265616d63"] }
//! ```
//!
//! I/O on **synkronista** (`std::fs`): tilaa luetaan kerran bootissa ja
//! kirjoitetaan vain harvoissa operaattorimutaatioissa, joten async-overhead ei
//! ole perusteltu. Puuttuva tiedosto = tyhjä config (ei virhe) — ensikäynnistys.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::task::ScheduledTaskId;

/// Persistoitu perhe-agency-tila: mitkä ajastetut tehtävät on otettu pois
/// käytöstä (kill-switch). Käyttöön otetut ovat oletus eikä niitä listata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgencyConfig {
    /// Pois käytöstä otettujen tehtävien tunnisteet UUID-merkkijonoina.
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl AgencyConfig {
    /// Lataa configin tiedostosta. **Puuttuva tiedosto = tyhjä config** (ei
    /// virhe) — tämä on normaali ensikäynnistys.
    ///
    /// # Errors
    /// [`io::Error`] jos tiedosto on olemassa mutta sitä ei voi lukea, tai
    /// [`serde_json`] -jäsennysvirhe käärittynä `io::Error`:iin
    /// ([`io::ErrorKind::InvalidData`]) jos sisältö ei ole kelvollista `JSON`ia.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Tallentaa configin tiedostoon (atominen: kirjoita temp + rename, jottei
    /// keskeytynyt kirjoitus jätä rikkinäistä tiedostoa).
    ///
    /// # Errors
    /// [`io::Error`] jos hakemiston luonti, kirjoitus tai uudelleennimeäminen
    /// epäonnistuu.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Atominen vaihto: kirjoita viereiseen temp-tiedostoon ja rename päälle.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Onko annettu tehtävä merkitty pois käytöstä tässä configissa.
    #[must_use]
    pub fn is_disabled(&self, id: ScheduledTaskId) -> bool {
        let id_str = id.to_string();
        self.disabled.iter().any(|d| d == &id_str)
    }

    /// Merkitsee tehtävän pois käytöstä / käyttöön configissa (idempotentti).
    ///
    /// `enabled = false` lisää id:n disabled-listaan (jos puuttuu); `true`
    /// poistaa sen. Ei kirjoita tiedostoon — kutsu [`save`](Self::save) erikseen.
    pub fn set(&mut self, id: ScheduledTaskId, enabled: bool) {
        let id_str = id.to_string();
        if enabled {
            self.disabled.retain(|d| d != &id_str);
        } else if !self.disabled.iter().any(|d| d == &id_str) {
            self.disabled.push(id_str);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> ScheduledTaskId {
        ScheduledTaskId::from_uuid(Uuid::from_u128(n))
    }

    #[test]
    fn default_is_empty() {
        assert!(AgencyConfig::default().disabled.is_empty());
    }

    #[test]
    fn set_toggles_idempotently() {
        let mut cfg = AgencyConfig::default();
        cfg.set(id(1), false);
        assert!(cfg.is_disabled(id(1)));
        // Toista disablointi → ei duplikaattia.
        cfg.set(id(1), false);
        assert_eq!(cfg.disabled.len(), 1);
        // Käyttöön otto poistaa.
        cfg.set(id(1), true);
        assert!(!cfg.is_disabled(id(1)));
        assert!(cfg.disabled.is_empty());
        // Käyttöön otto tuntemattomalle → no-op.
        cfg.set(id(2), true);
        assert!(cfg.disabled.is_empty());
    }

    #[test]
    fn load_missing_file_is_empty_not_error() {
        let path = std::env::temp_dir().join("familyclaw-agency-does-not-exist-xyz.json");
        let _ = std::fs::remove_file(&path);
        let cfg = AgencyConfig::load(&path).expect("missing file → default");
        assert!(cfg.disabled.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join("familyclaw-agency-rt-test");
        let path = dir.join("agency.json");
        let _ = std::fs::remove_file(&path);

        let mut cfg = AgencyConfig::default();
        cfg.set(id(7), false);
        cfg.set(id(9), false);
        cfg.save(&path).expect("save");

        let loaded = AgencyConfig::load(&path).expect("load");
        assert!(loaded.is_disabled(id(7)));
        assert!(loaded.is_disabled(id(9)));
        assert!(!loaded.is_disabled(id(1)));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_invalid_json_is_error() {
        let path = std::env::temp_dir().join("familyclaw-agency-bad.json");
        std::fs::write(&path, b"not json{{{").expect("write");
        let err = AgencyConfig::load(&path).expect_err("invalid → error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&path);
    }
}
