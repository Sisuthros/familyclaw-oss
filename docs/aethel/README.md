# Aethel PoC files

Runnable proof-of-concept `.aet` specs that model FamilyClaw's action effect
boundary in Aethel's `Claim<T>` / `Verified<T, Policy>` type system. See
[`../aethel-integration.md`](../aethel-integration.md) for the full design.

Requires the Aethel CLI (built from the `aethel` repo:
`cargo build --release -p aethel-cli`).

```sh
AETHEL_CLI=/path/to/aethel/target/release/aethel-cli   # .exe on Windows

# PASS: agent Claim is verified (approval) before reaching the executor.
"$AETHEL_CLI" check docs/aethel/familyclaw_action.aet          # -> exit 0

# BREAK: agent Claim dispatched WITHOUT verify() -> AE-EPISTEMIC-001.
"$AETHEL_CLI" check docs/aethel/familyclaw_action_breaker.aet  # -> exit 1
```

The passing file is the CI-gate artifact ("high-risk effects only accept
verified inputs"); the breaker file is the regression proof that the gate
fires. Both run entirely on Aethel's mature front-end (lexer/parser/checker) —
no dependency on Aethel's stub runtime.
