# Phase 0 embedder-spike — DECISION

**Date:** 2026-06-16. **Decides:** Phase 3 (`familyclaw-embeddings`) default local embedding backend.
**Method:** isolated throwaway crates, real `cargo +stable-x86_64-pc-windows-msvc generate-lockfile`.

## Evidence (measured, not assumed)

| Candidate | Resolves on MSVC stable? | Dep count | Native C++ link? | Network fetch of binary? | Stable release? |
|---|---|---|---|---|---|
| **`candle-core` 0.10** (pure-Rust) | ✅ yes, clean | 134 | **0** (no onnx/native) | no | ✅ yes (0.10.2) |
| **`ort` "2"** (onnxruntime wrapper) | ❌ **does NOT resolve** | — | — | — | ❌ all 2.x are `-rc` |
| **`ort` 2.0.0-rc.12** (pinned) | resolves only when pinned | 103 | **yes** (`ort-sys` = native FFI) | **yes** (`ureq`/`ureq-proto` download onnxruntime) | ❌ release candidate |

Key facts:
- `ort = "2"` **fails to select a version** — every 2.x is a prerelease (`2.0.0-rc.12`…). Cargo
  refuses prereleases without an explicit pin. A release-candidate native-FFI crate is the wrong
  default for a production platform pinned to MSVC stable + MSRV 1.85.
- Pinned `ort` pulls `ort-sys` (native ONNX Runtime C++ link → MSVC build-toolchain risk) and
  `ureq`/`ureq-proto` (it **downloads the onnxruntime binary over the network** → violates the
  köyhyys "local / offline, vendored-once" constraint AND adds a runtime-download failure mode).
- `candle` is pure-Rust, resolves cleanly on MSVC stable, no native link, no network binary fetch.

## Decision

**Phase 3 default embedding backend = pure-Rust (`candle`-class), feature-gated.**
- `ort`/ONNX is **NOT** the default. It may be offered as an **opt-in feature later** ONLY if a
  future spike proves it (a) builds on MSVC stable from a **stable** (non-rc) release, and (b)
  passes `cargo deny check licenses` with any required license added to `deny.toml`.
- The Phase-3 `EmbeddingProvider` default stays the deterministic zero-dep provider; the real
  local provider (candle-backed) is feature-gated and refuses/warns when `semantic_weight > 0`
  without a real provider configured.

## NOT verified locally (honest gaps → CI / Phase 3 will close)

- `cargo deny check licenses` was **NOT run** here — `cargo-deny` is not installed on this machine
  (`no such command: deny`). The license gate is enforced by the CI `cargo-deny` job; Phase 3 must
  run it against the candle dependency tree before the feature is enabled by default.
- A full `cargo build` (compiling candle) was not run — resolution + dep-shape is sufficient to
  pick the default; Phase 3 does the real build behind the feature flag.
- This spike crate is throwaway and is **not** a workspace member; delete `spikes/embedder-spike/`
  once Phase 3 lands, or keep `DECISION.md` as the record and drop the `*-path/` crates.
