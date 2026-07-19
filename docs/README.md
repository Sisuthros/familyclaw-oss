# FamilyClaw Documentation Index

This page groups the documents in `docs/` so you can find what you need
without guessing at filenames. If a file exists in `docs/` but isn't listed
here, it's most likely a dated planning/expo artifact — check
[`docs/archive/`](archive/) first.

## Getting started

| Document | What it covers |
|---|---|
| [QUICKSTART.md](QUICKSTART.md) | Get FamilyClaw running in 5 minutes — build, run the demos, connect Discord. |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Full technical overview: Resonance Bus, durable substrate, memory, dreaming, latent telepathy. |
| [architecture-channels.md](architecture-channels.md) | Channel adapter architecture (Discord/Telegram/WhatsApp/Signal). |
| [MIGRATION.md](MIGRATION.md) | Migrating between FamilyClaw versions. |
| [discord-setup.md](discord-setup.md) | Step-by-step Discord bot creation and configuration (the only channel setup guide, linked from the README Quick Start). |
| [RUNBOOK_WINDOWS.md](RUNBOOK_WINDOWS.md) | Windows-specific setup, including optional Telegram/Discord wiring. |
| [DEPLOYMENT.md](DEPLOYMENT.md) | Deploying the gateway. |
| [USERS.md](USERS.md) | Adoption profiles and the "should you use this" gate. |
| [CRASH_REPLAY.md](CRASH_REPLAY.md) | How durable replay and crash-safe dispatch work, and how to reproduce the proof. |
| [SKILLS.md](SKILLS.md) | Skill/WASM authoring guide: manifests, risk classes, capability model, fuel limits. |

## Security

| Document | What it covers |
|---|---|
| [SECURITY_MODEL.md](SECURITY_MODEL.md) | The eight defense layers (allowlists, approvals, taint tracing, redaction, identity anchors, sandbox, at-most-once dispatch, Layer A/B isolation). |
| [LAYER_BOUNDARY.md](LAYER_BOUNDARY.md) | The Layer A / Layer B split in detail — what may never enter the public repo. |
| [SECURITY_SCORECARD.md](SECURITY_SCORECARD.md) | Security scorecard results. |
| [SECURITY_BENCH.md](SECURITY_BENCH.md) | Security benchmark methodology and results. |
| [SECURITY_COMPARISON.md](SECURITY_COMPARISON.md) | Security posture compared against other frameworks. |
| [CRASH_SAFE_DISPATCH_CASE_STUDY.md](CRASH_SAFE_DISPATCH_CASE_STUDY.md) | Case study on at-most-once external side-effect dispatch. |

## Reference

| Document | What it covers |
|---|---|
| [COMPARISON.md](COMPARISON.md) | Feature comparison against other agent frameworks. |
| [NEMOCLAW_COMPARISON.md](NEMOCLAW_COMPARISON.md) | Comparison against NemoClaw. |
| [RUFLO_MAPPING.md](RUFLO_MAPPING.md) | Mapping reference (Ruflo). |
| [SCORECARD.md](SCORECARD.md) | The 8-scenario continuity benchmark scorecard. |
| [CI_GREEN_PROOF.md](CI_GREEN_PROOF.md) | Evidence that CI is green across the required feature matrix. |
| [RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md) | Steps to cut a release. |
| [RELEASE_NOTES_v1.0.0-rc.1.md](RELEASE_NOTES_v1.0.0-rc.1.md) | Release notes — v1.0.0-rc.1. |
| [RELEASE_NOTES_v1.0.0.md](RELEASE_NOTES_v1.0.0.md) | Release notes — v1.0.0. |
| [RELEASE_NOTES_v1.0.1.md](RELEASE_NOTES_v1.0.1.md) | Release notes — v1.0.1. |
| [RELEASE_NOTES_v1.2.0.md](RELEASE_NOTES_v1.2.0.md) | Release notes — v1.2.0. |
| [PHASE1_RELEASE_NOTES.md](PHASE1_RELEASE_NOTES.md) | Phase 1 release notes. |
| [LAUNCH.md](LAUNCH.md) | Launch playbook. |
| [DEMO.md](DEMO.md) | Demo script and talking points. |
| [BLOG_CASE_STUDY.md](BLOG_CASE_STUDY.md) | Blog-form case study. |

## Process / planning

| Document | What it covers |
|---|---|
| [PHASE1_PR_BODY.md](PHASE1_PR_BODY.md) | PR description used for the Phase 1 merge. |
| [PHASE3_PARALLEL_PLAN.md](PHASE3_PARALLEL_PLAN.md) | Phase 3 parallel workstream plan. |
| [PUBLISH_ORPHAN_PLAN.md](PUBLISH_ORPHAN_PLAN.md) | Plan for publishing an orphaned branch/history. |
| [GIT_CONSOLIDATION.md](GIT_CONSOLIDATION.md) | Git history consolidation notes. |
| [CODE_REVIEW_2026-06-04.md](CODE_REVIEW_2026-06-04.md) | Dated code review notes. |
| [plans/](plans/) | Dated implementation plans (parity roadmap, vision/image spec, close-out plans). |
| [source-blueprints/](source-blueprints/) | Source blueprint drafts referenced during early design. |

## Archive

[`docs/archive/`](archive/) holds superseded planning documents. They are kept
for evidentiary/historical value only — the active strategy document is
[`MASTERPLAN.md`](../MASTERPLAN.md) at the repo root. Do not update files in
`archive/`; update `MASTERPLAN.md` instead.

---

Some dated expo/demo-event and commercial-offer documents exist in `docs/` but
are intentionally not indexed here.
