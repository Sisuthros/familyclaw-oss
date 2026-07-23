# MCP works-with catalog

FamilyClaw bridges external [Model Context Protocol](https://modelcontextprotocol.io/)
servers into the action runtime as dynamic skills (`crates/familyclaw-mcp`).
Configure servers with `FAMILYCLAW_MCP_SERVERS` (stdio `name=command args` or
HTTP `name=https://host[:port][/path]`, semicolon-separated). Each discovered
tool is registered with a manifest derived from the MCP descriptor; execution
calls `tools/call` and treats output as **untrusted (tainted)**.

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

1. Set `FAMILYCLAW_MCP_SERVERS` only for processes you trust to spawn or URLs you
   trust to POST to.
2. Expect every MCP tool result to remain tainted through the pipeline.
3. Do not assume write tools are approval-gated by the bridge alone — verify the
   server's behavior and FamilyClaw skill policy before production use.
4. Prefer hermetic stdio servers in CI; use HTTP transports when the peer already
   speaks MCP over `/mcp`.

See also: [SKILLS.md](SKILLS.md), [SECURITY_MODEL.md](SECURITY_MODEL.md).
