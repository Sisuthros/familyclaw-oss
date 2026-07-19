# Discord Bot Setup Guide

This guide walks you through creating a Discord bot and configuring it for the
FamilyClaw system.

## 1. Create the bot in the Discord Developer Portal
1. Sign in to the [Discord Developer Portal](https://discord.com/developers/applications).
2. Click **New Application** in the top-right corner.
3. Give the application a name (e.g. FamilyClaw) and accept the terms. Click **Create**.
4. Select **Bot** from the left-hand menu.

## 2. Privileged Intents
For the bot to be able to read message content, you must enable the
`MESSAGE_CONTENT` intent. Without this privileged feature, message content is
empty and the bot cannot react to channel events.

1. On the Bot page, find the **Privileged Gateway Intents** section.
2. Enable **Message Content Intent**.
3. Save your changes (Save Changes).

*Note: Bots present in more than 100 servers require a separate Discord review
and approval for this intent.*

## 3. Inviting the bot to a server
1. From the left-hand menu, go to **OAuth2** -> **URL Generator**.
2. Under **Scopes**, select `bot`.
3. Under **Bot Permissions**, select:
   - `View Channels`
   - `Send Messages`
   - `Read Message History`
4. Copy the generated URL at the bottom of the page (the permissions value is
   included as a bitmask, e.g. `permissions=68608`).
5. Open the copied link in a browser and choose the server you want to add the
   bot to.

## 4. Finding the Channel ID
1. Open the Discord app.
2. Go to settings: **User Settings** -> **Advanced**.
3. Enable **Developer Mode**.
4. Right-click the text channel you want the bot to use and select **Copy
   Channel ID**.

## 5. Configuration
**IMPORTANT:** Never commit the bot token to version control! Add the `.env`
file to `.gitignore`.

Create a file named `.env` in the project root and add the following lines:
```env
DISCORD_BOT_TOKEN="Copy_the_bot_token_from_the_Bot_page_here"
DISCORD_CHANNEL_ID="Copy_the_channel_ID_here"
```

### Bidirectional bot mode vs. webhook posting
- **`DISCORD_BOT_TOKEN` set** → the gateway starts a serenity gateway
  connection: the bot **listens AND posts** (bidirectional). This is the
  recommended mode.
- **Only `DISCORD_WEBHOOK_URL` set** (no bot token) → the bot is **send-only**
  (posts via webhook, does not listen for messages).

### Optional: one-on-one DM with the owner
The owner ID can be set in two ways. **The env value overrides the TOML
value.**

As an env variable:
```env
FAMILYCLAW_OWNER_ID="Your-Discord-user-id"
```

Or in `familyclaw.toml`:
```toml
[channel.discord]
owner_id = 123456789012345678
```

If set, only this user can converse with the bot via **direct message** (DM);
the reply is routed back to the DM channel. Without this (missing, `0`, or an
invalid value), DMs are dropped — it is never "all DMs allowed" — and only the
group channel `DISCORD_CHANNEL_ID` is active. An invalid `FAMILYCLAW_OWNER_ID`
(not a number) is ignored with a warning, and the TOML/default value is kept.
In the group channel, humans pass straight through; other bots are only heard
when they @-mention the bot (this prevents a bot-to-bot loop).

## 6. Troubleshooting

| Problem / Symptom | Cause and solution |
|----------------|-----------------|
| Empty message content | The `MESSAGE_CONTENT` intent is missing. Enable it in the Discord Developer Portal. |
| HTTP 401 Unauthorized | Wrong or expired token. Check `DISCORD_BOT_TOKEN`. |
| "Missing Access" or HTTP 403 | The bot is not in that channel, or it lacks read/write permissions. |
| No events (silence) | Wrong channel ID in the `DISCORD_CHANNEL_ID` variable, or the bot is connected but not receiving guild messages (the bot needs the `GUILDS` intent — without it the guild stays permanently *unavailable* and no `MESSAGE_CREATE` ever arrives; this is built-in, not user-configurable). |
