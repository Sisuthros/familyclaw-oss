//! Benchmark-harnessin virhetyypit.
//!
//! Kaikki harnessin epäonnistumiset kulkevat [`BenchError`]- ja [`Result`]-
//! tyyppien kautta — tuotantopolulla EI käytetä `unwrap()`/`expect()`/
//! `panic!()`. [`BenchError`] kääräisee alustan ydincraten
//! [`familyclaw_core::FamilyClawError`]:n sekä durable-/dream-tason virheet,
//! jotta skenaariot voivat propagoida `?`-operaattorilla.

use thiserror::Error;

/// Benchmark-harnessin keskitetty virhetyyppi.
///
/// `#[non_exhaustive]` jotta uusia variantteja voi lisätä myöhemmin rikkomatta
/// downstream-skenaarioita.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BenchError {
    /// Subject (benchmarkattava järjestelmä) epäonnistui operaatiossa.
    #[error("subject error: {0}")]
    Subject(String),

    /// Skenaarion suoritus epäonnistui.
    #[error("scenario error: {0}")]
    Scenario(String),

    /// Mittarin laskenta sai kelvottoman syötteen.
    #[error("metric error: {0}")]
    Metric(String),

    /// Scorecardin sarjallistus tai kirjoitus epäonnistui.
    #[error("scorecard error: {0}")]
    Scorecard(String),

    /// Alustan ydinvirhe (config, IO, muisti, bus).
    #[error("core error: {0}")]
    Core(#[from] familyclaw_core::FamilyClawError),

    /// Durable-substraatin virhe (journal, replay).
    #[error("durable error: {0}")]
    Durable(#[from] familyclaw_durable::DurableError),

    /// JSON-sarjallistus tai -jäsennys epäonnistui.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Tiedosto- tai prosessi-IO epäonnistui.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl BenchError {
    /// Rakentaa [`BenchError::Subject`]-variantin.
    pub fn subject(msg: impl Into<String>) -> Self {
        Self::Subject(msg.into())
    }

    /// Rakentaa [`BenchError::Scenario`]-variantin.
    pub fn scenario(msg: impl Into<String>) -> Self {
        Self::Scenario(msg.into())
    }

    /// Rakentaa [`BenchError::Metric`]-variantin.
    pub fn metric(msg: impl Into<String>) -> Self {
        Self::Metric(msg.into())
    }

    /// Rakentaa [`BenchError::Scorecard`]-variantin.
    pub fn scorecard(msg: impl Into<String>) -> Self {
        Self::Scorecard(msg.into())
    }
}

/// Harnessin vakiotulostyyppi: [`std::result::Result`] jonka virhe on aina
/// [`BenchError`].
pub type Result<T> = std::result::Result<T, BenchError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_variant_and_message() {
        assert!(matches!(BenchError::subject("x"), BenchError::Subject(_)));
        assert!(matches!(BenchError::scenario("x"), BenchError::Scenario(_)));
        assert!(matches!(BenchError::metric("x"), BenchError::Metric(_)));
        assert!(matches!(BenchError::scorecard("x"), BenchError::Scorecard(_)));
    }

    #[test]
    fn core_error_converts_via_from() {
        let err: BenchError = familyclaw_core::FamilyClawError::config("boom").into();
        assert!(matches!(err, BenchError::Core(_)));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<BenchError>();
    }
}
