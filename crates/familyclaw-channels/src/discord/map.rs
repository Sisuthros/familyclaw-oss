//! Discord-viestin suodatus ja muunnos [`InboundEnvelope`]:ksi.
//!
//! [`map_message`] kapseloi gateway-eventistä poimitut kentät kanoniseen
//! sisään tulevaan kirjekuoreen. Suodatus tapahtuu ennen
//! [`InboundMessage::new`]-kutsua, jotta tyhjät ja bot-viestit eivät päädy
//! Resonance Busiin.

use crate::message::{ChannelKind, InboundEnvelope, InboundMessage};

/// Suodattaa ja muuntaa Discord-viestin [`InboundEnvelope`]:ksi.
///
/// Palauttaa `None`, jos viesti pitää jättää huomiotta:
/// - `channel_id != target_channel_id` (väärä kanava),
/// - `author_is_bot` (estetään kaiku),
/// - `content` on tyhjä tai pelkkää whitespacea.
///
/// Onnistuneessa tapauksessa `sender` ja `conversation` ovat desimaalimerkkijonoja,
/// `body` säilytetään sellaisenaan (ei trimmata), `kind` on [`ChannelKind::Discord`]
/// ja envelope `channel_id` on kohdekanavan id desimaalimuodossa.
///
/// # Esimerkkejä
///
/// ```
/// use familyclaw_channels::discord::map::map_message;
/// use familyclaw_channels::ChannelKind;
///
/// let env = map_message(42, false, 100, 100, "moi").expect("valid message");
/// assert_eq!(env.sender, "42");
/// assert_eq!(env.conversation, "100");
/// assert_eq!(env.body, "moi");
/// assert_eq!(env.kind, ChannelKind::Discord);
/// assert_eq!(env.channel_id, "100");
/// ```
pub fn map_message(
    author_id: u64,
    author_is_bot: bool,
    channel_id: u64,
    target_channel_id: u64,
    content: &str,
) -> Option<InboundEnvelope> {
    if channel_id != target_channel_id {
        return None;
    }
    if author_is_bot {
        return None;
    }
    if content.trim().is_empty() {
        return None;
    }

    let inbound =
        InboundMessage::new(author_id.to_string(), channel_id.to_string(), content).ok()?;

    Some(inbound.into_envelope(ChannelKind::Discord, target_channel_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::map_message;
    use crate::message::ChannelKind;

    #[test]
    fn maps_valid_human_message() {
        let env = map_message(42, false, 100, 100, "moi").expect("Some envelope");
        assert_eq!(env.sender, "42");
        assert_eq!(env.conversation, "100");
        assert_eq!(env.body, "moi");
        assert_eq!(env.kind, ChannelKind::Discord);
        assert_eq!(env.channel_id, "100");
    }

    #[test]
    fn wrong_channel_returns_none() {
        assert!(map_message(1, false, 99, 100, "moi").is_none());
    }

    #[test]
    fn bot_message_returns_none() {
        assert!(map_message(1, true, 100, 100, "moi").is_none());
    }

    #[test]
    fn whitespace_only_content_returns_none() {
        assert!(map_message(1, false, 100, 100, "").is_none());
        assert!(map_message(1, false, 100, 100, "   ").is_none());
        assert!(map_message(1, false, 100, 100, "\n\t").is_none());
    }

    #[test]
    fn u64_max_ids_as_decimal_strings() {
        let author = u64::MAX;
        let channel = u64::MAX;
        let env = map_message(author, false, channel, channel, "ping").expect("Some");
        assert_eq!(env.sender, author.to_string());
        assert_eq!(env.conversation, channel.to_string());
        assert_eq!(env.channel_id, channel.to_string());
    }

    #[test]
    fn body_is_not_trimmed_in_envelope() {
        let env = map_message(1, false, 100, 100, "  hello  ").expect("Some");
        assert_eq!(env.body, "  hello  ");
    }
}
