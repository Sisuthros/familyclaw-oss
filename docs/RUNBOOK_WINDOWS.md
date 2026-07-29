# Windows — public demo and private setup

1. **Public demo (Layer A)** — first, no keys: [`QUICKSTART.md`](QUICKSTART.md), `scripts/public-demo.ps1`
2. **Private gateway (Layer B)** — your own profiles and channels outside the repo: [`LAYER_BOUNDARY.md`](LAYER_BOUNDARY.md)

## Public demo (Layer A)

No Telegram, no SOUL files, no secrets:

```powershell
cd <repo-root>
powershell -File scripts/public-demo.ps1
powershell -File scripts/public-demo.ps1 -Full   # + compare-bench
```

Or a single 10 s run:

```powershell
cargo run -p minimal-gateway -- --duration 10
```

## Prerequisites (Layer B)

- Rust 1.88+ ([`rustup`](https://rustup.rs/)) — 1.88 is the workspace MSRV (`Cargo.toml`, `rust-version`)
- Git
- PowerShell 5.1+ or PowerShell 7
- Repo cloned, e.g. `E:\Familyclaw`

Check Rust:

```powershell
rustc --version   # 1.88 or newer
```

## Directory structure (Layer B)

These paths are **local** — choose your own locations; they are not part of the git repo.

| Variable / path | Use |
|------------------|--------|
| `FAMILYCLAW_PROFILE_DIR` | Agent profiles (`SOUL.md`, `IDENTITY.md`) |
| `FAMILYCLAW_DATA_DIR` | Durable data (MVP: `memory.json`, `journal.jsonl`) |

Example (replace with your own paths):

```powershell
$profiles = Join-Path $env:USERPROFILE "familyclaw-profiles"
$data     = Join-Path $env:USERPROFILE "familyclaw-data"
New-Item -ItemType Directory -Force -Path $profiles, $data | Out-Null
```

Profile structure (generic `agent_a`):

```
%FAMILYCLAW_PROFILE_DIR%\
  agent_a\
    SOUL.md
    IDENTITY.md
```

The gateway loads the soul from the path `FAMILYCLAW_PROFILE_DIR\<agent_name>\` when `FAMILYCLAW_AGENT_NAME` is set (default `agent_a`).

In development you can initialize the JSON data in a repo-local `.local/data` (gitignored):

```powershell
powershell -File scripts/init-familyclaw-data.ps1
```

## Required environment variables (Layer B)

Copy [`.env.example`](../.env.example) from the repo to a private path and fill in the values:

```powershell
$configDir = Join-Path $env:USERPROFILE ".config" "familyclaw"
New-Item -ItemType Directory -Force -Path $configDir | Out-Null
Copy-Item .env.example "$configDir\familyclaw.env"
# edit familyclaw.env — do not commit
. .\scripts\load-env.ps1 -Path "$configDir\familyclaw.env"
```

| Variable | Description |
|----------|--------|
| `FAMILYCLAW_PROFILE_DIR` | Root of profiles |
| `FAMILYCLAW_DATA_DIR` | Durable memory directory |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot API token ([@BotFather](https://t.me/BotFather)) |
| `FAMILYCLAW_GATEWAY_TOKEN` | Shared secret for production (webhook/HTTP protection) |

The gateway also requires the following for the Telegram channel:

| Variable | Description |
|----------|--------|
| `FAMILYCLAW_TELEGRAM_CHANNEL_ID` | Logical channel identifier (e.g. `tg-main`) |
| `FAMILYCLAW_REPLY_TARGET` | Telegram chat ID for replies (numeric) |
| `FAMILYCLAW_AGENT_NAME` | Profile folder name (e.g. `agent_a`) |

LLM responses (optional but recommended in production):

```powershell
$env:FAMILYCLAW_PROVIDERS = "openai=https://api.openai.com/v1=OPENAI_API_KEY"
$env:OPENAI_API_KEY = "<your-key>"
```

Example session initialization:

```powershell
cd <repo-root>
. .\scripts\load-env.ps1 -Path "$env:USERPROFILE\.config\familyclaw\familyclaw.env"
```

Listens on localhost only by default. If you expose the port to the network, make sure to configure the firewall and `FAMILYCLAW_GATEWAY_TOKEN`.

## Build the gateway

```powershell
cd E:\Familyclaw
cargo build --release -p familyclaw-gateway --locked
```

Binary: `target\release\familyclaw-gateway.exe`

Alternative debug run without a separate install step:

```powershell
cargo build -p familyclaw-gateway
```

## Start the gateway

```powershell
# Pre-check (does not start the server)
cargo run -p familyclaw-gateway -- doctor

# Start (default: serve)
cargo run -p familyclaw-gateway -- serve

# Or from the release binary:
.\target\release\familyclaw-gateway.exe serve
```

Health checks in another window:

```powershell
curl.exe -i http://127.0.0.1:8787/healthz
curl.exe -i http://127.0.0.1:8787/readyz
```

Status query via CLI:

```powershell
cargo run -p familyclaw-gateway -- status
```

Shutdown: `Ctrl+C` (the gateway invokes a clean `shutdown` path).

## MVP JSON vs SurrealDB (The Hearth)

### MVP — recommended for Windows first

When `FAMILYCLAW_DATA_DIR` is set, the runtime uses **JSON files**:

| File | Contents |
|----------|---------|
| `%FAMILYCLAW_DATA_DIR%\journal.jsonl` | Durable journal (crash replay) |
| `%FAMILYCLAW_DATA_DIR%\memory.json` | `LocalJsonStore` — working memory on disk |

Without `FAMILYCLAW_DATA_DIR`, memory lives only in the process's RAM (lost on restart).

This path is single-process and safe for Windows development — **no RocksDB LOCK conflicts**.

### Later — SurrealDB + RocksDB (The Hearth)

`familyclaw-hearth` supports a SurrealDB 3.x connection:

- Development: `mem://`
- Production (file): `rocksdb:///<absolute-path>/hearth`

Build with the feature flag once Hearth is adopted:

```powershell
cargo build -p familyclaw-hearth --features surreal
```

Identity anchor (optional):

```powershell
$env:FAMILYCLAW_HEARTH_ENABLED = "1"
```

**Do not use the JSON MVP and the RocksDB hearth simultaneously in the same directory without separate subfolders.** Keep the MVP JSON at the root (`memory.json`, `journal.jsonl`) and the hearth in a separate subfolder (`hearth\`).

## RocksDB LOCK — one process at a time

RocksDB allows only **one writing process** per database path. Typical error:

```
Resource temporarily unavailable
IO error: lock hold by current process
```

### Rule

**Only one process at a time** may open a RocksDB database path.

### Fix

1. Stop all gateway instances:

```powershell
Get-Process familyclaw-gateway -ErrorAction SilentlyContinue | Stop-Process -Force
```

2. Close programs that hold the data folder open (Cursor/VS Code explorer, another terminal running `cargo run`, an old `continuity_daemon`).

3. **During the MVP stage** use only the JSON path (`FAMILYCLAW_DATA_DIR` without SurrealDB/RocksDB hearth) — recommended in the parallel-agent plan.

4. If the LOCK persists after a process crash, recheck the process list. Remove the `LOCK` file **only** once you've confirmed no process is using the database.

5. Do not run the benchmark (`continuity_daemon`) and the gateway against the **same** data directory at the same time.

## Configuration (TOML)

An optional TOML file complements the env variables. Copy the example:

```powershell
$configDir = "$env:USERPROFILE\.config\familyclaw"
New-Item -ItemType Directory -Force -Path $configDir
Copy-Item familyclaw.toml.example "$configDir\familyclaw.toml"
# Edit: agent.name, channel.telegram, provider — keep secrets in env
```

Or point to it explicitly:

```powershell
$env:FAMILYCLAW_CONFIG = "$configDir\familyclaw.toml"
```

## Validation (bench + CI)

Run before shipping to production or merging:

```powershell
cd E:\Familyclaw

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings -A clippy::doc_markdown -A clippy::too_many_lines
cargo test --workspace
```

Continuity benchmark (requires the `continuity_daemon` binary — built automatically):

```powershell
cargo build -p familyclaw-agent --bin continuity_daemon
cargo run -p familyclaw-bench --bin bench -- all
```

Results:

- `crates\familyclaw-bench\out\scorecard.json`
- `crates\familyclaw-bench\out\SCORECARD.md`
- `docs\SCORECARD.md` (updated on the `all` run)

Expected summary: **Overall: PASS** (see [`SCORECARD.md`](SCORECARD.md)).

Single scenario:

```powershell
cargo run -p familyclaw-bench --bin bench -- s1   # crash matrix
cargo run -p familyclaw-bench --bin bench -- s3   # dream quality
```

## E2E: gateway + Telegram (Layer B)

Once the private env is loaded and `%FAMILYCLAW_PROFILE_DIR%\agent_a\SOUL.md` exists:

```powershell
. .\scripts\load-env.ps1 -Path "$env:USERPROFILE\.config\familyclaw\familyclaw.env"
.\scripts\e2e-gateway.ps1 -StartGateway
```

Send a message to the Telegram bot → expect a reply drawing on the agent's SOUL and memory, surviving a restart.

## Troubleshooting

**`TELEGRAM_BOT_TOKEN must be set`**

- The token is missing from env or the TOML is empty — set the env var (env wins over TOML).

**Gateway doesn't respond on `/readyz`**

- The bus didn't start — check the logs; run `doctor`.

**Memory clears on restart**

- `FAMILYCLAW_DATA_DIR` wasn't set → run `scripts/init-familyclaw-data.ps1` or set the path in env.

**RocksDB LOCK / data folder locked**

- See [RocksDB LOCK](#rocksdb-lock--one-process-at-a-time) — close competing processes, use the JSON MVP first.

**`cargo` is missing**

- Install Rust: [`rustup.rs`](https://rustup.rs/)

**Tests fail in parallel**

```powershell
cargo test --workspace -- --test-threads=1
```

---

FamilyClaw: agents that remember, feel, dream, and think — in Windows production, Layer B stays private.
