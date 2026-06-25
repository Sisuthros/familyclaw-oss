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

## Gaps closed by follow-up verification (2026-06-25)

The two honest gaps below were left open on 2026-06-16 because `cargo-deny` wasn't
installed and the full build wasn't run. Both are now **verified** (cargo-deny 0.19.9
present; MSVC stable host `x86_64-pc-windows-msvc`):

- ✅ **`cargo build` on MSVC stable** — a throwaway `candle-core` probe **compiled cleanly**
  (`Finished dev` in ~36s, exit 0). No native link step, no `link.exe` onnx errors. candle is
  confirmed pure-Rust-buildable on this toolchain.
- ✅ **`cargo deny check licenses`** against the repo `deny.toml` — candle's **transitive license
  tree passes** (`licenses ok`; the only failure was the probe crate itself being unlicensed,
  which disappears once given a `license` field). No rejected/copyleft licenses in the tree.
- ⚠️ **`cargo deny check advisories`** surfaces **RUSTSEC-2024-0436** (`paste` is *unmaintained*,
  **not a vulnerability**), transitive via `gemm`. This is the same class already ignored in
  `deny.toml` (`atomic-polyfill`, `number_prefix`). **Action for Phase 3:** add
  `RUSTSEC-2024-0436` to the `deny.toml` `ignore` list when the candle feature lands.

**Conclusion stands and is now fully evidence-backed: candle (pure-Rust) is the Phase-3 backend;**
`ort`/onnx stays rejected (rc-only, native FFI, network binary download).

## Spike crate lifecycle

This spike crate is throwaway and is **not** a workspace member; keep `DECISION.md` as the record
and drop the `*-path/` crates once the Phase-3 candle feature lands.
