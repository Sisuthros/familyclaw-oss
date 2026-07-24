# Slack Bot Setup Guide

FamilyClaw's Slack adapter is an MVP behind the `slack` feature (enabled in
the gateway build): outbound `chat.postMessage`, inbound via
[`SlackChannel::inject`] (gateway `/inject` or a future Events API handler).
Socket Mode is deferred.

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

## 4. Inbound MVP

Until Events API / Socket Mode lands, feed inbound messages with:

```bash
curl -X POST http://127.0.0.1:8787/inject \
  -H "Authorization: Bearer $FAMILYCLAW_GATEWAY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"sender":"U123","chat_id":"C0123456789","body":"hello"}'
```

## 5. Doctor

```bash
FAMILYCLAW_CHANNEL_KIND=slack familyclaw-gateway doctor
```

Missing `SLACK_BOT_TOKEN` fails closed.
