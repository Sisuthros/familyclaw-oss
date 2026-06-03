//! [`Channel`]-rajapinta ja saapuvien viestien virta ([`MessageStream`]).
//!
//! `Channel` on alustan abstraktio yhdelle kaksisuuntaiselle kanavalle:
//! - [`Channel::send`] lähettää [`OutboundMessage`]-viestin ulos,
//! - [`Channel::receive`] palauttaa [`MessageStream`]-virran saapuvista
//!   [`InboundEnvelope`]-kirjekuorista,
//! - [`Channel::channel_id`] ja [`Channel::kind`] tunnistavat kanavainstanssin.
//!
//! Rajapinta on **dyn-yhteensopiva** (`Box<dyn Channel>`), jotta eri kanavia
//! voi pitää yhdessä kokoelmassa. Tämän vuoksi `send` palauttaa eksplisiittisen
//! boxatun futuren ([`SendFuture`]) — näin vältetään raskas ulkoinen
//! `async-trait`-riippuvuus ja säilytetään silti trait-objektit.

use std::future::Future;
use std::pin::Pin;

use crate::error::ChannelResult;
use crate::message::{ChannelKind, InboundEnvelope, OutboundMessage};

/// Boxattu, lähetettävä future kanavan [`Channel::send`]-operaatiolle.
///
/// Eksplisiittinen tyyppi tekee [`Channel`]-traitista dyn-yhteensopivan ilman
/// `async-trait`-makroa.
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = ChannelResult<()>> + Send + 'a>>;

/// Saapuvien [`InboundEnvelope`]-kirjekuorten virta yhdeltä kanavalta.
///
/// Virtaa kulutetaan [`MessageStream::recv`]-metodilla. `None` tarkoittaa, että
/// kanava on suljettu eikä lisää viestejä tule. Tyyppi kääri sisäisen
/// kanava-vastaanottimen, jotta toteutus ei vuoda kutsujalle.
#[derive(Debug)]
pub struct MessageStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>,
}

impl MessageStream {
    /// Rakentaa virran annetusta vastaanottimesta.
    ///
    /// Kanava-toteutukset luovat parin
    /// [`tokio::sync::mpsc::unbounded_channel`]-kutsulla ja antavat
    /// vastaanottimen tähän.
    #[must_use]
    pub fn new(rx: tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>) -> Self {
        Self { rx }
    }

    /// Odottaa ja palauttaa seuraavan saapuvan viestin, tai `None` kun kanava
    /// on lopullisesti suljettu.
    pub async fn recv(&mut self) -> Option<InboundEnvelope> {
        self.rx.recv().await
    }

    /// Yrittää ottaa seuraavan viestin estymättä.
    ///
    /// # Errors
    /// [`crate::ChannelError::Receive`] jos jonossa ei juuri nyt ole viestiä
    /// (tyhjä) tai jos virta on suljettu.
    pub fn try_recv(&mut self) -> ChannelResult<InboundEnvelope> {
        self.rx
            .try_recv()
            .map_err(|e| crate::ChannelError::receive("stream", e.to_string()))
    }

    /// Sulkee virran: ei oteta enää uusia viestejä vastaan. Jo jonossa olevat
    /// viestit voi yhä kuluttaa [`recv`](MessageStream::recv)-kutsulla.
    pub fn close(&mut self) {
        self.rx.close();
    }
}

/// Yksi kaksisuuntainen kanava (Discord, Telegram, Mock, …).
///
/// Toteutusten on oltava `Send + Sync`, jotta kanavia voi jakaa actoreiden ja
/// tehtävien välillä Resonance Busissa.
pub trait Channel: Send + Sync {
    /// Tämän kanavainstanssin vakaa tunniste (esim. `"discord-main"`).
    /// Sama arvo päätyy [`InboundEnvelope::channel_id`]-kenttään.
    fn channel_id(&self) -> &str;

    /// Kanavan teknologiatyyppi.
    fn kind(&self) -> ChannelKind;

    /// Lähettää viestin ulos tältä kanavalta.
    ///
    /// Palauttaa boxatun futuren ([`SendFuture`]). Virhe ([`ChannelResult`])
    /// kuvaa kuljetus- tai elinkaarivian; semanttiset validointivirheet
    /// (tyhjä sisältö) on jo torjuttu [`OutboundMessage::new`]-konstruktorissa.
    fn send(&self, message: OutboundMessage) -> SendFuture<'_>;

    /// Avaa saapuvien viestien virran ([`MessageStream`]).
    ///
    /// Kukin saapuva kanavaviesti on jo kanonisoitu [`InboundEnvelope`]:ksi
    /// (sis. [`ChannelKind`] + `channel_id`), valmiina muunnettavaksi busin
    /// hyötykuormaksi.
    ///
    /// # Errors
    /// [`crate::ChannelError::Receive`] tai
    /// [`crate::ChannelError::Closed`] jos virtaa ei voi avata (esim. se on jo
    /// otettu tai kanava on suljettu).
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
        // Lähetys suljettuun virtaan epäonnistuu.
        assert!(tx.send(bus).is_err());
        assert!(stream.recv().await.is_none());
    }
}
