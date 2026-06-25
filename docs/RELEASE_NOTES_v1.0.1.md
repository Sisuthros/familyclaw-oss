# FamilyClaw v1.0.1

A patch release hardening v1.0.0 in response to an adversarial post-release
audit. No new features; no public API breaks. Every v1.0 gate stays green,
verified locally and reproducibly (`docs/CI_GREEN_PROOF.md`).

## Fixes

- **Security — Layer B audit blind spot (#34).** The Layer B leak audit scanned
  files by an extension allowlist (`*.md`, `*.rs`, …), silently skipping tracked
  text files in other formats (`.txt`, `.html`, `.csv`, `.xml`, `.sql`, `.ini`,
  `.cfg`, and extensionless text such as `LICENSE`). It now scans **every
  git-tracked text file**, classified by content (`grep -Iq .` — NUL bytes →
  binary → skipped), so a leaked private name in any text format fails the audit.
  Adds 14 sandboxed regression tests; the forbidden fixture name is derived at
  runtime from the audit's own list, so no real private name is committed.

- **Concurrency — scheduler held its control-plane lock across `await` (#35).**
  `SchedulerRunner::run_shared` held the shared `Mutex<Scheduler>` across the
  dispatch `await`, so operator-plane mutations (pause/resume/kill-switch via
  `set_task_enabled`) blocked behind slow dispatch I/O. The tick now runs in
  three phases — collect due dispatches under a brief lock (pure, no `await`) →
  dispatch **without** the lock → re-lock briefly to record each fired task —
  so control commands are no longer queued behind a long tick. At-most-once
  dispatch is preserved (stable firing key + outbox dedup). Adds 5 tests,
  including a barrier-skill proof that a mutation completes while a dispatch is
  in flight, and a 200-round concurrent no-deadlock check.

- **Docs — hosted CI status (#36).** Documents that hosted GitHub Actions is
  intentionally not relied upon for this private repo (to avoid spending paid
  Actions minutes); the local, reproducible proof in `docs/CI_GREEN_PROOF.md`
  is the authoritative gate.

## Verification

All gates green on `main` at this tag — Layer B audit + 14 regression tests,
`cargo fmt --check`, `cargo build/clippy -D warnings/test --workspace --features
discord`, `cargo doc -D warnings`, `cargo deny check`. Reproduce with the
commands in `docs/CI_GREEN_PROOF.md`.

## Integrity note

Honors the Layer A / Layer B wall (no private souls, keys, or paths in OSS
crates — enforced by the very audit hardened in #34). Each change shipped as a
small, independently gate-validated PR, verified on `main` after merge.
