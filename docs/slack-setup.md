# Slack Bot Setup Guide

FamilyClaw's Slack adapter is an **outbound-only MVP** behind the `slack`
feature (enabled in the gateway build): outbound `chat.postMessage` works.

**Inbound does not work yet.** Socket Mode / Events API is not implemented, and
the gateway's `POST /inject` endpoint is wired to the Discord channel only — in
Slack mode it answers `503 discord channel not configured`. `SlackChannel`
exposes an `inject` method, but no HTTP route reaches it. See §4.

## 1. Create a Slack app

1. Open [api.slack.com/apps](https://api.slack.com/apps) → **Create New App**.
2. Add bot scopes: `chat:write`, `channels:history` (and `im:write` if DMs).
3. Install to workspace; copy the **Bot User OAuth Token** (`xoxb-…`).
4. Invite the bot to the channel you will use.

## 2. Configuration

Never commit tokens. Private env file:

```env
FAMILYCLAW_CHANNEL_KIND=slack
SLACK_BOT_TOKEN="xoxb-…"
FAMILYCLAW_SLACK_CHANNEL_ID="C0123456789"
FAMILYCLAW_REPLY_TARGET="C0123456789"
FAMILYCLAW_GATEWAY_TOKEN="long-random-secret"
```

`FAMILYCLAW_SLACK_CHANNEL_ID` is the logical channel instance id for routing;
`FAMILYCLAW_REPLY_TARGET` is the Slack conversation id used for replies
(usually the same channel id).

## 3. Approvals

When an action suspends for approval, post (or read in
[Reliability Console](CONSOLE.md)) the approval id. The helper
`SlackChannel::format_approval_prompt` renders Approve/Deny HTTP instructions
plus a `/console` link — Hermes-style visibility without leaving Slack.

## 4. Inbound — not implemented

There is currently **no** inbound path for Slack. Neither Events API / Socket
Mode nor `POST /inject` will deliver a Slack message to the agent:

```bash
$ curl -i -X POST http://127.0.0.1:8787/inject \
  -H "Content-Type: application/json" \
  -d '{"sender":"U123","chat_id":"C0123456789","body":"hello"}'
HTTP/1.1 503 Service Unavailable
discord channel not configured
```

`POST /inject` constructs a Discord envelope and requires a configured Discord
channel; it is not generic over channel kinds. Making it Slack-aware (or adding
an Events API handler) is open work, not a shipped feature.

## 5. Doctor

`doctor` does **not** know the `slack` channel kind yet. With
`FAMILYCLAW_CHANNEL_KIND=slack` it falls through to the Telegram branch and
demands `TELEGRAM_BOT_TOKEN` / `FAMILYCLAW_TELEGRAM_CHANNEL_ID`, so it exits 1
even when the Slack variables are all set:

```text
[INFO]     channel   slack
[MISSING] env       TELEGRAM_BOT_TOKEN
[MISSING] env       FAMILYCLAW_TELEGRAM_CHANNEL_ID
Error: InvalidInput("doctor: one or more checks failed")
```

`serve` itself does validate Slack config and fails closed on a missing
`SLACK_BOT_TOKEN` or `FAMILYCLAW_SLACK_CHANNEL_ID`. Until `doctor` learns the
`slack` kind, use `FAMILYCLAW_CHANNEL_KIND=none familyclaw-gateway doctor` for
the generic host checks (port, data dir, sandbox) and rely on `serve` for the
Slack-specific ones.
