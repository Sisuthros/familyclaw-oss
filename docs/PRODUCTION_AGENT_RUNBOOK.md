# Production Agent Runbook

This runbook turns the FamilyClaw reliability core into a useful, observable,
long-running agent. It is deliberately strict: a process that is alive but
cannot think, call tools, write inside its workspace, or reach its operator is
**not production-ready**.

## Why this gate exists

FamilyClaw has safe defaults that can look deceptively healthy during setup:

- An unresolved LLM chain does not crash the runtime. The agent can remain alive
  for memory and emotion processing while producing no text replies.
- `fs_read`, `file_write`, and `shell_exec` remain registered when their scopes
  are empty, but execution fails closed.
- A bare model identifier does not identify a provider. Use `provider/model`.
- Discord direct messages are denied unless a numeric operator ID is configured.
- The action runtime can suspend a turn for approval. The approval must be
  completed through the gateway approval surface or a supported operator
  approval command, not inferred from conversational consent.

These are correct safety properties. The production gate makes their operational
consequences explicit before an agent is declared ready.

There is **no opt-in strictness flag**. `/readyz` is fail-closed by default:

- a configured tool scope pointing at a directory that does not exist is a
  **503**, not a warning — the skill is silently dead and nothing else would
  tell you;
- an empty tool scope is reported under `degraded`, because a locked-down
  deployment is legitimate but must stay visible;
- the LLM checks are skipped only in keyless demo mode
  (`FAMILYCLAW_CHANNEL_KIND=none` with no provider table), and the skip is
  likewise reported under `degraded`.

A deployment does not become strict by setting an environment variable; it is
strict unless it explicitly asked for the keyless demo path.

## 1. Build the serving binary

For a production agent that may run third-party skills, compile the real Wasmtime
sandbox backend:

```powershell
cargo build --release -p familyclaw-gateway --features wasmtime
```

Add `ollama` only when local semantic embeddings are intentionally configured:

```powershell
cargo build --release -p familyclaw-gateway --features "wasmtime,ollama"
```

A build without `wasmtime` stays fail-closed for third-party code. The offline
`doctor` command reports the compiled backend truthfully.

## 2. Create a private runtime configuration

Copy `.env.example` outside the repository:

```powershell
$ConfigDir = Join-Path $env:USERPROFILE ".config\familyclaw"
New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
Copy-Item .env.example (Join-Path $ConfigDir "familyclaw.env")
```

Edit the private file and set, at minimum:

- `FAMILYCLAW_PROFILE_DIR`
- `FAMILYCLAW_DATA_DIR`
- `FAMILYCLAW_AGENT_NAME`
- `FAMILYCLAW_GATEWAY_TOKEN`
- `FAMILYCLAW_PROVIDER_MODEL`
- `FAMILYCLAW_PROVIDERS`
- every API-key environment variable referenced by `FAMILYCLAW_PROVIDERS`
- `FAMILYCLAW_FS_READ_ALLOW`
- `FAMILYCLAW_FILE_WRITE_ALLOW`
- the active channel credentials and operator identity

Use a dedicated agent workspace for tool scopes. Do not allow an entire user
home directory merely to make setup convenient.

On Windows, PATH-style read/write lists use semicolons. `shell_exec` currently
uses semicolon-separated working-directory roots on every platform.

## 3. Load and verify before serving

```powershell
$EnvFile = Join-Path $env:USERPROFILE ".config\familyclaw\familyclaw.env"
. .\scripts\load-env.ps1 -Path $EnvFile
powershell -ExecutionPolicy Bypass -File scripts\production-agent-doctor.ps1 -SkipLive
```

The offline gate validates:

- durable directories exist and are writable,
- the primary and fallback models use `provider/model` syntax,
- provider entries are parseable and their referenced key variables are set,
- read, write, and shell scopes exist,
- the channel and operator gate are configured,
- the repository's own `familyclaw-gateway doctor` passes,
- no secret value is printed.

Use `-Fix` to create missing directories and invoke `doctor --fix`. It does not
invent credentials, widen scopes, or replace operator decisions.

## 4. Start the gateway

From the repository:

```powershell
.\target\release\familyclaw-gateway.exe serve
```

Keep it in the foreground for the first proof run. Confirm that startup logs
show a resolved model chain, the intended channel connection, configured tool
scope counts, durable stores, and the real sandbox backend.

## 5. Run the live production gate

In a second terminal with the same private environment loaded:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\production-agent-doctor.ps1
```

A passing live gate proves:

- `/healthz` responds,
- `/readyz` passes the bus, LLM completion, LLM tool-calling, channel, journal
  and workspace-scope checks exposed by the running gateway,
- anything the gateway deliberately skipped is listed under `degraded` — the
  doctor prints each entry as a warning, so a reduced-capability deployment
  cannot pass unnoticed,
- `/canary` completes a synthetic production turn,
- the offline configuration and capability boundary are still valid.

The script exits non-zero on any required failure, so it can be used by a
Scheduled Task, deployment script, or service wrapper.

## 6. Prove useful work, not just connectivity

Run one bounded acceptance sequence through the real operator channel:

1. Ask the agent to read a known file inside `FAMILYCLAW_FS_READ_ALLOW` and
   report a distinctive line.
2. Ask it to create a small receipt file inside `FAMILYCLAW_FILE_WRITE_ALLOW`.
3. Approve the suspended write through the gateway approval surface when
   required.
4. Restart the gateway and ask the agent to recall the acceptance result.
5. Confirm the receipt exists once, the turn audit contains the tool dispatch,
   and no duplicate external action occurred.

Do not accept prose such as “done” as evidence. Verify the file, audit event, or
external side effect directly.

## Failure map

| Symptom | Most likely cause | Corrective action |
|---|---|---|
| Process is healthy but the agent never replies | Provider cannot be resolved, key is missing, or model ID is bare | Fix `FAMILYCLAW_PROVIDER_MODEL`, `FAMILYCLAW_PROVIDERS`, and referenced key variables; rerun the gate |
| Text replies work but file reads fail | `FAMILYCLAW_FS_READ_ALLOW` is empty or the requested path escapes it | Scope the dedicated workspace root; do not disable the guard |
| Reads work but writes always suspend or fail | Approval is pending, write scope is empty, or path escapes the scope | Configure `FAMILYCLAW_FILE_WRITE_ALLOW`, inspect `/approvals/pending`, approve the exact payload |
| Tool turns time out or end empty | Provider tool-calling is unsupported, request timeout is too small, or every fallback is cooling down | Inspect `llm_tools_ping`, configure valid fallbacks, and keep bounded timeouts |
| Discord channel works but direct messages disappear | `FAMILYCLAW_OWNER_ID` is unset or does not match the sender | Configure the numeric operator user ID |
| Third-party skill returns `NotImplemented` | `FAMILYCLAW_SANDBOX_SKILLS=1` but the binary lacks `wasmtime` | Rebuild with `--features wasmtime` |
| Agent behaves like a canned workflow instead of a general worker | A deployment-specific fast path or profile instruction is intercepting the turn | Disable/remove the private fast path in Layer B; keep reusable Layer A behavior generic |

## Release truth

Passing this runbook proves one configured deployment is operational. It does
not by itself prove every open roadmap horizon is complete. In particular, the
Compound Harness attention kernel, durable attention inbox, proactive producers,
and staged self-improvement remain separate capabilities and must not be called
shipped until their branches are tested, integrated, and merged.
