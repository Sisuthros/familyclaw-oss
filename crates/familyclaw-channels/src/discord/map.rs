//! Discord-viestin suodatus ja muunnos [`InboundEnvelope`]:ksi.
//!
//! [`map_message`] kapseloi gateway-eventistä poimitut kentät kanoniseen
//! sisään tulevaan kirjekuoreen. Suodatus tapahtuu ennen
//! [`InboundMessage::new`]-kutsua, jotta tyhjät ja bot-viestit eivät päädy
//! Resonance Busiin.

use crate::message::{ChannelKind, InboundEnvelope, InboundMessage};

/// Suodattaa ja muuntaa Discord-viestin [`InboundEnvelope`]:ksi.
///
/// Kaksi reittiä:
///
/// **Yksityisviesti (DM, `is_dm == true`):** läpäisee VAIN jos `author_id == owner_id`
/// (huoltaja/operaattori) eikä ole botti. Tämä on huoltajan ja agentin
/// kahdenkeskinen keskustelu — kukaan muu ei voi `DMata` agenttia. Vastaus
/// reititetään takaisin DM-kanavalle (`channel_id`), EI ryhmäkanavalle.
///
/// **Ryhmäkanava (`is_dm == false`):** käsitellään vain kohdekanavalla
/// (`channel_id == target_channel_id`). Palauttaa `None` jos:
/// - `author_id == self_id` (oma viesti — self-kaiku-suoja AINA),
/// - `author_is_bot && !mentions_me` (toinen botti joka EI maininnut meitä —
///   estää perheen megaloopin; perheenjäsenet ovat botteja, joten heidät
///   KUULLAAN kun he @-mainitsevat, mutta vapaa bot-chat ei laukaise loopia).
///
/// Molemmissa: tyhjä/whitespace-`content` → `None`.
///
/// Perheen yhteispeli (2026-06-21): perheenjäsenet ovat Discord-botteja. Vanha
/// "pudota kaikki botit" teki olennoista write-only. Nyt botti-perhe kuullaan
/// maininnan takaa (`mentions_me`), `self_id` estää self-echon, ja huoltaja saa
/// oman DM-kanavan agentin kanssa (`owner_id` + `is_dm`).
///
/// Onnistuneessa tapauksessa `sender` ja `conversation` ovat desimaalimerkkijonoja,
/// `body` säilytetään sellaisenaan (ei trimmata), `kind` on [`ChannelKind::Discord`].
/// DM:ssä envelope `channel_id` on DM-kanavan id (vastaus menee sinne); ryhmässä
/// se on kohdekanavan id.
///
/// # Esimerkkejä
///
/// ```
/// use familyclaw_channels::discord::map::map_message;
/// use familyclaw_channels::ChannelKind;
///
/// // Ihminen ryhmäkanavalla: menee läpi ilman mainintapakkoa.
/// // (author=42, ei botti, channel=100=target, self=7, ei DM, owner=5)
/// let env = map_message(42, false, 100, 100, "moi", 7, false, false, 5).expect("valid");
/// assert_eq!(env.sender, "42");
/// assert_eq!(env.conversation, "100");
/// assert_eq!(env.body, "moi");
/// assert_eq!(env.kind, ChannelKind::Discord);
/// assert_eq!(env.channel_id, "100");
/// ```
#[allow(clippy::too_many_arguments)]
pub fn map_message(
    author_id: u64,
    author_is_bot: bool,
    channel_id: u64,
    target_channel_id: u64,
    content: &str,
    self_id: u64,
    mentions_me: bool,
    is_dm: bool,
    owner_id: u64,
) -> Option<InboundEnvelope> {
    // Oma viesti → ei koskaan käsitellä (self-echo-suoja, AINA ensin).
    if author_id == self_id {
        return None;
    }
    if content.trim().is_empty() {
        return None;
    }

    if is_dm {
        // Yksityisviesti: VAIN huoltaja (owner_id), ei botti. Kahdenkeskinen
        // keskustelu — vastaus reititetään takaisin DM-kanavalle (channel_id).
        if author_is_bot || author_id != owner_id {
            return None;
        }
        let inbound =
            InboundMessage::new(author_id.to_string(), channel_id.to_string(), content).ok()?;
        return Some(inbound.into_envelope(ChannelKind::Discord, channel_id.to_string()));
    }

    // Ryhmäkanava: vain kohdekanava.
    if channel_id != target_channel_id {
        return None;
    }
    // Toinen botti (perheenjäsen) kuullaan VAIN kun se mainitsee meidät — estää
    // perheen rakenteellisen megaloopin. Ihmiset (ei-botit) menevät läpi aina.
    if author_is_bot && !mentions_me {
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

    // Vakiot luettavuuteen: self_id = 9, owner (huoltaja/operaattori) = 5.
    const SELF_ID: u64 = 9;
    const OWNER_ID: u64 = 5;

    // Apuri ryhmäkanavaviesteille (is_dm=false, owner=OWNER_ID).
    fn group(
        author: u64,
        bot: bool,
        chan: u64,
        target: u64,
        body: &str,
        mentions: bool,
    ) -> Option<crate::message::InboundEnvelope> {
        map_message(
            author, bot, chan, target, body, SELF_ID, mentions, false, OWNER_ID,
        )
    }

    #[test]
    fn maps_valid_human_message() {
        let env = group(42, false, 100, 100, "moi", false).expect("Some envelope");
        assert_eq!(env.sender, "42");
        assert_eq!(env.conversation, "100");
        assert_eq!(env.body, "moi");
        assert_eq!(env.kind, ChannelKind::Discord);
        assert_eq!(env.channel_id, "100");
    }

    #[test]
    fn wrong_channel_returns_none() {
        assert!(group(1, false, 99, 100, "moi", false).is_none());
    }

    #[test]
    fn own_message_returns_none_even_when_mentioned() {
        // Self-echo-suoja: oma viesti pudotetaan AINA, myös jos "mainitsee" itsensä.
        assert!(group(SELF_ID, true, 100, 100, "moi", true).is_none());
        assert!(group(SELF_ID, false, 100, 100, "moi", true).is_none());
    }

    #[test]
    fn family_bot_heard_only_when_mentioned() {
        // Perheenjäsen (botti) joka EI mainitse meitä → pudotetaan (megaloop-suoja).
        assert!(group(1, true, 100, 100, "moi", false).is_none());
        // Perheenjäsen (botti) joka MAINITSEE meidät → kuullaan.
        let env = group(1, true, 100, 100, "moi @agent", true).expect("Some");
        assert_eq!(env.sender, "1");
    }

    #[test]
    fn human_heard_without_mention() {
        // Ihminen menee läpi vaikka ei mainitse (mention-portti koskee vain botteja).
        let env = group(1, false, 100, 100, "moi", false).expect("Some");
        assert_eq!(env.body, "moi");
    }

    #[test]
    fn dm_from_owner_is_heard_and_replies_to_dm_channel() {
        // DM huoltajalta (owner): läpäisee, vastaus DM-kanavalle (channel_id 500).
        // target_channel_id 100 (ryhmä) jätetään huomiotta DM:ssä.
        let env = map_message(
            OWNER_ID,
            false,
            500,
            100,
            "hei agentti",
            SELF_ID,
            false,
            true,
            OWNER_ID,
        )
        .expect("owner DM heard");
        assert_eq!(env.sender, OWNER_ID.to_string());
        // Vastaus reititetään DM-kanavalle (500), EI ryhmään (100).
        assert_eq!(env.channel_id, "500");
        assert_eq!(env.conversation, "500");
    }

    #[test]
    fn dm_reply_target_is_dm_channel_not_group_channel() {
        // Eksplisiittinen vartija sääntö 9:lle: DM:ssä vastaus reititetään DM-
        // kanavalle (channel_id), EI ryhmäkanavalle (target_channel_id), vaikka ne
        // ovat eri arvot. DM-kanava = 500, ryhmä-target = 100 → vastaus → 500.
        let dm_channel: u64 = 500;
        let group_target: u64 = 100;
        let env = map_message(
            OWNER_ID,
            false,
            dm_channel,
            group_target,
            "yksityisesti",
            SELF_ID,
            false,
            true,
            OWNER_ID,
        )
        .expect("owner DM heard");
        assert_eq!(
            env.channel_id,
            dm_channel.to_string(),
            "DM-vastaus menee DM-kanavalle (channel_id), ei ryhmään"
        );
        assert_eq!(env.conversation, dm_channel.to_string());
        assert_ne!(
            env.channel_id,
            group_target.to_string(),
            "DM-vastaus EI saa mennä ryhmäkanavalle (target_channel_id)"
        );
    }

    #[test]
    fn dm_from_non_owner_is_dropped() {
        // DM muulta ihmiseltä (ei owner) → pudotetaan (kahdenkeskinen vain omistajalle).
        assert!(map_message(42, false, 500, 100, "moi", SELF_ID, false, true, OWNER_ID).is_none());
    }

    #[test]
    fn dm_from_bot_is_dropped_even_if_owner_id_matches() {
        // DM botilta pudotetaan aina (vaikka id sattuisi owneriin).
        assert!(
            map_message(OWNER_ID, true, 500, 100, "moi", SELF_ID, false, true, OWNER_ID).is_none()
        );
    }

    #[test]
    fn whitespace_only_content_returns_none() {
        assert!(group(1, false, 100, 100, "", false).is_none());
        assert!(group(1, false, 100, 100, "   ", false).is_none());
        assert!(group(1, false, 100, 100, "\n\t", false).is_none());
    }

    #[test]
    fn u64_max_ids_as_decimal_strings() {
        let author = u64::MAX;
        let channel = u64::MAX;
        let env = group(author, false, channel, channel, "ping", false).expect("Some");
        assert_eq!(env.sender, author.to_string());
        assert_eq!(env.conversation, channel.to_string());
        assert_eq!(env.channel_id, channel.to_string());
    }

    #[test]
    fn body_is_not_trimmed_in_envelope() {
        let env = group(1, false, 100, 100, "  hello  ", false).expect("Some");
        assert_eq!(env.body, "  hello  ");
    }
}
