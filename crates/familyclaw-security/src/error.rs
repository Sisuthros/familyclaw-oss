//! Turvakerroksen virhetyypit.
//!
//! Kaikki tämän craten epäonnistumiset kulkevat [`SecurityError`]-tyypin
//! kautta — **ei** `unwrap()`/`expect()`/`panic!()` tuotantopolulla. Tyyppi
//! muuntuu [`familyclaw_core::FamilyClawError`]:ksi [`From`]-toteutuksella,
//! jotta turvavirheet voivat kulkea alustan keskitetyn virhetyypin läpi.

use thiserror::Error;

use familyclaw_core::FamilyClawError;

/// Turvakerroksen virhetyyppi.
///
/// Kattaa identity-anchorien, tamper-tunnistuksen ja [`crate::HumanCorrection`]:n
/// virheluokat. `#[non_exhaustive]` jotta uusia variantteja voi lisätä
/// rikkomatta downstream-koodia.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SecurityError {
    /// Annettu sisältö oli kelvoton (esim. tyhjä SOUL-sisältö ankkurille).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Annettu hash-merkkijono ei ollut kelvollinen heksadesimaalinen
    /// SHA-256-tiiviste (väärä pituus tai ei-heksamerkkejä).
    #[error("invalid hash: {0}")]
    InvalidHash(String),

    /// JSON-sarjallistus tai -jäsennys epäonnistui.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl SecurityError {
    /// Rakentaa [`SecurityError::InvalidInput`]-variantin.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    /// Rakentaa [`SecurityError::InvalidHash`]-variantin.
    pub fn invalid_hash(msg: impl Into<String>) -> Self {
        Self::InvalidHash(msg.into())
    }
}

impl From<SecurityError> for FamilyClawError {
    fn from(err: SecurityError) -> Self {
        match err {
            // Säilytä serde luonnollisena alustan varianttina.
            SecurityError::Serde(serde) => FamilyClawError::Serde(serde),
            // Loput ovat syöte-/validointivirheitä.
            other => FamilyClawError::invalid_input(other.to_string()),
        }
    }
}

/// Turvacraten vakiotulostyyppi.
pub type Result<T> = std::result::Result<T, SecurityError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_input_constructor_formats() {
        let err = SecurityError::invalid_input("empty soul");
        assert_eq!(err.to_string(), "invalid input: empty soul");
    }

    #[test]
    fn invalid_hash_constructor_formats() {
        let err = SecurityError::invalid_hash("odd length");
        assert_eq!(err.to_string(), "invalid hash: odd length");
    }

    #[test]
    fn serde_converts_into_core_serde() {
        let parse = serde_json::from_str::<serde_json::Value>("{bad").expect_err("must fail");
        let sec: SecurityError = parse.into();
        let core: FamilyClawError = sec.into();
        assert!(matches!(core, FamilyClawError::Serde(_)));
    }

    #[test]
    fn invalid_input_converts_into_core_invalid_input() {
        let sec = SecurityError::invalid_input("boom");
        let core: FamilyClawError = sec.into();
        assert!(matches!(core, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn invalid_hash_converts_into_core_invalid_input() {
        let sec = SecurityError::invalid_hash("nope");
        let core: FamilyClawError = sec.into();
        assert!(matches!(core, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<SecurityError>();
    }
}
