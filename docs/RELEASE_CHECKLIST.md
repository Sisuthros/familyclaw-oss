# Release Checklist

This checklist protects the Layer A / Layer B boundary and keeps FamilyClaw release-ready. It separates private-alpha readiness from future public OSS readiness.

## Release types

| Release type | Meaning |
| --- | --- |
| Private alpha | Internal/private runtime use. Private profiles may exist outside the repo. Repository remains Layer A only. |
| Public OSS release | Publishable Layer A source. Requires stronger history and metadata hygiene. |

## Non-negotiable Layer B rules

Before any release:

- No real `SOUL.md` files are tracked.
- No private profile directories are tracked.
- No calibration files with real values are tracked.
- No private memory stores, journals, anchors, or conversation history are tracked.
- No API keys, tokens, webhook URLs, provider keys, or private credentials are tracked.
- No absolute private machine paths are tracked.
- No private agent names appear in examples, docs, test fixtures, branch names, release notes, or generated files.
- Examples use generic names only, such as `agent_a`, `agent_b`, `operator`, and `private-family`.

Run:

```bash
bash scripts/audit-layer-b.sh
```

Expected result: PASS.

## Documentation truth check

Docs must match implementation and CI.

Check:

- README verification commands match CI.
- `docs/DEMO.md` verification commands match CI.
- No docs recommend `cargo test --workspace --all-features` while `familyclaw-hearth/surreal` remains excluded.
- Dead or quarantined features are clearly labeled.
- Provider examples are truthful: current HTTP LLM client expects OpenAI-compatible chat-completions endpoints unless a native adapter exists.
- Discord mode is documented exactly as implemented.
- Static reply target is documented as fallback when per-message origin is unavailable.
- `/healthz` and `/readyz` are documented.
- Deployment docs do not imply private data belongs in the repo or image.

## Build and test matrix

Run the default suite:

```bash
cargo test --workspace
```

Run the living-feature matrix that mirrors CI:

```bash
cargo test --workspace \
  --features familyclaw-channels/discord \
  --features familyclaw-channels/telegram \
  --features familyclaw-channels/whatsapp \
  --features familyclaw-channels/signal \
  --features familyclaw-sandbox/wasmtime
```

Run formatting:

```bash
cargo fmt --all -- --check
```

Run clippy:

```bash
cargo clippy --workspace --all-targets --features discord -- -D warnings
```

Run docs:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features discord
```

## Audit and dependency checks

Run cargo-audit using the same ignore policy documented in CI:

```bash
cargo audit \
  --ignore RUSTSEC-2023-0071 \
  --ignore RUSTSEC-2026-0049 \
  --ignore RUSTSEC-2026-0098 \
  --ignore RUSTSEC-2026-0099 \
  --ignore RUSTSEC-2026-0104 \
  --ignore RUSTSEC-2023-0089 \
  --ignore RUSTSEC-2025-0119
```

Run cargo-deny:

```bash
cargo deny check
```

Each ignore must have an explicit reason in repository policy. Do not silently add ignores.

## Continuity benchmark

Run:

```bash
cargo run -p familyclaw-bench --bin bench -- all
```

Expected:

- All published scenarios pass.
- Scorecard output is regenerated.
- Any failed scenario blocks release.

## Gateway smoke test

Start the gateway with private runtime config outside the repo:

```bash
FAMILYCLAW_GATEWAY_ADDR=127.0.0.1:8787 \
FAMILYCLAW_CONFIG=/absolute/private/familyclaw.toml \
FAMILYCLAW_PROFILE_DIR=/absolute/private/profiles \
FAMILYCLAW_DATA_DIR=/absolute/private/data \
cargo run -p familyclaw-gateway -- serve
```

In another shell:

```bash
curl -fsS http://127.0.0.1:8787/healthz
curl -fsS http://127.0.0.1:8787/readyz
```

If `/inject` is enabled, production-like tests must use a bearer token.

## Deployment check

Before private alpha deployment:

- `docs/DEPLOYMENT.md` is current.
- Runtime data directory is persistent.
- Secrets are runtime-injected.
- Private profiles are mounted or referenced outside the repo.
- Container image does not contain Layer B files.
- Health and readiness probes are configured.
- Logs do not print secrets.
- `/inject` is protected by `FAMILYCLAW_GATEWAY_TOKEN` outside local loopback development.

## Feature status check

Before release, every feature must be one of:

- supported and tested
- experimental and labeled
- quarantined and clearly documented
- removed

`familyclaw-hearth/surreal` must not be presented as supported while it remains API-stale or excluded from the living-feature matrix.

## Public OSS hygiene

For a future public OSS release, file-level audit is not enough.

Check:

- Git history does not expose private names, prompts, profiles, memories, or machine paths.
- Commit author and committer metadata are neutral and publishable.
- Branch names are neutral and publishable.
- PR titles and comments are neutral and publishable.
- Release notes contain no private family names or private context.
- Generated artifacts contain no private data.

Recommended public-release strategy:

1. Create a clean release branch from current private main.
2. Squash or rewrite history into neutral commits.
3. Re-run Layer B audit.
4. Re-run test matrix.
5. Re-run docs truth check.
6. Review commit metadata.
7. Tag only after all checks pass.

## Private-alpha definition of done

Private alpha is releasable when:

- Layer B audit passes.
- Test matrix passes.
- Bench scorecard passes.
- Docs and CI agree.
- Deployment guide exists and is current.
- Release checklist exists and is current.
- Gateway smoke test passes.
- Dead features are not presented as supported.
- Private runtime data stays outside the repo.

## Public OSS definition of done

Public OSS is releasable when private-alpha checks pass plus:

- Git history is neutralized or reviewed.
- Commit metadata is publishable.
- No private names appear in public artifacts.
- Examples are generic.
- The release branch contains Layer A only.
- A new external developer can run the documented demo without private data.
