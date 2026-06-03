//! [`MockChannel`] — in-memory testikanava ja bus-integraation referenssi.
//!
//! `MockChannel` toteuttaa [`Channel`]-rajapinnan kahdella in-memory-jonolla:
//! - **outbox** — kaikki [`Channel::send`]-kutsut tallentuvat tänne, jotta
//!   testit voivat tarkistaa mitä alusta lähetti,
//! - **inbox** — testi voi syöttää saapuvia viestejä
//!   [`MockChannel::inject`]-metodilla; ne virtaavat [`Channel::receive`]-
//!   virtaan jo kanonisoituina [`InboundEnvelope`]-kirjekuorina.
//!
//! `MockChannel` ei vedä sisään yhtään ulkoista kanava-SDK:ta — se on koko
//! kanavakerroksen testattava ydin ilman verkkoa.
//!
//! ## Bus-integraatio
//! [`pump_to`] yhdistää kanavan ja Resonance Busin: se kuluttaa
//! [`MessageStream`]-virran ja antaa jokaisen [`InboundEnvelope`]-kirjekuoren
//! annetulle julkaisijalle (`publish`-sulkeumalle). Varsinainen envelope →
//! `familyclaw_bus::BusMessage` -muunnos ja julkaisu busiin elävät
//! agent-kerroksessa (joka riippuu molemmista crateista); tämä sulkeuma pitää
//! kanavakerroksen riippumattomana busin sisäisestä Ractor-toteutuksesta.

use std::sync::{Arc, Mutex};

use crate::channel::{Channel, MessageStream, SendFuture};
use crate::error::{ChannelError, ChannelResult};
use crate::message::{ChannelKind, InboundEnvelope, InboundMessage, OutboundMessage};

/// In-memory kanava testaukseen.
///
/// Klonattavissa: kloonit jakavat saman tilan (outbox + lähetin), jotta
/// taustatehtävä voi pitää klonin ja testi tarkistaa toisen klonin kautta.
#[derive(Clone)]
pub struct MockChannel {
    inner: Arc<Inner>,
}

struct Inner {
    channel_id: String,
    kind: ChannelKind,
    /// Kaikki lähetetyt viestit, vanhimmasta uusimpaan.
    outbox: Mutex<Vec<OutboundMessage>>,
    /// Lähetin saapuville viesteille; `None` kun virta on jo otettu.
    inbound_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<InboundEnvelope>>>,
    /// Vastaanotin, joka luovutetaan [`Channel::receive`]-kutsussa kerran.
    inbound_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>>>,
}

impl MockChannel {
    /// Rakentaa uuden mock-kanavan annetulla tunnisteella.
    ///
    /// Kanavatyyppi on [`ChannelKind::Mock`].
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] jos tunniste on tyhjä.
    pub fn new(channel_id: impl Into<String>) -> ChannelResult<Self> {
        Self::with_kind(channel_id, ChannelKind::Mock)
    }

    /// Rakentaa mock-kanavan, joka esiintyy annettuna [`ChannelKind`]-tyyppinä.
    ///
    /// Hyödyllinen, kun testataan tyyppikohtaista reititystä ilman oikeaa
    /// kanava-SDK:ta (esim. teeskennellään Discord-kanavaa).
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] jos tunniste on tyhjä.
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

    /// Syöttää saapuvan raakaviestin. Se kanonisoidaan [`InboundEnvelope`]:ksi
    /// tämän kanavan tyypillä ja tunnisteella, ja työnnetään
    /// [`Channel::receive`]-virtaan.
    ///
    /// # Errors
    /// [`ChannelError::Closed`] jos virta on jo suljettu (vastaanotin
    /// pudotettu).
    pub fn inject(&self, message: InboundMessage) -> ChannelResult<InboundEnvelope> {
        let env = message.into_envelope(self.inner.kind, self.inner.channel_id.clone());
        self.push_envelope(env)
    }

    /// Työntää valmiin [`InboundEnvelope`]:n virtaan sellaisenaan.
    ///
    /// # Errors
    /// [`ChannelError::Closed`] jos virta on jo suljettu.
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

    /// Palauttaa kopion tähän mennessä lähetetyistä viesteistä.
    #[must_use]
    pub fn sent(&self) -> Vec<OutboundMessage> {
        self.inner
            .outbox
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Montako viestiä on lähetetty.
    #[must_use]
    pub fn sent_count(&self) -> usize {
        self.inner.outbox.lock().map_or(0, |v| v.len())
    }

    /// Sulkee saapuvan virran: ei enää [`inject`](MockChannel::inject)-
    /// kutsuja onnistu. Aiheuttaa [`MessageStream::recv`]-palautukseksi `None`
    /// kun jäljellä olevat viestit on kulutettu.
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
        let mut guard = self
            .inner
            .inbound_rx
            .lock()
            .map_err(|_| ChannelError::backend(self.channel_id(), "inbound_rx lock poisoned"))?;
        let rx = guard.take().ok_or_else(|| {
            ChannelError::receive(self.channel_id(), "receive stream already taken")
        })?;
        Ok(MessageStream::new(rx))
    }
}

/// Pumppaa kanavan saapuvan virran kohti Resonance Busia.
///
/// Kuluttaa [`MessageStream`]-virran loppuun ja kutsuu `publish`-sulkeumaa
/// jokaiselle [`InboundEnvelope`]:lle. Tämä on kanavakerroksen ja busin
/// **integraatiosauma**: agent-kerros antaa tähän sulkeuman joka muuntaa
/// envelopen `familyclaw_bus::BusMessage`:ksi ja julkaisee sen busiin
/// (ks. agent-craten adapteri). Kanavakerros itse pysyy bus-riippumattomana.
///
/// Funktio palaa, kun virta sulkeutuu (`recv` palauttaa `None`) tai kun
/// `publish` palauttaa virheen — jälkimmäinen propagoidaan kutsujalle.
/// Palautusarvona on käsiteltyjen viestien määrä.
///
/// # Errors
/// Propagoi ensimmäisen `publish`-sulkeuman palauttaman virheen.
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
        // Jäljellä oleva viesti tulee vielä, sitten None.
        assert_eq!(stream.recv().await.expect("buffered").body, "a");
        assert!(stream.recv().await.is_none());
        // Sulkemisen jälkeen inject epäonnistuu.
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
        // Alkuperäinen näkee klonin lähetyksen (jaettu outbox).
        assert_eq!(ch.sent_count(), 1);
        assert_eq!(ch.sent()[0].body, "from clone");
    }

    #[tokio::test]
    async fn pump_to_publishes_all_messages_to_bus() {
        let ch = MockChannel::new("m1").expect("channel");
        let stream = ch.receive().expect("stream");

        for i in 0..3 {
            ch.inject(
                InboundMessage::new("u", "c", format!("msg{i}")).expect("inbound"),
            )
            .expect("inject");
        }
        ch.close_inbound();

        // "Busi" — kerää julkaistut kirjekuoret talteen.
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
        // Kaikki kantavat oikean alkuperän.
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
        // Varmistaa että trait on dyn-yhteensopiva.
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
