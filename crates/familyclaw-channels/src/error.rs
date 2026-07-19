//! Channel layer error types.
//!
//! [`ChannelError`] covers the channel's transport and lifecycle errors
//! (channel closed, receive/send failed, unknown channel). The type
//! converts into the platform's centralized [`FamilyClawError`] type
//! ([`FamilyClawError::Bus`]) via a [`From`] implementation, so channel
//! errors flow through the same error path as the rest of the Resonance Bus
//! traffic.
//!
//! The production path does NOT use `unwrap()`/`expect()`/`panic!()` — all
//! channel errors flow through the [`Result`] type.

use familyclaw_core::FamilyClawError;
use thiserror::Error;

/// A channel layer error.
///
/// `#[non_exhaustive]` so new variants can be added later without breaking
/// downstream code.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChannelError {
    /// The channel is closed and can no longer send or receive.
    #[error("channel '{0}' is closed")]
    Closed(String),

    /// Sending a message on the channel failed.
    #[error("send failed on channel '{channel}': {reason}")]
    Send {
        /// The identifier of the channel the send failed on.
        channel: String,
        /// A human-readable reason.
        reason: String,
    },

    /// Receiving a message from the channel failed.
    #[error("receive failed on channel '{channel}': {reason}")]
    Receive {
        /// The identifier of the channel the receive failed on.
        channel: String,
        /// A human-readable reason.
        reason: String,
    },

    /// The given input (e.g. empty message text or channel id) was invalid.
    #[error("invalid channel input: {0}")]
    InvalidInput(String),

    /// The underlying channel adapter (Discord/Telegram/…) reported an error.
    #[error("backend error on channel '{channel}': {reason}")]
    Backend {
        /// The channel's identifier.
        channel: String,
        /// The reason reported by the adapter.
        reason: String,
    },
}

impl ChannelError {
    /// Builds a [`ChannelError::Closed`] variant.
    pub fn closed(channel: impl Into<String>) -> Self {
        Self::Closed(channel.into())
    }

    /// Builds a [`ChannelError::Send`] variant.
    pub fn send(channel: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Send {
            channel: channel.into(),
            reason: reason.into(),
        }
    }

    /// Builds a [`ChannelError::Receive`] variant.
    pub fn receive(channel: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Receive {
            channel: channel.into(),
            reason: reason.into(),
        }
    }

    /// Builds a [`ChannelError::InvalidInput`] variant.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    /// Builds a [`ChannelError::Backend`] variant.
    pub fn backend(channel: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Backend {
            channel: channel.into(),
            reason: reason.into(),
        }
    }
}

impl From<ChannelError> for FamilyClawError {
    /// A channel error is classified at the platform level as a bus error:
    /// channels are the Resonance Bus's edges to the outside world.
    fn from(err: ChannelError) -> Self {
        FamilyClawError::bus(err.to_string())
    }
}

/// The channel layer's standard result type.
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
