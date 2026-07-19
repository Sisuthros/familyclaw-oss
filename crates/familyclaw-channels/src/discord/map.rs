//! Discord message filtering and conversion into an [`InboundEnvelope`].
//!
//! [`map_message`] encapsulates the fields extracted from a gateway event
//! into a canonical inbound envelope. Filtering happens before the
//! [`InboundMessage::new`] call so that empty and bot messages never reach
//! the Resonance Bus.

use crate::message::{ChannelKind, InboundEnvelope, InboundMessage};

/// Filters and converts a Discord message into an [`InboundEnvelope`].
///
/// Two paths:
///
/// **Direct message (DM, `is_dm == true`):** passes through ONLY if
/// `author_id == owner_id` (the operator) and the author is not a bot. This
/// is the one-on-one conversation between the operator and the agent — no
/// one else can DM the agent. The reply is routed back to the DM channel
/// (`channel_id`), NOT to the group channel.
///
/// **Group channel (`is_dm == false`):** only processed on the target
/// channel (`channel_id == target_channel_id`). Returns `None` if:
/// - `author_id == self_id` (own message — self-echo protection, ALWAYS applied),
/// - `author_is_bot && !mentions_me` (another bot that did NOT mention us —
///   prevents a bot-to-bot infinite loop; peer agent bots are heard when
///   they @-mention us, but unprompted bot chatter does not trigger the loop).
///
/// In both cases: an empty/whitespace-only `content` → `None`.
///
/// Multi-agent interoperability (2026-06-21): peer instances are Discord
/// bots. The old "drop all bots" behavior made the system effectively
/// write-only. Now peer bots are heard via mention (`mentions_me`), `self_id`
/// prevents self-echo, and the operator gets their own DM channel with the
/// agent (`owner_id` + `is_dm`).
///
/// On success, `sender` and `conversation` are decimal strings, `body` is
/// kept as-is (not trimmed), `kind` is [`ChannelKind::Discord`]. In a DM, the
/// envelope's `channel_id` is the DM channel's id (the reply goes there); in
/// a group, it is the target channel's id.
///
/// # Examples
///
/// ```
/// use familyclaw_channels::discord::map::map_message;
/// use familyclaw_channels::ChannelKind;
///
/// // A human on a group channel: passes through without needing a mention.
/// // (author=42, not a bot, channel=100=target, self=7, not a DM, owner=5)
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
    // Own message → never processed (self-echo protection, ALWAYS first).
    if author_id == self_id {
        return None;
    }
    if content.trim().is_empty() {
        return None;
    }

    if is_dm {
        // Direct message: ONLY the operator (owner_id), not a bot. A
        // one-on-one conversation — the reply is routed back to the DM
        // channel (channel_id).
        if author_is_bot || author_id != owner_id {
            return None;
        }
        let inbound =
            InboundMessage::new(author_id.to_string(), channel_id.to_string(), content).ok()?;
        return Some(inbound.into_envelope(ChannelKind::Discord, channel_id.to_string()));
    }

    // Group channel: only the target channel.
    if channel_id != target_channel_id {
        return None;
    }
    // Another bot (a peer agent) is heard ONLY when it mentions us — this
    // prevents a structural bot-to-bot infinite loop. Humans (non-bots)
    // always pass through.
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

    // Constants for readability: self_id = 9, owner (operator) = 5.
    const SELF_ID: u64 = 9;
    const OWNER_ID: u64 = 5;

    // Helper for group channel messages (is_dm=false, owner=OWNER_ID).
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
        // Self-echo protection: our own message is ALWAYS dropped, even if it "mentions" itself.
        assert!(group(SELF_ID, true, 100, 100, "moi", true).is_none());
        assert!(group(SELF_ID, false, 100, 100, "moi", true).is_none());
    }

    #[test]
    fn family_bot_heard_only_when_mentioned() {
        // A peer bot that does NOT mention us → dropped (infinite-loop protection).
        assert!(group(1, true, 100, 100, "moi", false).is_none());
        // A peer bot that DOES mention us → heard.
        let env = group(1, true, 100, 100, "moi @agent", true).expect("Some");
        assert_eq!(env.sender, "1");
    }

    #[test]
    fn human_heard_without_mention() {
        // A human passes through even without a mention (the mention gate only applies to bots).
        let env = group(1, false, 100, 100, "moi", false).expect("Some");
        assert_eq!(env.body, "moi");
    }

    #[test]
    fn dm_from_owner_is_heard_and_replies_to_dm_channel() {
        // A DM from the operator (owner): passes through, reply goes to the
        // DM channel (channel_id 500). target_channel_id 100 (group) is
        // ignored in a DM.
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
        // The reply is routed to the DM channel (500), NOT to the group (100).
        assert_eq!(env.channel_id, "500");
        assert_eq!(env.conversation, "500");
    }

    #[test]
    fn dm_reply_target_is_dm_channel_not_group_channel() {
        // Explicit guard for rule 9: in a DM, the reply is routed to the DM
        // channel (channel_id), NOT to the group channel (target_channel_id),
        // even though these are different values. DM channel = 500,
        // group target = 100 → reply → 500.
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
            "the DM reply must go to the DM channel (channel_id), not the group"
        );
        assert_eq!(env.conversation, dm_channel.to_string());
        assert_ne!(
            env.channel_id,
            group_target.to_string(),
            "the DM reply must NOT go to the group channel (target_channel_id)"
        );
    }

    #[test]
    fn dm_from_non_owner_is_dropped() {
        // A DM from another human (not the owner) → dropped (one-on-one is owner-only).
        assert!(map_message(42, false, 500, 100, "moi", SELF_ID, false, true, OWNER_ID).is_none());
    }

    #[test]
    fn dm_from_bot_is_dropped_even_if_owner_id_matches() {
        // A DM from a bot is always dropped (even if its id happens to match the owner).
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
