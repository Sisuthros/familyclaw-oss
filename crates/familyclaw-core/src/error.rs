//! Virhetyypit koko FamilyClaw-alustalle.
//!
//! Yksi keskitetty virhetyyppi [`FamilyClawError`] kattaa kaikki kerrokset
//! (config, IO, sarjallistus, bus, muisti). Crateit voivat kääriä omat
//! virheensä tähän tai määritellä omat tyyppinsä jotka muuntuvat tähän
//! [`From`]-toteutuksilla. Tuotantopolulla EI käytetä `unwrap()`/`expect()`/
//! `panic!()` — kaikki virheet kulkevat [`Result`]-tyypin kautta.

use std::io;

use thiserror::Error;

/// FamilyClaw-alustan keskitetty virhetyyppi.
///
/// Jokainen variantti vastaa yhtä virheluokkaa jonka alusta voi kohdata.
/// Tyyppi on `#[non_exhaustive]` jotta uusia variantteja voi lisätä
/// myöhemmin rikkomatta downstream-koodia.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FamilyClawError {
    /// Konfiguraation lataus tai validointi epäonnistui.
    #[error("config error: {0}")]
    Config(String),

    /// Tiedosto- tai verkko-IO epäonnistui.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// JSON-sarjallistus tai -jäsennys epäonnistui.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Resonance Bus -tason virhe (actor-viestintä, kanavat, mailbox).
    #[error("bus error: {0}")]
    Bus(String),

    /// Muisti-substraatin virhe (Eternal Thread, vektorit, decay).
    #[error("memory error: {0}")]
    Memory(String),

    /// Pyydettyä resurssia (agentti, perhe, viesti) ei löytynyt.
    #[error("not found: {0}")]
    NotFound(String),

    /// Annettu syöte oli kelvoton (validointivirhe).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// LLM-pyyntö epäonnistui (verkko- tai API-virhe).
    #[error("llm error: {0}")]
    Llm(String),

    /// Sandbox-suoritus epäonnistui (WASM, fuel, capability).
    #[error("sandbox error: {0}")]
    Sandbox(String),
}

impl FamilyClawError {
    /// Rakentaa [`FamilyClawError::Config`]-variantin mistä tahansa
    /// merkkijonoksi muunnettavasta arvosta.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Rakentaa [`FamilyClawError::Bus`]-variantin.
    pub fn bus(msg: impl Into<String>) -> Self {
        Self::Bus(msg.into())
    }

    /// Rakentaa [`FamilyClawError::Memory`]-variantin.
    pub fn memory(msg: impl Into<String>) -> Self {
        Self::Memory(msg.into())
    }

    /// Rakentaa [`FamilyClawError::NotFound`]-variantin.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// Rakentaa [`FamilyClawError::InvalidInput`]-variantin.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    /// Rakentaa [`FamilyClawError::Llm`]-variantin.
    pub fn llm(msg: impl Into<String>) -> Self {
        Self::Llm(msg.into())
    }

    /// Rakentaa [`FamilyClawError::Sandbox`]-variantin.
    pub fn sandbox(msg: impl Into<String>) -> Self {
        Self::Sandbox(msg.into())
    }
}

/// Alustan vakiotulostyyppi: [`std::result::Result`] jonka virhe on
/// aina [`FamilyClawError`].
pub type Result<T> = std::result::Result<T, FamilyClawError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_constructor_sets_variant_and_message() {
        let err = FamilyClawError::config("missing key");
        assert!(matches!(err, FamilyClawError::Config(_)));
        assert_eq!(err.to_string(), "config error: missing key");
    }

    #[test]
    fn bus_memory_not_found_invalid_constructors() {
        assert_eq!(
            FamilyClawError::bus("mailbox closed").to_string(),
            "bus error: mailbox closed"
        );
        assert_eq!(
            FamilyClawError::memory("decay failed").to_string(),
            "memory error: decay failed"
        );
        assert_eq!(
            FamilyClawError::not_found("agent_x").to_string(),
            "not found: agent_x"
        );
        assert_eq!(
            FamilyClawError::invalid_input("empty name").to_string(),
            "invalid input: empty name"
        );
    }

    #[test]
    fn io_error_converts_via_from() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "no file");
        let err: FamilyClawError = io_err.into();
        assert!(matches!(err, FamilyClawError::Io(_)));
        assert!(err.to_string().starts_with("io error:"));
    }

    #[test]
    fn serde_error_converts_via_from() {
        let parse_err = serde_json::from_str::<serde_json::Value>("{ not json")
            .expect_err("malformed json must fail");
        let err: FamilyClawError = parse_err.into();
        assert!(matches!(err, FamilyClawError::Serde(_)));
        assert!(err.to_string().starts_with("serde error:"));
    }

    #[test]
    fn result_alias_is_usable() {
        fn maybe(fail: bool) -> Result<u8> {
            if fail {
                Err(FamilyClawError::config("boom"))
            } else {
                Ok(42)
            }
        }
        assert_eq!(maybe(false).expect("ok"), 42);
        assert!(maybe(true).is_err());
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<FamilyClawError>();
    }

    #[test]
    fn sandbox_constructor_sets_variant_and_message() {
        let err = FamilyClawError::sandbox("no wasmtime");
        assert!(matches!(err, FamilyClawError::Sandbox(_)));
        assert_eq!(err.to_string(), "sandbox error: no wasmtime");
    }
}
