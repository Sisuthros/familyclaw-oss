//! Durable-substraatin virhetyypit.
//!
//! Kaikki tämän craten epäonnistumiset kulkevat [`DurableError`]-tyypin
//! kautta — **ei** `unwrap()`/`expect()`/`panic!()` tuotantopolulla. Tyyppi
//! muuntuu [`familyclaw_core::FamilyClawError`]:ksi [`From`]-toteutuksella,
//! jotta durable-virheet voivat kulkea alustan keskitetyn virhetyypin läpi.

use thiserror::Error;

use familyclaw_core::FamilyClawError;

/// Durable-substraatin virhetyyppi.
///
/// `#[non_exhaustive]` jotta uusia variantteja voi lisätä rikkomatta
/// downstream-koodia.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DurableError {
    /// Journalin taustatallennuksen IO epäonnistui (avaus, kirjoitus, fsync,
    /// luku).
    #[error("journal io error: {0}")]
    Io(#[from] std::io::Error),

    /// Journal-rivin sarjallistus tai jäsennys epäonnistui.
    #[error("journal serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Replay havaitsi epädeterminismin: tallennettu askel ei vastaa
    /// nyt suoritettavaa askelta (esim. eri nimi samalla sekvenssipaikalla).
    ///
    /// Tämä on durable-execution-mallin kovin invariantti: koodin täytyy
    /// tuottaa samat askeleet samassa järjestyksessä joka ajolla.
    #[error(
        "nondeterministic replay at step #{index}: expected step {expected:?}, found {found:?}"
    )]
    NondeterministicReplay {
        /// Askeleen sekvenssijärjestysnumero (0-pohjainen).
        index: u64,
        /// Replay-koodin tällä paikalla odottama askeleen nimi.
        expected: String,
        /// Journalista tällä paikalla löytynyt askeleen nimi.
        found: String,
    },

    /// Journalin rivi oli vioittunut eikä sitä voitu jäsentää merkitykselliseksi
    /// entryksi (esim. typistynyt JSONL-rivi kaatumisen jäljiltä).
    #[error("corrupt journal entry at line {line}: {reason}")]
    CorruptEntry {
        /// 1-pohjainen rivinumero taustatiedostossa.
        line: u64,
        /// Ihmisluettava syy miksi rivi hylättiin.
        reason: String,
    },

    /// Askeleen sisällä ajettu suljin palautti virheen. Virhe säilytetään
    /// merkkijonona, koska durable-loki tallentaa virhetuloksen tekstinä.
    #[error("step '{step}' failed: {message}")]
    StepFailed {
        /// Askeleen looginen nimi.
        step: String,
        /// Sulkimen palauttama virheviesti.
        message: String,
    },

    /// Timeline-haarautus (fork) epäonnistui — esim. leikkauspiste on lokin
    /// askelmäärän ulkopuolella tai kohdejournal ei ollut tyhjä.
    ///
    /// Fork on **fail-closed**: epäselvässä tilanteessa haarautus kieltäytyy
    /// sen sijaan että tuottaisi hiljaa vääränmuotoisen aikajanan.
    #[error("invalid timeline fork: {reason}")]
    InvalidFork {
        /// Ihmisluettava syy miksi haarautus hylättiin.
        reason: String,
    },
}

impl DurableError {
    /// Rakentaa [`DurableError::StepFailed`]-variantin.
    pub fn step_failed(step: impl Into<String>, message: impl Into<String>) -> Self {
        Self::StepFailed {
            step: step.into(),
            message: message.into(),
        }
    }

    /// Rakentaa [`DurableError::CorruptEntry`]-variantin.
    pub fn corrupt(line: u64, reason: impl Into<String>) -> Self {
        Self::CorruptEntry {
            line,
            reason: reason.into(),
        }
    }

    /// Rakentaa [`DurableError::InvalidFork`]-variantin.
    pub fn invalid_fork(reason: impl Into<String>) -> Self {
        Self::InvalidFork {
            reason: reason.into(),
        }
    }
}

impl From<DurableError> for FamilyClawError {
    fn from(err: DurableError) -> Self {
        match err {
            // Säilytä IO/serde luonnollisina alustan variantteina.
            DurableError::Io(io) => FamilyClawError::Io(io),
            DurableError::Serde(serde) => FamilyClawError::Serde(serde),
            // Loput kuvataan muisti-kerroksen virheiksi (durable = muistin
            // substraatti) säilyttäen ihmisluettava viesti.
            other => FamilyClawError::memory(other.to_string()),
        }
    }
}

/// Durable-craten vakiotulostyyppi.
pub type Result<T> = std::result::Result<T, DurableError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_failed_constructor_formats() {
        let err = DurableError::step_failed("render", "out of memory");
        assert_eq!(err.to_string(), "step 'render' failed: out of memory");
    }

    #[test]
    fn corrupt_constructor_formats() {
        let err = DurableError::corrupt(7, "truncated json");
        assert_eq!(
            err.to_string(),
            "corrupt journal entry at line 7: truncated json"
        );
    }

    #[test]
    fn nondeterministic_replay_formats() {
        let err = DurableError::NondeterministicReplay {
            index: 2,
            expected: "b".to_string(),
            found: "c".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "nondeterministic replay at step #2: expected step \"b\", found \"c\""
        );
    }

    #[test]
    fn io_converts_into_core_io() {
        let io = std::io::Error::other("disk full");
        let durable: DurableError = io.into();
        let core: FamilyClawError = durable.into();
        assert!(matches!(core, FamilyClawError::Io(_)));
    }

    #[test]
    fn serde_converts_into_core_serde() {
        let parse = serde_json::from_str::<serde_json::Value>("{bad").expect_err("must fail");
        let durable: DurableError = parse.into();
        let core: FamilyClawError = durable.into();
        assert!(matches!(core, FamilyClawError::Serde(_)));
    }

    #[test]
    fn non_io_converts_into_core_memory() {
        let durable = DurableError::step_failed("s", "boom");
        let core: FamilyClawError = durable.into();
        assert!(matches!(core, FamilyClawError::Memory(_)));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<DurableError>();
    }
}
