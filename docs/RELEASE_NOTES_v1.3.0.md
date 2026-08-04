# FamilyClaw v1.3.0 — Release Notes (Public Preview)

A Rust agent runtime where in-flight work survives a crash — at-most-once
external side effects, durable memory, contract-checked coordination.

This is the first version published from a rewritten, Layer-B-clean git
history. See [`docs/HISTORY_REWRITE.md`](HISTORY_REWRITE.md) for exactly what
that rewrite removed and how it was verified.

## Highlights

- **Public history rewrite.** Commit author/committer identity, private names
  and a personal email address in historical file contents and commit
  messages, and a throwaway OIDC RSA test key were removed from every one of
  the 366 commits present at rewrite time, via `git-filter-repo` against a
  fresh single-branch clone. Three further commits were added afterwards and
  carry the same public identity, so the leak gate covers all 369 commits on
  the release branch. The published tree is byte-identical to the tree that
  was audited before the rewrite — the rewrite changed history only, not
  content. Old pre-rewrite tags were deliberately not carried over.

- **Two release-blocking leak-gate bugs fixed.** `scripts/audit-layer-b.sh`
  and `scripts/pre-publish-scan.sh` both read their forbidden-name list from a
  gitignored, operator-local file and silently fell back to placeholder names
  when it was absent — meaning a "PASS" could have been reported after
  searching for strings like `PlaceholderAgentOne` instead of any real name.
  The audit script now requires the real list (or an explicit, loudly
  announced opt-out for public CI); the publish gate has no placeholder mode
  at all and fails closed if the list is missing. Neither gate previously
  inspected commit author/committer metadata (`%an/%ae/%cn/%ce`) — that blind
  spot is closed by new checks in both scripts.

- **`scripts/crash-proof.sh`.** One command, no API keys, no network: runs the
  existing crash-safety proof across two crash windows over a real process
  boundary and prints `side_effect_overcount`, `approval_payload_match`, and a
  commit-SHA-bound receipt. A negative control on the pre-fix code path
  double-fires, so the proof measures actual behavior rather than a constant.

- **Model-agnostic dependability harness** (`familyclaw-harness`,
  `familyclaw-actions::dependability`) with receipts and a gate test. See
  [`docs/DEPENDABILITY_HARNESS.md`](DEPENDABILITY_HARNESS.md).

- **Production agent runbook + one-command truth gate.** See
  [`docs/PRODUCTION_AGENT_RUNBOOK.md`](PRODUCTION_AGENT_RUNBOOK.md) and
  `scripts/production-agent-doctor.ps1` (offline and live modes, secret-safe
  output). The doctor prints everything `/readyz` reports as `degraded`, so a
  knowingly reduced-capability deployment cannot pass unnoticed.

- **`.env.example` documents the full capability boundary** — workspace
  scopes, shell mode, sandbox, MCP servers, embeddings, fallback chain and
  request timeout, every variable verified against the code before being
  written down.

- **`/readyz` checks workspace tool scopes on every request, with no opt-in
  flag.** A configured scope whose root does not exist is now a hard `503`.
  An empty scope is reported under `degraded` instead — a locked-down
  deployment is legitimate, but it must stay visible. Paths are never echoed
  back, only counts.

- **Copyright attributed to "The FamilyClaw Authors"** across LICENSE,
  README, and GOVERNANCE — the standard form.

### Also landed since v1.2.0 (2026-07-02)

- **Reliability Console** (`GET /console`) — operator surface with a live
  status strip, an SSE audit feed, and one-click approvals.
- **Time Machine** — inspect/fork/diff over durable history, fail-closed by
  construction, with no apply path.
- **`familyclaw import --from openclaw|hermes`** — tolerant import adapters;
  imported skills are quarantined (never registered, never executed) and
  imported memories are never auto-admitted.
- **Security bench suite** — fuel exhaustion, capability denial, SSRF/prompt
  injection, and unapproved-side-effect checks against the real sandbox and
  actions APIs, with a deterministic committed artifact.
- **Three real executor skills** — `web_search` (keyless, read-only,
  SSRF-guarded), `file_write` (real disk write behind a canonicalized
  allowlist and an approval gate; proof records a path hash and byte count,
  never content), and `research` (multi-source fetch with host dedup and
  injection-escaped output).
- **`familyclaw-mcp` crate** — MCP client (stdio + HTTP transport) plus new
  skills (`shell_exec`, `schedule_task`, `spawn_subagent`, `github_issue`,
  `file_patch_apply`) and multi-agent subagent support.
- **Soft/hard turn watchdog** and **auto-continuation for token-limited
  replies**, both configurable via environment variables.
- **Deep `/readyz` + `POST /canary`** — provider ping, channel state, and
  journal writability, plus a 5-minute canary script.
- **Content-hash-bound growth approvals** — an approval binds to a SHA-256 of
  the proposal content, so a record→approve content swap fails instead of
  silently approving. There is still no `apply()` path.
- **Native OIDC/JWT operator auth** for protected gateway routes, fail-closed
  on partial configuration.
- **Slack channel adapter** (outbound only in this release).
- **`PostgresJournal`** (library-level only — `serve` still always opens a
  `FileJournal`, even with `DATABASE_URL` set).

## Fixed

- **`/readyz` failover stall** — a missing overall deadline around the
  provider failover walk could let the probe run for tens of seconds; a 20 s
  total deadline now bounds it.
- **Retired/missing models are treated as provider-dead** and rotate to the
  next provider with a long cooldown.
- **Discord message chunking** now breaks on line count as well as character
  count, and the bot correctly advertises online presence.
- **`clippy::manual_assert_eq`** that had been blocking CI on `main`.

## Security

- **`web_fetch` SSRF fix** — the host is now resolved before the fetch and
  internal/link-local addresses are rejected, closing a bypass where a domain
  resolving to a cloud metadata address or loopback slipped past the
  literal-IP check.
- **`shell_exec` smart mode** confines file arguments to a cwd allowlist.
- **Catastrophic `rm` target detection** now hard-blocks a named user home
  (`/home/<user>`, `/Users/<user>`) and its top-level sweep, not only the
  literal `/home`.
- **Dependency advisories addressed** — `crossbeam-epoch` (RUSTSEC-2026-0204),
  `surrealdb`, and `openssl` updated; 8 advisories cleared.
- **Layer B leak scrubbing** — private agent/operator names and paths removed
  from publishable code, comments, test fixtures, and deploy scripts; local
  debug/probe scripts quarantined.

## Known limitations

Honest list of what is **not** a shipped capability in v1.3.0:

- **PostgresJournal / multi-node HA** — implemented at the library level only;
  no runtime path selects it yet.
- **Growth-loop apply path** — deferred for safety; the proposal core cannot
  mutate any skill, policy, or permission.
- **Slack channel** — outbound only; no Socket Mode/Events API yet, and
  `POST /inject` is still wired to Discord only.
- **OTLP span-envelope scaffolding** — not an OpenTelemetry SDK; no network
  exporter, off by default.
- **Semantic retrieval weight** — engaged via `FAMILYCLAW_SEMANTIC_WEIGHT`,
  not proven against a labeled fixture in this release.

## Verification

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
--features discord -- -D warnings`, and `cargo test --workspace
--all-features` were run against this exact branch before release; see
`STATUS.md` and the release evidence log for the full local, reproducible gate
results this version was cut from. Hosted GitHub Actions is not relied upon as
the authoritative gate.
