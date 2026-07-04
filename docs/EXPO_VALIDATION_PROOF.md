# Expo Validation Proof — v1.2.0

> Exact, reproducible results of the full local verification suite run for the
> `feat/expo-finish-pass` branch. This is the authoritative proof of what passes
> locally. Hosted CI status is stated separately at the bottom (it runs on a
> zero-spend GitHub account and is **not** claimed here as the source of truth).

## Environment

| Field | Value |
|---|---|
| Date | 2026-07-02 |
| Branch | `feat/expo-finish-pass` |
| Base commit | `a995bf3` (branch created from `main`) |
| OS | Windows 11 (win32), MINGW64 shell |
| Rust | `rustc 1.95.0 (59807616e 2026-04-14)` |
| Workspace | 23 crates + `examples/minimal-gateway` |
| Version | `v1.2.0` |

> The commit SHA of the validated tree is recorded in the PR; the results below
> were produced on the branch tip with all seven Expo deliverables applied.

## Results

Each command was run locally on the branch. Exit codes and counts are verbatim.

| # | Command | Exit | Result |
|---|---------|:----:|--------|
| V1 | `cargo fmt --all -- --check` | 0 | Clean (after `cargo fmt --all` applied formatting to the new example) |
| V2 | `bash scripts/audit-layer-b.sh` | 0 | LAYER B AUDIT PASSED — no private souls/keys/profiles in Layer A |
| V3 | `bash scripts/test-audit-layer-b.sh` | 0 | 14 passed, 0 failed |
| V4 | `cargo build --workspace` | 0 | Finished (default features) |
| V5 | `cargo build --workspace --features discord` | 0 | Finished |
| V6 | `cargo clippy --workspace --all-targets --features discord -- -D warnings` | 0 | Clean, no warnings |
| V7 | `cargo build -p familyclaw-agent --bin continuity_daemon` | 0 | Finished |
| V8 | `cargo clippy --workspace --all-features --all-targets -- -D warnings` | 0 | Clean, no warnings |
| V9 | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features` | 0 | Generated docs, no doc warnings |
| V10 | `cargo test --workspace --features discord` | 0 | **1708 passed, 0 failed, 3 ignored** |
| V11 | `cargo test --workspace --all-features` | 0 | **1736 passed, 0 failed, 3 ignored** |
| V12 | `cargo run -p familyclaw-bench --bin bench -- all` | 0 | **Scorecard 8/8 PASS — Overall: PASS** (s1..s8) |
| V13 | `cargo run -p familyclaw-agent --example two_agents_memory` | 0 | Flagship demo — all invariants asserted, exit 0 |
| V14 | `bash scripts/expo-demo.sh` | 0 | Expo demo completed |
| V15 | `bash scripts/pre-publish-scan.sh` | 1 | **Working tree clean; git history leaks names (expected — see below)** |
| V16 | `powershell -File scripts/expo-demo.ps1` | 0 | Expo demo completed (ASCII-only, PowerShell 5.1 safe) |
| V17 | `powershell -File scripts/public-demo.ps1` | — | Parses clean; fast mode = flagship demo + crash-replay + 8-scenario scorecard |

### Test counts

- **`--features discord`:** 1708 passed / 0 failed / 3 ignored.
- **`--all-features`:** 1736 passed / 0 failed / 3 ignored (the delta over `discord`
  is the additional feature-gated tests, incl. the repaired `surreal` backend).

### Continuity scorecard (V12)

`bench -- all` regenerates `crates/familyclaw-bench/out/SCORECARD.md` deterministically:

```
Overall: PASS
s1_crash_matrix       — PASS   (CorruptedJournal: loud refusal, correct)
s2_retention_curve    — PASS
s3_dream_quality      — PASS
s4_emotional_contagion— PASS
s5_semantic_retrieval — PASS
s6_eternal_thread     — PASS
s7_provenance_gate    — PASS
s8_weekly_review      — PASS
```

### Flagship demo (V13) — every printed claim is asserted

`cargo run -p familyclaw-agent --example two_agents_memory` proves, live and with
`assert!`s (process exits non-zero on any failed invariant):

1. Two named agents (`Alice`, `Bob`) are live on the resonance bus (2 beings registered).
2. Real message delivery: Alice publishes to the bus → Bob's actor receives and stores it → recalled from Bob's real store. The sender does not store her own broadcast.
3. Real emotion contagion over the bus: Bob's Joy `0 → 18.0` after Alice's pulse, read through the emotion probe; the pulse is not stored as a memory.
4. Dream consolidation on **Bob's** memory: active memories `4 → 3` (duplicates merged), the relative date `"yesterday"` grounded to `"yesterday (YYYY-MM-DD)"`.
5. Time and decay change retrieval (a separate, honestly-named section — not the dream): the same query returns a different top memory on day 1 vs day 8.
6. The `ProtectedCore` identity anchor survives Ebbinghaus decay (retention stays 1.00) while the `Fast`-decay trivia fades.

## Pre-publish history result (V15)

`scripts/pre-publish-scan.sh` reports:

- **Working tree: CLEAN** — the current tracked files contain no private Layer B
  names (`audit-layer-b.sh` passes; the family names appearing there are the
  audit's own denylist, which is intentional).
- **No secret patterns in history.**
- **Git history leaks private names** in commit messages and diff content
  (approx: agent_alpha 44, the operator 41, agent_gamma 38, agent_beta 31, agent_epsilon/agent_delta 26,
  assistant 7 commits of content).

**Conclusion: the repository *content* is publishable, but the git *history* is
not.** The correct, documented path is a clean-history **orphan repo**
(`docs/PUBLISH_ORPHAN_PLAN.md`): a single commit of the current tree into a fresh
public repository, after which this gate passes. This PR **does not rewrite git
history** (out of scope by instruction); the orphan publish is an operator step.

## Hosted CI status (stated separately)

The repository defines CI gates in `.github/workflows/ci.yml` (`layer-b-audit`,
`check-build-test` = fmt + build + clippy `-D warnings` + test + doc, `msrv` on
Rust 1.88, `all-features`, and a Windows build+test job). These run on a
**zero-spend GitHub account**; hosted CI is therefore **not** claimed here as
green. The authoritative proof for this branch is the local run recorded above.
