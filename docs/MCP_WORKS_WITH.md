# MCP works-with catalog

FamilyClaw bridges external [Model Context Protocol](https://modelcontextprotocol.io/)
servers into the action runtime as dynamic skills (`crates/familyclaw-mcp`).
Two config-driven ways to attach a server, both funnel into the same bridge:

- `FAMILYCLAW_MCP_SERVERS` — env var, quick attach (stdio `name=command args`
  or HTTP `name=https://host[:port][/path]`, semicolon-separated). Always
  registers at the safe `ReadOnly` trust class.
- `FAMILYCLAW_MCP_CONFIG` — path to a **TOML config file**, the first-class
  attachment path (see [Config-driven attachment](#config-driven-attachment-familyclaw_mcp_config)
  below). Supports per-server trust elevation.

Both sources are read at boot (`familyclaw_mcp::register_from_env`, wired
into every runtime via `familyclaw-runtime::register_mcp_from_env`); a TOML
entry overrides an env entry with the same server name. Each discovered tool
is registered with a manifest derived from the MCP descriptor; execution
calls `tools/call` and treats output as **untrusted (tainted)**.

## Config-driven attachment (`FAMILYCLAW_MCP_CONFIG`)

This is the first-class, documented "attach my existing MCP servers as
trusted, runnable `ActionRuntime` skills" path, and it is **separate from**
the `familyclaw import` quarantine path (`docs/MIGRATION.md`,
`crates/familyclaw-agent/src/import_cli.rs`). The two exist for different
reasons — see the module doc comment in `crates/familyclaw-mcp/src/config.rs`
for the full rationale; short version:

| | `familyclaw import` | `FAMILYCLAW_MCP_CONFIG` (this path) |
|---|---|---|
| Input | A static *export* file from another agent runtime | A **live** MCP server the bridge can `tools/list`/`tools/call` against |
| Trust model | Cannot verify what the source skill's code does → **quarantine, never registered/executed** | The MCP protocol is the contract; no code to sandbox → **registered and runnable directly** |
| Risk class | Always `ExecuteCode` + `AlwaysRequireApproval` (frozen) | Operator-chosen per server: `read_only` (default) or `trusted` |
| Activation | Requires separate sandbox validation + manual approval (out of scope for the importer) | Registered at boot — no separate activation step |

Point `FAMILYCLAW_MCP_CONFIG` at a TOML file:

```toml
# familyclaw-mcp.toml — attach existing MCP servers as ActionRuntime skills.

[[servers]]
name = "docs_search"          # logical name; prefixes bridged skill ids
command = "npx"               # stdio transport: command + args
args = ["-y", "@my/mcp-docs-server"]
trust = "read_only"           # default; omit for the same effect

[[servers]]
name = "local_notes"
command = "my-notes-mcp-server"
trust = "trusted"             # operator has reviewed this server

[[servers]]
name = "remote_kb"
url = "https://kb.internal.example/mcp"   # HTTP transport
trust = "read_only"
```

Each entry needs exactly one of `command` (stdio) or `url` (HTTP); `trust`
defaults to `read_only`. Malformed entries (empty name, neither/both of
`command`/`url`, duplicate names, unknown `trust` value) fail closed with a
clear error at load time — nothing is silently skipped.

### Trust classes

| `trust` | Bridged risk class | Approval policy | Effect |
|---|---|---|---|
| `read_only` (default) | `ActionRisk::ReadOnly` | `AutoIfReadOnly` | Auto-runs; assumed no side effects. |
| `trusted` | `ActionRisk::WriteLocal` | `RequireApproval` | Still auto-runs (local-write class is auto-runnable under `RequireApproval`), but any tool call that the pipeline reclassifies as external/irreversible/money/message/code still hits the approval gate — see `familyclaw-actions::policy::required_approval`. |

`trust` is a **per-server operator declaration**, not something derived from
inspecting the tool — the bridge cannot know what a remote MCP tool actually
does. Only mark a server `trusted` after reviewing what it exposes.

## What the bridge actually supports

| Capability | Status |
|---|---|
| MCP `initialize` + `notifications/initialized` handshake | Supported |
| `tools/list` → skill registration | Supported |
| `tools/call` → skill execution | Supported |
| Stdio JSON-RPC transport | **Tested in-repo** (hermetic mock server) |
| HTTP POST JSON-RPC (`…/mcp`) | Implemented (contract-compatible; not hermetically exercised like stdio) |
| Resources / prompts / sampling | **Not** bridged — tools only |
| Trust elevation from tool output | **Never** — fail-closed taint |

Default risk class for bridged tools is `ReadOnly` + `AutoIfReadOnly`. That is a
**contract assumption**: operators must not point the bridge at MCP servers that
perform irreversible side effects unless they accept that classification (or
wrap those tools behind a separate, approval-gated skill).

## Catalog (categories)

Honest labels:

- **Tested in-repo** — exercised by hermetic tests under `crates/familyclaw-mcp`.
- **Contract-compatible** — any MCP server that speaks the supported tool
  handshake over stdio or HTTP; not specifically validated against that vendor
  in this repository.

| # | Category | Examples (illustrative) | Status | Notes |
|---|---|---|---|---|
| 1 | Echo / fixture tools | In-repo `mock-mcp-stdio-server` (`mcp_mock_echo`) | **Tested in-repo** | Stdio list + call + runtime registration. |
| 2 | Fetch / HTTP research | URL fetch, public page read helpers | Contract-compatible | Prefer FamilyClaw's built-in `web_fetch` / `research` when you need SSRF guards in-process. |
| 3 | Filesystem (sandboxed) | Allowlisted path read/list MCP servers | Contract-compatible | Bridge does not add a path allowlist; rely on the MCP server's own sandbox + FamilyClaw policy. |
| 4 | Git / source control | Repo status, diff, blame-style tools | Contract-compatible | Treat outputs as untrusted text; do not let them alter approval policy. |
| 5 | Tickets / issues | Issue trackers, project boards | Contract-compatible | Write/close actions need an approval-aware wrapper; default MCP bridge class is read-only. |
| 6 | Calendar / scheduling | Free/busy, event list tools | Contract-compatible | Same trust model: tainted results only. |
| 7 | Cloud object storage | Bucket list/get metadata | Contract-compatible | Credentials stay in the MCP server process env — not in FamilyClaw Layer A. |
| 8 | Search / knowledge bases | Doc search, wiki query | Contract-compatible | Complements built-in `web_search`. |
| 9 | Messaging / notifications | Chat post/read adapters exposed as MCP tools | Contract-compatible | Prefer FamilyClaw channel adapters for first-class Telegram/Discord paths. |
| 10 | Databases (read-oriented) | SQL/query MCP fronts | Contract-compatible | Fail closed on secrets in proofs; redaction applies on the FamilyClaw side. |

## Operator checklist

1. Set `FAMILYCLAW_MCP_SERVERS` (quick attach) or `FAMILYCLAW_MCP_CONFIG`
   (first-class config file) only for processes you trust to spawn or URLs
   you trust to POST to.
2. Expect every MCP tool result to remain tainted through the pipeline.
3. Do not assume write tools are approval-gated by the bridge alone — verify the
   server's behavior and FamilyClaw skill policy before production use.
4. Only set a server's `trust = "trusted"` in the TOML config after reviewing
   what it exposes — it is an operator declaration, never auto-derived.
5. Prefer hermetic stdio servers in CI; use HTTP transports when the peer already
   speaks MCP over `/mcp`.

See also: [SKILLS.md](SKILLS.md), [SECURITY_MODEL.md](SECURITY_MODEL.md), [MIGRATION.md](MIGRATION.md).
