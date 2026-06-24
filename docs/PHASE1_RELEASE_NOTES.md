# FamilyClaw Phase 1 Release Notes

## Status

Phase 1 is merged into `main`.

This release establishes FamilyClaw as a reliability-first Rust runtime for long-running AI agents. The goal is not to be another chat wrapper. The goal is to make agent work survive crashes, approvals, retries, and restarts without lying about what happened.

## What landed

### Durable replay and at-most-once external dispatch

FamilyClaw now has the core proof that in-flight work can survive crash/restart boundaries while preventing duplicate external side-effect dispatch.

The guarantee is deliberately scoped:

- external dispatch is **at most once** under crash
- committed dispatches replay as committed values
- an intent-only crash fails closed instead of blindly firing again
- this is not a universal exactly-once completion claim

### Human approval and resume

Risky actions can pause for explicit approval and resume after approval. The approval path is tested against double-submit behavior and replay/restart hazards.

### Discord / channel hardening

Discord/channel paths were hardened with:

- owner-only DM gating
- self-echo protection
- bot-to-bot mention gate
- token no-echo tests
- inbound-only webhook error clarity
- outbound reply routing based on the inbound message target so DM replies stay in the DM channel

### Layer A / Layer B boundary

The public repo remains Layer A only. Private profiles, identities, memories, keys, local paths and channel tokens stay outside the repository and are loaded at runtime.

Layer B leak audit is part of CI.

### CI release gate

The release gate is green on the merged Phase 1 head:

- Layer B leak audit
- cargo audit
- cargo deny
- Check, Build, and Test
- Build and Test on Windows
- Clippy + Doc on Windows / MSVC
- MSRV check
- living feature matrix

The advertised MSRV is now Rust 1.88.

## What this release does not claim

FamilyClaw Phase 1 does **not** claim:

- universal exactly-once completion
- production-ready Discord operations without live smoke testing
- fully shipped live multi-agent orchestration
- safe execution of arbitrary untrusted code without deeper sandbox e2e proofs
- any private Layer B runtime/profile is part of this public repo

## Why it matters

Checkpointing can remember state. That is useful, but it is not enough once an agent performs external work.

FamilyClaw’s Phase 1 reliability wedge is this:

> An agent must not accidentally pull the trigger twice after a crash.

That is the difference between a demo agent and infrastructure that can eventually do useful work.

## Next work

1. Post-merge docs and release polish.
2. Crash-safe dispatch case study.
3. Action / Skill Runtime.
4. Live multi-agent integration proof.
5. WASM sandbox end-to-end safety proofs.
6. Additional channel adapters after the safety gates remain green.

Separate private projects stay outside this repository and outside this release plan.