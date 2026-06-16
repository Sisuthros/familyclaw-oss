# Phase 0 — Embedder backend spike

> **Throwaway spike** (NOT a workspace member — `Cargo.toml` `members = ["crates/*",
> "examples/minimal-gateway"]` excludes `spikes/`). Its only product is the **decision note**
> in `DECISION.md`, which picks the **Phase 3** default embedding backend. No production
> dependency lands from this spike.

## Question

Phase 3 (`familyclaw-embeddings`) needs a LOCAL embedding provider (köyhyys: no API).
The v1.0 roadmap (D4) requires: the **default real provider must build on MSVC stable AND pass
`cargo deny`** with the repo's `deny.toml` license allowlist. Candidates:

- **`candle` (pure-Rust)** — `candle-core` "Minimalist ML framework", pure-Rust, no native C++ link.
- **`fastembed`** — ergonomic local embeddings, BUT wraps `ort`.
- **`ort` (onnxruntime)** — `2.0.0-rc.12` (release candidate), a safe wrapper around the **native
  ONNX Runtime C++** library. Native link → MSVC build risk + transitive license surface.

## Method (evidence, not opinion)

For each candidate, in an isolated throwaway crate, run:
1. `cargo generate-lockfile` (resolve the dependency tree — proves it RESOLVES).
2. `cargo deny check licenses` against the repo `deny.toml` (proves it passes the license gate).
3. (optional, expensive) `cargo build` on MSVC stable — only if 1+2 pass and time allows.

Record which candidate passes deny + resolves cleanly. The winner is the Phase 3 default; the
loser is relegated to an opt-in feature only if it can be made to pass deny (license added).

See `DECISION.md` for the recorded outcome.
