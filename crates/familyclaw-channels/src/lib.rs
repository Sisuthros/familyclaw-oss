//! # familyclaw-channels
//!
//! FamilyClaw-alustan **kanavakerros** (KERROS A / OSS, design §3): yhtenäinen
//! [`Channel`]-rajapinta saapuville ja lähteville viesteille sekä silta
//! Resonance Busiin ([`InboundEnvelope`]).
//!
//! ## Mitä tämä crate tarjoaa
//! - [`Channel`] — kaksisuuntaisen kanavan rajapinta: [`Channel::send`],
//!   [`Channel::receive`] ([`MessageStream`]), [`Channel::channel_id`],
//!   [`Channel::kind`]. Dyn-yhteensopiva (`Box<dyn Channel>`).
//! - [`InboundEnvelope`] — kanonisoitu, alkuperätietoinen kirjekuore.
//! - [`ChannelKind`] — Discord / Telegram / `WhatsApp` / Signal / Mock.
//! - [`OutboundMessage`] / [`InboundMessage`] / [`InboundEnvelope`] —
//!   viestityypit ja kanonisointi (`saapuva viesti → InboundEnvelope`).
//! - [`MockChannel`] — in-memory testikanava ilman ulkoista SDK:ta.
//! - [`pump_to`] — integraatiosauma: kanavan virta → Resonance Bus.
//!
//! ## Kanava-adapterit ovat feature-flagien takana
//! Oikeat adapterit (esim. **serenity** Discordille, **teloxide** Telegramille)
//! vetävät sisään raskaita kanava-SDK:ita. Siksi ne ovat craten feature-
//! flagien (`discord`, `telegram`, `whatsapp`, `signal`) takana, eivät
//! pakollisia riippuvuuksia. Oletuskäännös sisältää **vain** rungon +
//! [`MockChannel`], joten alusta kääntyy ja testautuu ilman verkkoa tai
//! raskaita SDK:ita. Adapterien konkreettiset SDK-riippuvuudet lisätään
//! kunkin featuren `[dependencies]`-osioon vasta kun adapteri toteutetaan.
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate ei kovakoodaa kanavatokeneita, Discord-/Telegram-tunnisteita,
//! palvelin-IP:itä tai henkilökohtaisia polkuja. Tunnukset ja kohteet ovat
//! ajonaikaista konfiguraatiota; tyypit kantavat vain geneerisen rakenteen.
//!
//! ## Esimerkki
//! ```
//! # use familyclaw_channels::{Channel, ChannelKind, InboundMessage, MockChannel, OutboundMessage};
//! # #[tokio::main]
//! # async fn main() -> familyclaw_channels::ChannelResult<()> {
//! let channel = MockChannel::new("agent-a-mock")?;
//! let mut inbound = channel.receive()?;
//!
//! // Ulkomaailma syöttää saapuvan viestin → se kanonisoituu InboundEnvelopeksi.
//! channel.inject(InboundMessage::new("user-1", "general", "moi")?)?;
//! let bus_msg = inbound.recv().await.expect("one message");
//! assert_eq!(bus_msg.kind, ChannelKind::Mock);
//! assert_eq!(bus_msg.body, "moi");
//!
//! // Vastataan samaan keskusteluun.
//! channel.send(bus_msg.reply("hei takaisin")?).await?;
//! assert_eq!(channel.sent()[0].body, "hei takaisin");
//! # Ok(())
//! # }
//! ```

mod channel;
mod error;
mod message;
mod mock;

#[cfg(feature = "discord")]
mod discord;

#[cfg(feature = "discord")]
mod discord_interactions;

#[cfg(feature = "telegram")]
mod telegram;

pub use channel::{Channel, MessageStream, SendFuture};
pub use error::{ChannelError, ChannelResult};
pub use message::{ChannelKind, InboundEnvelope, InboundMessage, OutboundMessage};
pub use mock::{pump_to, MockChannel};

#[cfg(feature = "discord")]
pub use discord::DiscordChannel;

#[cfg(feature = "discord")]
pub use discord_interactions::{
    verify_signature, DiscordInteraction, RESPONSE_CHANNEL_MESSAGE, RESPONSE_DEFERRED_CHANNEL_MESSAGE,
    RESPONSE_PONG,
};

#[cfg(feature = "telegram")]
pub use telegram::TelegramChannel;

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // Jos jokin re-export poistetaan, tämä testi ei käänny.
        let kind = ChannelKind::Mock;
        assert_eq!(kind, ChannelKind::Mock);
        let out = OutboundMessage::new("c", "b").expect("outbound");
        assert_eq!(out.body, "b");
        let inbound = InboundMessage::new("s", "c", "b").expect("inbound");
        assert_eq!(inbound.sender, "s");
        let ch = MockChannel::new("m").expect("mock channel");
        assert_eq!(ch.channel_id(), "m");
        let err = ChannelError::closed("c");
        assert!(matches!(err, ChannelError::Closed(_)));
        let ok: ChannelResult<()> = Ok(());
        assert!(ok.is_ok());
    }

    #[tokio::test]
    async fn end_to_end_inbound_to_bus_and_reply() {
        let ch = MockChannel::with_kind("e2e", ChannelKind::Telegram).expect("channel");
        let mut stream = ch.receive().expect("stream");

        ch.inject(InboundMessage::new("u", "chat-9", "hello").expect("inbound"))
            .expect("inject");

        let env: InboundEnvelope = stream.recv().await.expect("message");
        assert_eq!(env.kind, ChannelKind::Telegram);
        assert_eq!(env.channel_id, "e2e");

        ch.send(env.reply("hi").expect("reply"))
            .await
            .expect("send");
        let sent = ch.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].target, "chat-9");
        assert_eq!(sent[0].body, "hi");
    }
}
