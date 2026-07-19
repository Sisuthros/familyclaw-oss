//! [`MockChannel`] — an in-memory test channel and bus integration reference.
//!
//! `MockChannel` implements the [`Channel`] interface with two in-memory queues:
//! - **outbox** — every [`Channel::send`] call is recorded here so tests can
//!   check what the platform sent,
//! - **inbox** — a test can feed inbound messages via the
//!   [`MockChannel::inject`] method; they flow into the [`Channel::receive`]
//!   stream already canonicalized as [`InboundEnvelope`] envelopes.
//!
//! `MockChannel` does not pull in any external channel SDK — it is the
//! testable core of the entire channel layer without any network access.
//!
//! ## Bus integration
//! [`pump_to`] connects the channel to the Resonance Bus: it drains the
//! [`MessageStream`] and hands each [`InboundEnvelope`] to the given
//! publisher (the `publish` closure). The actual envelope →
//! `familyclaw_bus::BusMessage` conversion and publishing to the bus live in
//! the agent layer (which depends on both crates); this closure keeps the
//! channel layer independent of the bus's internal Ractor implementation.

use std::sync::{Arc, Mutex};

use crate::channel::{Channel, MessageStream, SendFuture};
use crate::error::{ChannelError, ChannelResult};
use crate::message::{ChannelKind, InboundEnvelope, InboundMessage, OutboundMessage};

/// An in-memory channel for testing.
///
/// Cloneable: clones share the same state (outbox + sender), so a
/// background task can hold a clone while the test checks it through another clone.
#[derive(Clone)]
pub struct MockChannel {
    inner: Arc<Inner>,
}

struct Inner {
    channel_id: String,
    kind: ChannelKind,
    /// All sent messages, oldest to newest.
    outbox: Mutex<Vec<OutboundMessage>>,
    /// Sender for inbound messages; `None` once the stream has been taken.
    inbound_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<InboundEnvelope>>>,
    /// Receiver, handed out once in the [`Channel::receive`] call.
    inbound_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>>>,
}

impl MockChannel {
    /// Builds a new mock channel with the given identifier.
    ///
    /// The channel type is [`ChannelKind::Mock`].
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] if the identifier is empty.
    pub fn new(channel_id: impl Into<String>) -> ChannelResult<Self> {
        Self::with_kind(channel_id, ChannelKind::Mock)
    }

    /// Builds a mock channel that presents itself as the given [`ChannelKind`].
    ///
    /// Useful for testing type-specific routing without a real channel SDK
    /// (e.g. impersonating a Discord channel).
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] if the identifier is empty.
    pub fn with_kind(channel_id: impl Into<String>, kind: ChannelKind) -> ChannelResult<Self> {
        let channel_id = channel_id.into();
        if channel_id.trim().is_empty() {
            return Err(ChannelError::invalid_input("channel_id must not be empty"));
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            inner: Arc::new(Inner {
                channel_id,
                kind,
                outbox: Mutex::new(Vec::new()),
                inbound_tx: Mutex::new(Some(tx)),
                inbound_rx: Mutex::new(Some(rx)),
            }),
        })
    }

    /// Injects a raw inbound message. It is canonicalized into an
    /// [`InboundEnvelope`] with this channel's type and identifier, and
    /// pushed into the [`Channel::receive`] stream.
    ///
    /// # Errors
    /// [`ChannelError::Closed`] if the stream is already closed (receiver dropped).
    pub fn inject(&self, message: InboundMessage) -> ChannelResult<InboundEnvelope> {
        let env = message.into_envelope(self.inner.kind, self.inner.channel_id.clone());
        self.push_envelope(env)
    }

    /// Pushes a completed [`InboundEnvelope`] into the stream as-is.
    ///
    /// # Errors
    /// [`ChannelError::Closed`] if the stream is already closed.
    pub fn push_envelope(&self, env: InboundEnvelope) -> ChannelResult<InboundEnvelope> {
        let guard = self
            .inner
            .inbound_tx
            .lock()
            .map_err(|_| ChannelError::backend(self.channel_id(), "inbound lock poisoned"))?;
        let tx = guard
            .as_ref()
            .ok_or_else(|| ChannelError::closed(self.channel_id()))?;
        tx.send(env.clone())
            .map_err(|_| ChannelError::closed(self.channel_id()))?;
        Ok(env)
    }

    /// Returns a copy of the messages sent so far.
    #[must_use]
    pub fn sent(&self) -> Vec<OutboundMessage> {
        self.inner
            .outbox
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// How many messages have been sent.
    #[must_use]
    pub fn sent_count(&self) -> usize {
        self.inner.outbox.lock().map_or(0, |v| v.len())
    }

    /// Closes the inbound stream: no further
    /// [`inject`](MockChannel::inject) calls will succeed. Causes
    /// [`MessageStream::recv`] to return `None` once any remaining messages
    /// have been drained.
    pub fn close_inbound(&self) {
        if let Ok(mut guard) = self.inner.inbound_tx.lock() {
            *guard = None;
        }
    }
}

impl std::fmt::Debug for MockChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockChannel")
            .field("channel_id", &self.inner.channel_id)
            .field("kind", &self.inner.kind)
            .field("sent_count", &self.sent_count())
            .finish()
    }
}

impl Channel for MockChannel {
    fn channel_id(&self) -> &str {
        &self.inner.channel_id
    }

    fn kind(&self) -> ChannelKind {
        self.inner.kind
    }

    fn send(&self, message: OutboundMessage) -> SendFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let mut outbox = inner
                .outbox
                .lock()
                .map_err(|_| ChannelError::backend(&inner.channel_id, "outbox lock poisoned"))?;
            outbox.push(message);
            Ok(())
        })
    }

    fn receive(&self) -> ChannelResult<MessageStream> {
        let mut guard =
            self.inner.inbound_rx.lock().map_err(|_| {
                ChannelError::backend(self.channel_id(), "inbound_rx lock poisoned")
            })?;
        let rx = guard.take().ok_or_else(|| {
            ChannelError::receive(self.channel_id(), "receive stream already taken")
        })?;
        Ok(MessageStream::new(rx))
    }
}

/// Pumps a channel's inbound stream toward the Resonance Bus.
///
/// Drains the [`MessageStream`] to completion and calls the `publish`
/// closure for each [`InboundEnvelope`]. This is the **integration seam**
/// between the channel layer and the bus: the agent layer supplies this
/// closure, which converts the envelope into a `familyclaw_bus::BusMessage`
/// and publishes it to the bus (see the agent crate's adapter). The channel
/// layer itself remains bus-independent.
///
/// The function returns once the stream closes (`recv` returns `None`) or
/// once `publish` returns an error — the latter is propagated to the
/// caller. The return value is the number of messages processed.
///
/// # Errors
/// Propagates the first error returned by the `publish` closure.
pub async fn pump_to<F>(mut stream: MessageStream, mut publish: F) -> ChannelResult<usize>
where
    F: FnMut(InboundEnvelope) -> ChannelResult<()>,
{
    let mut count = 0usize;
    while let Some(env) = stream.recv().await {
        publish(env)?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_id() {
        assert!(MockChannel::new("  ").is_err());
        assert!(MockChannel::new("ok").is_ok());
    }

    #[tokio::test]
    async fn send_records_to_outbox() {
        let ch = MockChannel::new("m1").expect("channel");
        assert_eq!(ch.sent_count(), 0);

        ch.send(OutboundMessage::new("room", "hello").expect("msg"))
            .await
            .expect("send ok");
        ch.send(OutboundMessage::new("room", "again").expect("msg"))
            .await
            .expect("send ok");

        let sent = ch.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].body, "hello");
        assert_eq!(sent[1].body, "again");
        assert_eq!(ch.sent_count(), 2);
    }

    #[tokio::test]
    async fn inject_flows_to_receive_stream_as_bus_message() {
        let ch = MockChannel::new("m1").expect("channel");
        let mut stream = ch.receive().expect("stream");

        let injected = ch
            .inject(InboundMessage::new("user1", "general", "ping").expect("inbound"))
            .expect("inject ok");

        let got = stream.recv().await.expect("one message");
        assert_eq!(got.id, injected.id);
        assert_eq!(got.kind, ChannelKind::Mock);
        assert_eq!(got.channel_id, "m1");
        assert_eq!(got.sender, "user1");
        assert_eq!(got.conversation, "general");
        assert_eq!(got.body, "ping");
    }

    #[tokio::test]
    async fn with_kind_tags_bus_message_kind() {
        let ch = MockChannel::with_kind("disc-1", ChannelKind::Discord).expect("channel");
        assert_eq!(ch.kind(), ChannelKind::Discord);
        let mut stream = ch.receive().expect("stream");
        ch.inject(InboundMessage::new("u", "c", "hi").expect("inbound"))
            .expect("inject");
        let got = stream.recv().await.expect("message");
        assert_eq!(got.kind, ChannelKind::Discord);
        assert_eq!(got.channel_id, "disc-1");
    }

    #[test]
    fn receive_can_only_be_taken_once() {
        let ch = MockChannel::new("m1").expect("channel");
        assert!(ch.receive().is_ok());
        assert!(ch.receive().is_err());
    }

    #[tokio::test]
    async fn close_inbound_ends_stream() {
        let ch = MockChannel::new("m1").expect("channel");
        let mut stream = ch.receive().expect("stream");
        ch.inject(InboundMessage::new("u", "c", "a").expect("inbound"))
            .expect("inject");
        ch.close_inbound();
        // The remaining message still arrives, then None.
        assert_eq!(stream.recv().await.expect("buffered").body, "a");
        assert!(stream.recv().await.is_none());
        // After closing, inject fails.
        assert!(ch
            .inject(InboundMessage::new("u", "c", "b").expect("inbound"))
            .is_err());
    }

    #[tokio::test]
    async fn clones_share_state() {
        let ch = MockChannel::new("m1").expect("channel");
        let clone = ch.clone();
        clone
            .send(OutboundMessage::new("r", "from clone").expect("msg"))
            .await
            .expect("send");
        // The original sees the clone's send (shared outbox).
        assert_eq!(ch.sent_count(), 1);
        assert_eq!(ch.sent()[0].body, "from clone");
    }

    #[tokio::test]
    async fn pump_to_publishes_all_messages_to_bus() {
        let ch = MockChannel::new("m1").expect("channel");
        let stream = ch.receive().expect("stream");

        for i in 0..3 {
            ch.inject(InboundMessage::new("u", "c", format!("msg{i}")).expect("inbound"))
                .expect("inject");
        }
        ch.close_inbound();

        // The "bus" — collects the published envelopes.
        let collected = Arc::new(Mutex::new(Vec::<InboundEnvelope>::new()));
        let sink = Arc::clone(&collected);
        let count = pump_to(stream, move |env| {
            sink.lock()
                .map_err(|_| ChannelError::backend("bus", "sink poisoned"))?
                .push(env);
            Ok(())
        })
        .await
        .expect("pump ok");

        assert_eq!(count, 3);
        let published = collected.lock().expect("lock");
        assert_eq!(published.len(), 3);
        assert_eq!(published[0].body, "msg0");
        assert_eq!(published[2].body, "msg2");
        // All of them carry the correct origin.
        assert!(published.iter().all(|m| m.channel_id == "m1"));
    }

    #[tokio::test]
    async fn pump_to_propagates_publish_error() {
        let ch = MockChannel::new("m1").expect("channel");
        let stream = ch.receive().expect("stream");
        ch.inject(InboundMessage::new("u", "c", "x").expect("inbound"))
            .expect("inject");

        let err = pump_to(stream, |_bus| Err(ChannelError::backend("bus", "down")))
            .await
            .expect_err("publish error propagates");
        assert!(matches!(err, ChannelError::Backend { .. }));
    }

    #[test]
    fn debug_impl_is_concise() {
        let ch = MockChannel::new("m1").expect("channel");
        let dbg = format!("{ch:?}");
        assert!(dbg.contains("MockChannel"));
        assert!(dbg.contains("m1"));
    }

    #[tokio::test]
    async fn works_through_dyn_channel() {
        // Verifies that the trait is dyn-compatible.
        let ch = MockChannel::new("dynm").expect("channel");
        let boxed: Box<dyn Channel> = Box::new(ch.clone());
        assert_eq!(boxed.channel_id(), "dynm");
        assert_eq!(boxed.kind(), ChannelKind::Mock);
        boxed
            .send(OutboundMessage::new("r", "via dyn").expect("msg"))
            .await
            .expect("send");
        assert_eq!(ch.sent_count(), 1);
    }
}
