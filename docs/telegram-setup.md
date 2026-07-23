# Telegram Bot Setup Guide

This guide walks you through creating a Telegram bot and configuring it for the
FamilyClaw system.

## 1. Create the bot with BotFather
1. Open Telegram and start a chat with [@BotFather](https://t.me/BotFather).
2. Send `/newbot` and follow the prompts (display name, then a username ending
   in `bot`).
3. BotFather replies with an **HTTP API token**. Copy it somewhere safe — you
   will put it in an environment variable, never in git.

## 2. Find your chat / reply target
FamilyClaw needs a numeric Telegram **chat id** for outbound replies
(`FAMILYCLAW_REPLY_TARGET`), plus a logical channel instance id for routing
(`FAMILYCLAW_TELEGRAM_CHANNEL_ID`).

Common ways to learn the chat id:

1. Message your bot in a private chat (or add it to a group and send a message).
2. Long-poll once with the Bot API (replace `<token>`):

   ```bash
   curl "https://api.telegram.org/bot<token>/getUpdates"
   ```

3. In the JSON, find `message.chat.id` (a number, sometimes negative for groups).

Use that number as `FAMILYCLAW_REPLY_TARGET`. Pick any stable label you like for
`FAMILYCLAW_TELEGRAM_CHANNEL_ID` (for example `tg-main`).

## 3. Configuration
**IMPORTANT:** Never commit the bot token to version control! Keep tokens in a
private env file outside the repo (see [LAYER_BOUNDARY.md](LAYER_BOUNDARY.md)).

Create or extend your private env file with:

```env
FAMILYCLAW_CHANNEL_KIND=telegram
TELEGRAM_BOT_TOKEN="Copy_the_bot_token_from_BotFather_here"
FAMILYCLAW_TELEGRAM_CHANNEL_ID=tg-main
FAMILYCLAW_REPLY_TARGET="123456789"
```

| Variable | Required | Purpose |
|---|---|---|
| `FAMILYCLAW_CHANNEL_KIND=telegram` | Yes (for Telegram mode) | Selects the Telegram channel adapter. |
| `TELEGRAM_BOT_TOKEN` | Yes | Bot API token from BotFather. Treat as secret. |
| `FAMILYCLAW_TELEGRAM_CHANNEL_ID` | Yes | Logical channel instance id used for routing. |
| `FAMILYCLAW_REPLY_TARGET` | Yes | Numeric Telegram chat id for replies. |

Build/run the gateway with the `telegram` feature enabled on
`familyclaw-channels` (CI and local matrix already cover this path).

### Long-poll basics
FamilyClaw's Telegram adapter talks to the Bot API over HTTP (`getUpdates` /
`sendMessage`). It does **not** require a public webhook URL.

- **Receive:** a background task long-polls `getUpdates` (server-side block,
  typically ~30s). When updates arrive, each text message is canonicalized into
  an inbound envelope and pushed onto the channel stream.
- **Acknowledge:** after each batch, the next poll uses
  `offset = max(update_id) + 1`, so Telegram stops redelivering those updates.
- **Send:** replies go out via `sendMessage` to the conversation's chat id
  (the same `chat.id` carried on the inbound envelope / reply target).

Do not run a second long-poll client against the same bot token at the same
time — Telegram delivers each update to only one `getUpdates` consumer.

## 4. Approval flow notes
Channel traffic and **operator approval** are separate concerns:

- Messages from Telegram enter the agent loop like any other channel.
- When a skill requires human approval, the tool loop **suspends**. No automatic
  approval is granted from a Telegram message.
- Operators list and grant approvals on the gateway's bearer-protected routes
  (same `FAMILYCLAW_GATEWAY_TOKEN` as `/inject`):

  ```bash
  # List pending (redacted summaries only — never raw payloads)
  curl -s -H "Authorization: Bearer $FAMILYCLAW_GATEWAY_TOKEN" \
    http://127.0.0.1:8787/approvals/pending

  # Approve one (payload-bound, single-use)
  curl -s -X POST -H "Authorization: Bearer $FAMILYCLAW_GATEWAY_TOKEN" \
    http://127.0.0.1:8787/approvals/<approval_id>/approve
  ```

Approvals remain fail-closed: a changed payload cannot reuse a granted
approval. See [SECURITY_MODEL.md](SECURITY_MODEL.md) and the gateway docs for
the full surface.

## 5. Troubleshooting

| Problem / Symptom | Cause and solution |
|----------------|-----------------|
| Gateway refuses to start Telegram mode | `FAMILYCLAW_CHANNEL_KIND` is not `telegram`, or `TELEGRAM_BOT_TOKEN` / channel id / reply target is missing or empty. |
| HTTP 401 Unauthorized from Bot API | Wrong or revoked token. Regenerate with BotFather and update `TELEGRAM_BOT_TOKEN`. |
| No inbound messages (silence) | Another process is already long-polling the same bot; or the chat id / reply target does not match the chat you are messaging. Confirm with a one-shot `getUpdates`. |
| Bot receives but never replies | `FAMILYCLAW_REPLY_TARGET` points at the wrong chat, or the agent suspended awaiting operator approval (`GET /approvals/pending`). |
| Group chat oddities | Privacy mode may hide non-command messages from the bot. Disable privacy via BotFather (`/setprivacy`) or @-mention the bot / use commands, depending on your setup. |
| Feature / compile errors for Telegram | Build with `--features familyclaw-channels/telegram` (or the gateway crate's telegram feature set). |
