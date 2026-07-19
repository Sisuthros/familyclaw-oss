//! The [`Channel`] interface and the inbound message stream ([`MessageStream`]).
//!
//! `Channel` is the platform's abstraction for a single bidirectional channel:
//! - [`Channel::send`] sends an [`OutboundMessage`] out,
//! - [`Channel::receive`] returns a [`MessageStream`] of inbound
//!   [`InboundEnvelope`] envelopes,
//! - [`Channel::channel_id`] and [`Channel::kind`] identify the channel instance.
//!
//! The interface is **dyn-compatible** (`Box<dyn Channel>`), so different
//! channels can be kept together in one collection. This is why `send`
//! returns an explicit boxed future ([`SendFuture`]) — this avoids a heavy
//! external `async-trait` dependency while still supporting trait objects.

use std::future::Future;
use std::pin::Pin;

use crate::error::ChannelResult;
use crate::message::{ChannelKind, InboundEnvelope, OutboundMessage};

/// A boxed, sendable future for a channel's [`Channel::send`] operation.
///
/// The explicit type makes the [`Channel`] trait dyn-compatible without an
/// `async-trait` macro.
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = ChannelResult<()>> + Send + 'a>>;

/// A stream of inbound [`InboundEnvelope`] envelopes from one channel.
///
/// The stream is drained via the [`MessageStream::recv`] method. `None`
/// means the channel is closed and no more messages will arrive. The type
/// wraps the internal channel receiver so the implementation does not leak
/// to the caller.
#[derive(Debug)]
pub struct MessageStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>,
}

impl MessageStream {
    /// Builds a stream from the given receiver.
    ///
    /// Channel implementations create the pair via
    /// [`tokio::sync::mpsc::unbounded_channel`] and pass the receiver here.
    #[must_use]
    pub fn new(rx: tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>) -> Self {
        Self { rx }
    }

    /// Waits for and returns the next inbound message, or `None` once the
    /// channel is permanently closed.
    pub async fn recv(&mut self) -> Option<InboundEnvelope> {
        self.rx.recv().await
    }

    /// Attempts to take the next message without blocking.
    ///
    /// # Errors
    /// [`crate::ChannelError::Receive`] if there is currently no message in
    /// the queue (empty) or if the stream is closed.
    pub fn try_recv(&mut self) -> ChannelResult<InboundEnvelope> {
        self.rx
            .try_recv()
            .map_err(|e| crate::ChannelError::receive("stream", e.to_string()))
    }

    /// Closes the stream: no new messages will be accepted. Messages already
    /// in the queue can still be drained via [`recv`](MessageStream::recv).
    pub fn close(&mut self) {
        self.rx.close();
    }
}

/// A single bidirectional channel (Discord, Telegram, Mock, …).
///
/// Implementations must be `Send + Sync` so channels can be shared between
/// actors and tasks on the Resonance Bus.
pub trait Channel: Send + Sync {
    /// The stable identifier for this channel instance (e.g. `"discord-main"`).
    /// The same value ends up in the [`InboundEnvelope::channel_id`] field.
    fn channel_id(&self) -> &str;

    /// The channel's technology type.
    fn kind(&self) -> ChannelKind;

    /// Sends a message out from this channel.
    ///
    /// Returns a boxed future ([`SendFuture`]). An error ([`ChannelResult`])
    /// describes a transport or lifecycle failure; semantic validation
    /// errors (empty content) have already been rejected in the
    /// [`OutboundMessage::new`] constructor.
    fn send(&self, message: OutboundMessage) -> SendFuture<'_>;

    /// Opens the inbound message stream ([`MessageStream`]).
    ///
    /// Each inbound channel message is already canonicalized into an
    /// [`InboundEnvelope`] (including [`ChannelKind`] + `channel_id`), ready
    /// to be converted into the bus payload.
    ///
    /// # Errors
    /// [`crate::ChannelError::Receive`] or [`crate::ChannelError::Closed`] if
    /// the stream cannot be opened (e.g. it has already been taken or the
    /// channel is closed).
    fn receive(&self) -> ChannelResult<MessageStream>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::InboundMessage;

    #[tokio::test]
    async fn message_stream_recv_yields_then_none_on_close() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = MessageStream::new(rx);

        let bus = InboundMessage::new("u", "r", "hi")
            .expect("valid")
            .into_envelope(ChannelKind::Mock, "m");
        tx.send(bus.clone()).expect("send");

        let got = stream.recv().await.expect("one message");
        assert_eq!(got.body, "hi");

        drop(tx);
        assert!(stream.recv().await.is_none());
    }

    #[test]
    fn message_stream_try_recv_empty_is_error() {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = MessageStream::new(rx);
        assert!(stream.try_recv().is_err());
    }

    #[tokio::test]
    async fn message_stream_close_stops_new_messages() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = MessageStream::new(rx);
        stream.close();
        let bus = InboundMessage::new("u", "r", "late")
            .expect("valid")
            .into_envelope(ChannelKind::Mock, "m");
        // Sending into a closed stream fails.
        assert!(tx.send(bus).is_err());
        assert!(stream.recv().await.is_none());
    }
}
