//! Kanavakerroksen virhetyypit.
//!
//! [`ChannelError`] kattaa kanavan kuljetus- ja elinkaarivirheet (kanava
//! suljettu, vastaanotto/lähetys epäonnistui, tuntematon kanava). Tyyppi
//! muuntuu alustan keskitettyyn [`FamilyClawError`]-tyyppiin
//! ([`FamilyClawError::Bus`]) [`From`]-toteutuksella, jotta kanavavirheet
//! virtaavat samaan virhepolkuun kuin muu Resonance Bus -liikenne.
//!
//! Tuotantopolulla EI käytetä `unwrap()`/`expect()`/`panic!()` — kaikki
//! kanavavirheet kulkevat [`Result`]-tyypin kautta.

use familyclaw_core::FamilyClawError;
use thiserror::Error;

/// Kanavakerroksen virhe.
///
/// `#[non_exhaustive]` jotta uusia variantteja voi lisätä myöhemmin
/// rikkomatta downstream-koodia.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChannelError {
    /// Kanava on suljettu eikä voi enää lähettää tai vastaanottaa.
    #[error("channel '{0}' is closed")]
    Closed(String),

    /// Viestin lähetys kanavalle epäonnistui.
    #[error("send failed on channel '{channel}': {reason}")]
    Send {
        /// Kanavan tunniste johon lähetys epäonnistui.
        channel: String,
        /// Ihmisluettava syy.
        reason: String,
    },

    /// Viestin vastaanotto kanavalta epäonnistui.
    #[error("receive failed on channel '{channel}': {reason}")]
    Receive {
        /// Kanavan tunniste jolta vastaanotto epäonnistui.
        channel: String,
        /// Ihmisluettava syy.
        reason: String,
    },

    /// Annettu syöte (esim. tyhjä viestiteksti tai kanava-id) oli kelvoton.
    #[error("invalid channel input: {0}")]
    InvalidInput(String),

    /// Taustalla oleva kanava-adapteri (Discord/Telegram/…) raportoi virheen.
    #[error("backend error on channel '{channel}': {reason}")]
    Backend {
        /// Kanavan tunniste.
        channel: String,
        /// Adapterin raportoima syy.
        reason: String,
    },
}

impl ChannelError {
    /// Rakentaa [`ChannelError::Closed`]-variantin.
    pub fn closed(channel: impl Into<String>) -> Self {
        Self::Closed(channel.into())
    }

    /// Rakentaa [`ChannelError::Send`]-variantin.
    pub fn send(channel: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Send {
            channel: channel.into(),
            reason: reason.into(),
        }
    }

    /// Rakentaa [`ChannelError::Receive`]-variantin.
    pub fn receive(channel: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Receive {
            channel: channel.into(),
            reason: reason.into(),
        }
    }

    /// Rakentaa [`ChannelError::InvalidInput`]-variantin.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    /// Rakentaa [`ChannelError::Backend`]-variantin.
    pub fn backend(channel: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Backend {
            channel: channel.into(),
            reason: reason.into(),
        }
    }
}

impl From<ChannelError> for FamilyClawError {
    /// Kanavavirhe luokitellaan alustan tasolla bus-virheeksi: kanavat ovat
    /// Resonance Busin reunat ulkomaailmaan.
    fn from(err: ChannelError) -> Self {
        FamilyClawError::bus(err.to_string())
    }
}

/// Kanavakerroksen vakiotulostyyppi.
pub type ChannelResult<T> = std::result::Result<T, ChannelError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_variant_and_message() {
        assert!(matches!(ChannelError::closed("c"), ChannelError::Closed(_)));
        assert_eq!(
            ChannelError::closed("discord").to_string(),
            "channel 'discord' is closed"
        );
        assert_eq!(
            ChannelError::send("tg", "timeout").to_string(),
            "send failed on channel 'tg': timeout"
        );
        assert_eq!(
            ChannelError::receive("tg", "queue empty").to_string(),
            "receive failed on channel 'tg': queue empty"
        );
        assert_eq!(
            ChannelError::invalid_input("empty body").to_string(),
            "invalid channel input: empty body"
        );
        assert_eq!(
            ChannelError::backend("sig", "401").to_string(),
            "backend error on channel 'sig': 401"
        );
    }

    #[test]
    fn converts_into_familyclaw_bus_error() {
        let err: FamilyClawError = ChannelError::closed("discord").into();
        assert!(matches!(err, FamilyClawError::Bus(_)));
        assert!(err.to_string().contains("channel 'discord' is closed"));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<ChannelError>();
    }
}
