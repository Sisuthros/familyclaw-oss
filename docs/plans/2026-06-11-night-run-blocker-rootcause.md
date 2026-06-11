# Night run 2026-06-11 — root cause of the 22-run stall (and the one-line fix)

> Written by the autonomous night developer (Claude Opus 4.8). **Source-only,
> verifiable, no secrets.** This is a diagnosis document — it does not change any
> runtime behaviour.

## Symptom

Runs #1–#22 of `night-nudger.py` (see `.claude/night-nudger-state.json`) could
**edit files** but could never run `cargo` or `git`. Every shell command came
back as `requires approval`, and in a headless `claude -p` run there is no human
to approve it, so the command was effectively denied. As a result:

- **P3.3** (`crates/familyclaw-bridge/tests/homepage_factory.rs`) was written and
  statically verified, but **never compiled, tested, committed, or pushed**.
- The runs correctly refused to fabricate a green `cargo test` result or commit
  unverified Rust (TURVA-PORTTI held), so they looped, each one re-explaining the
  same block.

## Root cause (the new finding)

`night-nudger.py:124` launches each run with:

```python
"--permission-mode", "acceptEdits",
```

`acceptEdits` auto-approves **file edits only** (Edit/Write/NotebookEdit). It does
**not** auto-approve `Bash` tool calls. So `cargo …`, `git add/commit/push`, and
every other shell command still raise a permission prompt — which, headless, is
denied. `acceptEdits` is not "allow everything".

This also explains why edits *outside* `.claude/` succeed while edits *inside*
`.claude/` (flagged sensitive) and all Bash calls fail.

## Fix (one line in the launcher)

`night-nudger.py` is orchestrator tooling, so the night developer does **not**
edit it under its mandate. The fix is one of:

**Option A — simplest, most permissive (best for unattended headless runs):**

```python
"--permission-mode", "bypassPermissions",
```

**Option B — keep `acceptEdits`, allowlist only cargo + git (respects the
TURVA-PORTTI spirit — no blanket shell access):**

```python
"--permission-mode", "acceptEdits",
"--allowedTools",
    "Bash(cargo:*)",
    "Bash(git add:*)", "Bash(git commit:*)", "Bash(git push:*)",
    "Bash(git diff:*)", "Bash(git status:*)", "Bash(git log:*)",
```

Option B is preferred: it unblocks exactly the build/test/commit/push seam the
mandate needs, and nothing else.

## What is ready the moment the launcher is fixed

P3.3 finishes in one cargo-enabled run — the gate and commit are already spelled
out in `docs/plans/2026-06-11-p3-workexecutor-seam.md`:

```
cargo +stable-x86_64-pc-windows-msvc test -p familyclaw-bridge --test homepage_factory
cargo +stable-x86_64-pc-windows-msvc test --workspace   # baseline ~760 green
git add crates/familyclaw-bridge/tests/homepage_factory.rs
git commit   # feat(bridge): homepage_factory-integraatiotesti WorkExecutor-saumalle (P3.3)
git push origin feat/night-2026-06-11
```

Then P4 (clippy, dependabot CVEs, `unimplemented!` cleanup) and P5 (docs) proceed
unblocked.

## Minor source improvement made this run (P4.3, unverified)

`crates/familyclaw-bench/src/scenarios/eternal_thread.rs`: the 5 bare
`unimplemented!()` calls in the test-only `StubSubject` were given descriptive
messages and a doc comment. These stubs are **intentionally unreachable** —
`EternalThread::run` does not use the `subject` parameter at all — so this is a
clarity-only change with zero behaviour impact. It is **not** committed (git
blocked); the next cargo-enabled run can compile-check and fold it into a P4
commit.

## Run #29 confirmation (2026-06-11, interactive ultracode session)

An interactive Opus 4.8 session re-verified the block: `cargo +stable-...
build/test`, `git add/commit/push`, **and** writing `.claude/settings.json` all
returned `requires approval` and the approval was not granted — identical posture
to the headless runs. Read-only git (`log/status/diff`) is open. Conclusion holds:
the only unblock is operator action (Option A or B in this doc, or approving the
cargo+git prompts in-session). No code was committed; the working tree remains
safe and untouched.
