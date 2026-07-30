# Deployment Guide

FamilyClaw is designed as a public Layer A runtime that loads private Layer B data only at runtime. Deployment must preserve that boundary: container images, repository files, CI logs, and public examples must not contain private profiles, real memories, API keys, tokens, or machine-specific paths.

This guide covers a production-like gateway deployment. It intentionally uses generic names only.

## Runtime shape

The gateway binary provides:

- `/healthz` for process liveness
- `/readyz` for runtime readiness
- `/inject` for supervised inbound testing
- `/discord/interactions` when Discord public-key verification is configured
- channel-to-bus-to-agent routing through the runtime assembly layer

The gateway should be deployed as a long-running process with a persistent data directory mounted outside the image.

## Required principles

- Build images from Layer A only.
- Keep private profiles outside the repository.
- Mount or inject secrets at runtime.
- Never bake `SOUL.md`, calibration files, memory files, journals, API keys, tokens, or real profiles into an image.
- Prefer explicit environment variables over checked-in local config.
- Treat logs as public unless proven otherwise.

## Build locally

```bash
cargo build --release -p familyclaw-gateway
```

Run locally:

```bash
FAMILYCLAW_GATEWAY_ADDR=127.0.0.1:8787 \
FAMILYCLAW_CONFIG=/absolute/private/familyclaw.toml \
FAMILYCLAW_PROFILE_DIR=/absolute/private/profiles \
FAMILYCLAW_DATA_DIR=/absolute/private/data \
cargo run -p familyclaw-gateway -- serve
```

Check health:

```bash
curl http://127.0.0.1:8787/healthz
curl http://127.0.0.1:8787/readyz
```

## Core environment variables

| Variable | Required | Purpose |
| --- | --- | --- |
| `FAMILYCLAW_GATEWAY_ADDR` | Recommended | Socket address. Default is local-only. Use `0.0.0.0:PORT` inside containers when required by the platform. |
| `FAMILYCLAW_CONFIG` | Recommended | Absolute path to private `familyclaw.toml`. |
| `FAMILYCLAW_PROFILE_DIR` | Optional but expected for private agents | Private profile root. Must live outside the repository. |
| `FAMILYCLAW_DATA_DIR` | Recommended | Persistent runtime data directory for journals and stores. Mount this as durable storage. **The directory must already exist** — see the note below. |
| `FAMILYCLAW_PROVIDERS` | Required for text replies | Provider map in `prefix=base_url=KEY_ENV` form. Endpoints must be OpenAI-compatible chat-completions endpoints unless a native adapter exists. |
| `FAMILYCLAW_REPLY_TARGET` | Required for static fallback routing | Fallback outbound target when per-message origin is unavailable. |
| `FAMILYCLAW_GATEWAY_TOKEN` | Required in production | Bearer token for `/inject`. Empty token is acceptable only for local loopback development. |

Example provider map:

```bash
FAMILYCLAW_PROVIDERS="openai=https://api.openai.com/v1=OPENAI_API_KEY;local=http://127.0.0.1:11434/v1=LOCAL_API_KEY"
```

The value after the second `=` is the name of another environment variable. Do not put the API key itself inside `FAMILYCLAW_PROVIDERS`.

### `FAMILYCLAW_DATA_DIR` must exist before first start

The gateway does **not** create the data directory for you. It opens the durable
journals fail-closed, so pointing `FAMILYCLAW_DATA_DIR` at a path that does not
exist yet makes both `doctor` and `serve` abort with a raw OS error rather than a
readable message:

```text
Error: Config("durable action stores open failed: proof error:
open pending journal failed: journal io error: <OS 'path not found' error>")
```

Create it first:

```bash
mkdir -p /absolute/private/data          # Linux / macOS
```

```powershell
powershell -File scripts/init-familyclaw-data.ps1   # Windows (also seeds the store files)
```

Verified behaviour: with the directory missing, `doctor` exits `1`; with the
directory present, `doctor` reports
`durability persistent (data_dir set); dispatch_outbox=journal; pending_store=journal`
and exits `0`. Leaving `FAMILYCLAW_DATA_DIR` unset is also fine for evaluation —
`doctor` then exits `0` but warns that durability is in-memory and the
at-most-once-under-crash guarantee does **not** hold.

## Discord variables

| Variable | Required | Purpose |
| --- | --- | --- |
| `FAMILYCLAW_CHANNEL_KIND=discord` | Required for Discord mode | Selects Discord channel mode. |
| `DISCORD_WEBHOOK_URL` | Required for current Discord webhook mode | Runtime-provided Discord webhook URL. Treat as secret. |
| `DISCORD_CHANNEL_ID` | Required | Discord channel identifier or configured channel label. |
| `DISCORD_PUBLIC_KEY` | Required for `/discord/interactions` | Discord application public key for Ed25519 verification. |

Notes:

- Discord interactions verify `timestamp || raw_body` with Ed25519.
- `/discord/interactions` is enabled only when a public key and Discord channel are configured.
- Outbound Discord behavior must remain explicit in code and docs. Do not assume webhook and bot-token modes are interchangeable.

## Telegram variables

| Variable | Required | Purpose |
| --- | --- | --- |
| `FAMILYCLAW_CHANNEL_KIND=telegram` | Required for Telegram mode | Selects Telegram channel mode. |
| `TELEGRAM_BOT_TOKEN` | Required | Telegram bot token. Treat as secret. |
| `FAMILYCLAW_TELEGRAM_CHANNEL_ID` | Required | Target Telegram chat/channel. |

## Private runtime directory

Keep private runtime state outside the repository:

```text
private-family/
  profiles/
    agent_a/
      SOUL.md
      calibration.json
  data/
    journal.jsonl
    memory.json
    anchors.json
  backups/
  familyclaw.toml
  familyclaw.env
```

Rules:

- This directory is Layer B.
- It must not be copied into Docker images.
- It must not be committed.
- It should be backed up separately.
- Production deployments should mount `data/` as durable storage.

## Docker baseline

When a Dockerfile is present, the image must follow these rules:

- Multi-stage build.
- Runtime image contains the gateway binary only.
- Runtime image does not contain private profiles or data.
- Non-root runtime user where practical.
- Health check uses `/healthz`.
- Persistent storage is mounted into `FAMILYCLAW_DATA_DIR`.

Example run shape:

```bash
docker run --rm \
  -p 8787:8787 \
  -e FAMILYCLAW_GATEWAY_ADDR=0.0.0.0:8787 \
  -e FAMILYCLAW_CONFIG=/run/secrets/familyclaw.toml \
  -e FAMILYCLAW_PROFILE_DIR=/private/profiles \
  -e FAMILYCLAW_DATA_DIR=/data \
  -v /secure/private-family/profiles:/private/profiles:ro \
  -v /secure/private-family/data:/data \
  familyclaw-gateway:local
```

## Railway-style deployment

- Set `FAMILYCLAW_GATEWAY_ADDR=0.0.0.0:$PORT` if the platform provides `PORT`.
- Add secrets through the platform dashboard.
- Mount persistent storage for `FAMILYCLAW_DATA_DIR` if available.
- Do not store private profiles in the repository.
- Use `/healthz` as the liveness check.
- Use `/readyz` as the readiness check.

## Fly-style deployment

- Bind the gateway to `0.0.0.0:<internal_port>`.
- Mount a Fly volume for `FAMILYCLAW_DATA_DIR`.
- Inject provider keys and channel tokens as secrets.
- Keep private profiles in a mounted private volume or secure runtime secret bundle.
- Verify `/healthz` and `/readyz` after deploy.

## Cloud Run-style deployment

- Bind to `0.0.0.0:$PORT`.
- Use Secret Manager or platform secrets for tokens and provider keys.
- Cloud Run filesystem is ephemeral by default. Do not rely on local disk for durable continuity unless backed by an external persistent store or mounted volume.
- For durable memory, prefer an attached persistent backend or explicitly documented storage strategy.
- Keep `/healthz` and `/readyz` reachable.

## Smoke test

```bash
curl -fsS http://127.0.0.1:8787/healthz
curl -fsS http://127.0.0.1:8787/readyz
```

`/readyz` runs `llm_ping` + `llm_tools_ping` and returns `503` if the
configured provider is unreachable — a production deploy without a working
`FAMILYCLAW_PROVIDERS` entry is expected to fail this smoke test. The single
exception is the keyless demo mode (`FAMILYCLAW_CHANNEL_KIND=none` *and* no
`FAMILYCLAW_PROVIDERS`), where the LLM probes are skipped and reported in the
response's `degraded` array instead; do not run production that way. Use
`POST /canary` when you want an unconditional live-LLM assertion.

If `/inject` is enabled, use a bearer token in production:

```bash
curl -X POST http://127.0.0.1:8787/inject \
  -H "Authorization: Bearer $FAMILYCLAW_GATEWAY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"sender":"operator","conversation":"local-test","body":"hello"}'
```

## Deployment readiness checklist

Before deploying:

- `bash scripts/audit-layer-b.sh` passes.
- Tests pass using the documented living-feature matrix.
- `cargo fmt --all -- --check` passes.
- `cargo clippy --workspace --all-targets --features discord -- -D warnings` passes.
- Provider endpoints are OpenAI-compatible unless a native adapter is used.
- `FAMILYCLAW_GATEWAY_TOKEN` is set outside local loopback development.
- Private profiles and runtime data live outside the image.
- `/healthz` and `/readyz` are verified after start.
