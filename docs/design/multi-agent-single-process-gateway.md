# Design: hosting N agents + N channels from one OS process

Status: **not implemented — design notes only.** The shipped slice for
"run the whole family from one config" is
`scripts/familyclaw-family-launcher.ps1` (a fleet launcher: one manifest +
one command starts/stops/status-checks N per-agent watcher processes). That
solves the *operator ergonomics* half of the gap. This document describes
the deeper, not-yet-built half: making the `familyclaw-gateway` binary
itself host multiple agents across multiple channel kinds inside a single
OS process, instead of one process per channel.

## Where things stand today

`crates/familyclaw-gateway/src/config.rs` already supports **multiple
agents sharing one process**: `FamilyConfig.agents: Vec<AgentCfg>` /
`all_agents()`, wired up in `main.rs` via `build_extra_agent_specs` +
`AgentBuildSpec`. A single gateway process can run several named agents
(each with a stable id, its own soul, its own `reply_target`) that all
publish onto the same Resonance Bus.

What is **not** supported: those agents all share **one `[channel]`
block** (`ChannelCfg { kind, discord, telegram }` — a single `kind` string
and a single `DiscordCfg`/`TelegramCfg`). `start_runtime` in `main.rs`
branches once on `channel_kind` (`"none" | "discord" | "telegram"`) and
builds exactly one `Box<dyn Channel>` for the whole process. So today:

- Multiple agents, **one** channel kind, one process: works.
- Multiple **channel kinds** (e.g. one agent on Discord + the same agent on Telegram) in
  one process: not supported — the gateway binds one channel per process.
- Different agents each wanting a *different* channel (agent_beta on
  Telegram, agent_gamma on Discord) from **one** process: not supported for the
  same reason — `ChannelCfg` is a singleton, not a list.

## The gap this document is for

> "5 members != 5 hand-managed processes."

Two different things can satisfy that sentence, and they are not the same
engineering effort:

1. **Fleet supervision** (shipped): one config + one command manages 5
   *processes*. Each process is still one gateway instance, but a human
   no longer hand-registers 5 Scheduled Tasks or babysits 5 terminals.
   → `scripts/familyclaw-family-launcher.ps1` + `family.manifest.example.json`.
2. **Process unification**: 5 members served by **one OS process**, one
   Tokio runtime, one `/healthz`, one `/metrics` — actual multi-tenancy
   inside the gateway binary. Not built. This document is the plan for it.

(1) is a multi-hour slice, safe to ship today. (2) is multi-week: it
touches config schema, channel lifecycle, routing, auth/ACL boundaries
between agents, and observability — see below.

## Why (2) is multi-week, not a quick patch

- **`ChannelCfg` → `Vec<ChannelInstanceCfg>`.** Each entry needs its own
  `kind`, its own secrets (bot token / webhook / public key), and its own
  `reply_target`-to-`agent` binding. This is a breaking config schema
  change (`familyclaw.toml` gets a `[[channels]]` array, mirroring
  `[[agents]]`), and every existing single-channel deployment (`.env`,
  `familyclaw.toml.example`, `docs/RUNBOOK_WINDOWS.md`, Docker Compose env)
  needs a compatible migration path (`[channel]` singular stays as the
  backward-compatible 1-entry case, same pattern as `agent` vs `agents`).
- **Channel↔agent routing.** Right now one process = one channel = every
  agent in `all_agents()` can be addressed through it via `reply_target`.
  With N channels in one process, an inbound message needs to resolve
  *which* channel instance it arrived on AND *which* agent it's for
  (today `reply_target` is agent→channel outbound only; there is no
  inbound multiplexing table). This is new routing logic, not a config
  tweak — `start_runtime`'s "three channel branches" become an N-way
  dispatch table keyed by channel instance id.
- **Two-way channels are stateful per instance.** `DiscordChannel` (see
  `main.rs`, `bot_token` two-way mode) owns a serenity client connection
  and its own event loop task. N Discord instances (e.g. two different
  bot tokens for two different family members) means N independent
  serenity clients inside one process — needs its own supervised-task
  lifecycle (start/stop/restart per instance) so one channel's crash
  doesn't take the others down. `familyclaw-channels` currently assumes
  one channel owns the process's channel-shaped resources (webhook client,
  interactions verifier, etc.) — auditing for hidden singletons
  (global state, single static client) is required before this is safe.
- **Auth / ACL boundary between agents in one process.** `SecurityCfg`
  (`gateway_token`, `profile`) and the Discord `owner_id` DM gate are
  currently process-wide. With N agents behind one process, per-agent
  operator ACLs need their own scoping (today's fail-closed DM gate
  logic — `owner_id = 0` means "off" — must be re-verified per agent, not
  globally, or one misconfigured agent silently opens another's DMs).
- **Observability.** `/healthz`, `/readyz`, `/metrics` are process-level
  today. Multi-tenant hosting needs either per-agent sub-paths
  (`/agents/{name}/healthz`) or labeled Prometheus series
  (`familyclaw_agent="agent_beta"`) so "the whole family is healthy" doesn't
  collapse into one boolean that hides a single dead member.
- **Blast radius.** One process crash currently takes down one
  channel/agent pair. Unify several agents into one process and a single
  panic (unwrap on a bad payload from any one channel) takes down the
  whole family at once — the exact "silent death" failure mode
  `ops/AUTOSTART.md` / the per-agent watcher script were built to catch,
  reintroduced at a bigger blast radius. This needs either
  `catch_unwind` isolation per channel task or an explicit acceptance
  that process unification trades isolation for operational simplicity —
  a decision for the operator, not an implementation detail.

## Recommended path (not started)

1. `[[channels]]` TOML array (additive, `[channel]` singular stays as the
   1-entry fallback — same pattern already proven for `[[agents]]` vs
   `[agent]` in `config.rs`).
2. `AgentBuildSpec` gains a `channel_ref: String` (which `[[channels]]`
   entry serves this agent); default = index 0 for backward compat.
3. `start_runtime` per-channel-kind branches become a loop building N
   `Box<dyn Channel>` + N background tasks (one Tokio task per two-way
   channel instance, supervised — restart-on-panic *inside* the process,
   not just outside it via the watcher script).
4. `/healthz` aggregates N channel-instance health checks;
   `familyclaw-observability` gets an `agent`/`channel_id` label
   dimension.
5. Ship behind a feature flag or a `--multi-tenant` opt-in until the ACL
   isolation story (item above) is verified with a test that proves agent
   A's `owner_id` gate cannot be satisfied by agent B's operator.

None of this is started. Treat `scripts/familyclaw-family-launcher.ps1`
as the correct answer for "run the whole family without hand-managing N
processes" until/unless the family decides the isolation trade-off in
step 5 above is worth taking.
