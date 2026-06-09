# Contributing to FamilyClaw

Thank you for considering contributing to FamilyClaw! 🤝

## Quick Start

```bash
# 1. Fork & clone
git clone https://github.com/YOUR_USERNAME/familyclaw
cd familyclaw

# 2. Verify build + tests + benchmarks
cargo test --workspace
cargo run -p familyclaw-bench --bin bench -- all  # must pass

# 3. Create a branch
git checkout -b feat/your-feature-name
```

## What We Accept

| Type | Guidelines |
|------|------------|
| **Bug fixes** | Always include a test that reproduces the issue |
| **Features** | Must pass `cargo check && cargo test && cargo bench` |
| **Documentation** | Keep `README.md`, `docs/`, and crate-level docs in sync |
| **Benchmarks** | New scenarios in `familyclaw-bench` must be deterministic (fixed clock) |
| **Refactors** | No behavior change — benchmark must still pass |

## What We Don't Accept

| ❌ | Reason |
|---|--------|
| Breaking changes without version bump | Semver is enforced |
| Features behind feature flags that don't build | All features must compile |
| "Magic numbers" without docs | Explain `const` values in comments |
| `unwrap()`/`expect()` on hot paths | Use `Result` + `FamilyClawError` |

## Code Style

- **Edition**: 2021 (workspace-level)
- **Lints**: `#[warn(clippy::all, clippy::pedantic)]` — fix all warnings before PR
- **Async**: `tokio` only, no `async-std` mixing
- **Errors**: `thiserror` + `FamilyClawError` variants — no `anyhow` in public APIs
- **Tests**: `#[tokio::test]` for async, unit tests in `#[cfg(test)]` modules

## Benchmark Requirements

All new benchmarks **must**:
1. Use `time::parse_rfc3339(FIXED_CLOCK_RFC3339)` — NOT `time::now()`
2. Be deterministic — same input = byte-for-byte identical `scorecard.json`
3. Extend `Scorecard` with new metrics (not ad-hoc prints)
4. Pass `bench all` on clean checkout

See `crates/familyclaw-bench/src/bin/bench.rs` for the pattern.

## Pull Request Checklist

Before opening a PR, verify:

```
☐ cargo check --workspace
☐ cargo test --workspace
☐ cargo run -p familyclaw-bench --bin bench -- all
☐ cargo clippy --workspace -- -D warnings
☐ Updated CHANGELOG.md (if user-facing change)
☐ Updated docs/ (if architecture change)
☐ No `unwrap()`/`expect()` in non-test code
☐ Commit messages follow conventional commits (feat:, fix:, docs:, bench:, refactor:)
```

## Commit Message Format

```
<type>(<scope>): <subject>

<body>

<footer>
```

Types: `feat`, `fix`, `docs`, `bench`, `refactor`, `test`, `chore`, `ci`

Example:
```
feat(hearth): add narrative thread cross-references

Adds NarrativeThread::link_to() for bidirectional thread linking.
Cross-reference recall now verified in s6_eternal_thread benchmark.

Closes #42
```

## Reporting Issues

Use the GitHub issue templates:
- **Bug report** — include `cargo --version`, OS, minimal reproduction
- **Feature request** — explain the use case, not just the solution
- **Benchmark regression** — attach `scorecard.json` before/after

## Security

Report security issues privately via GitHub Security Advisories — NOT public issues.

## Questions?

Open a **Discussion** on GitHub (not an issue) for design questions, architecture debates, or "how do I..." questions.

---

**FamilyClaw is KERROS A (OSS).** No hardcoded family members, keys, or paths. All runtime config via env / `familyclaw.toml`. Keep it that way.