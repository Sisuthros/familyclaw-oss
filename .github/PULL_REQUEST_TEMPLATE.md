## Summary

What does this PR change, and why?

## Related issue(s)

Closes #

## Checklist

Before requesting review, confirm all of the following:

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --all-targets --features discord -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Full feature-matrix tests pass where relevant (see `CLAUDE.md` / `CONTRIBUTING.md` for the exact command)
- [ ] `bash scripts/audit-layer-b.sh` passes (no Layer B leakage)
- [ ] No private persona/family names, secrets, real API keys, real private
      paths, or real deployment endpoints introduced anywhere in this diff
- [ ] Only generic terms used where applicable (`agent_a`, `agent_b`,
      `operator`, `mock_provider`, etc. — see `CLAUDE.md` for the allowed list)
- [ ] Documentation and code comments added/updated are in **English**
- [ ] `docs/` and `README.md` updated if this changes architecture or user-facing behavior
- [ ] `CHANGELOG.md` updated if this is a user-facing change
- [ ] New benchmark scenarios (if any) are deterministic and integrated into `Scorecard`

## Notes for reviewers

Anything reviewers should pay special attention to (risk areas, tradeoffs,
follow-up work intentionally deferred).
