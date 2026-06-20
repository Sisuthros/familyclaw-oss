# Security Policy

## Supported Versions

FamilyClaw is currently **pre-1.0**. Security fixes apply to the `main` branch until the first tagged release.

| Version | Supported          |
| ------- | ------------------ |
| main    | ✅ Active development |
| < 1.0   | ❌ Not applicable   |

## Reporting a Vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately through:
- GitHub Security Advisories (preferred): https://github.com/Sisuthros/familyclaw/security/advisories/new
- Email: viltsu.operator@gmail.com

We aim to acknowledge valid reports promptly.

## Scope

Security-sensitive areas include:

- **Layer A / Layer B boundary leaks** — The core security property of FamilyClaw. No private identities, keys, souls, calibrations, or `.env` files must ever enter the public repository. CI enforces this via `layer-b-audit`.

- **Profile loading** — Runtime profiles (SOUL.md, calibrations) are loaded from private filesystem locations; ensure paths are never logged or serialized.

- **API keys and token handling** — LLM client (`familyclaw-agent/src/llm.rs`) accepts keys at runtime only; keys are never hardcoded or committed.

- **Sandbox execution** — WASM sandbox (`familyclaw-sandbox`) uses Wasmtime with fuel metering and deny-by-default capabilities. Verify sandbox isolation before deploying untrusted code.

- **Durable journal integrity** — `FileJournal` uses append-only JSONL with fsync. Corruption detection is built into replay; verify journal integrity on startup.

- **External side-effect dispatch** — Outbound side effects (tool dispatch, post-approval continuations) are **dispatched at most once under a crash**: each dispatch is bound to a caller-derived idempotency key and journaled in two phases (intent before the side effect, committed after). A committed dispatch replays as a value without re-running the effect; a crash in the narrow intent-only window **fails closed** (zero or one execution, requires recovery) rather than blindly re-firing. This is crash-survival duplicate-prevention — **at-most-once dispatch, not a guarantee of universal exactly-once *completion*.**

- **Channel adapters** — Discord, Telegram, WhatsApp, Signal adapters handle external tokens. Ensure webhook URLs and bot tokens are loaded from environment, never committed.

## Responsible Disclosure

- We follow coordinated vulnerability disclosure.
- Fixes will be released on `main` branch with a security advisory.
- No CVE assignment planned until 1.0 release.

## Known Advisories (`cargo audit`)

We track `cargo audit` honestly rather than hiding open advisories. As of
the current `Cargo.lock`, the following are known and **transitive only**
(not in FamilyClaw's own code):

- **`rustls-webpki` 0.102.8** (RUSTSEC-2026-0049/0098/0099/0104) — pulled in
  **only under the `discord` feature** via `serenity 0.12.5 → tokio-tungstenite
  0.21 → rustls 0.22`. These advisories concern certificate-revocation-list
  and name-constraint handling in TLS server-certificate verification. They
  cannot be resolved without an upstream `serenity` release that bumps its
  `tungstenite`/`rustls` chain; we will upgrade as soon as one is available.
  Builds **without** the `discord` feature are unaffected.
- **`rsa` 0.9.x** (RUSTSEC-2023-0071, "Marvin Attack") — timing side-channel;
  no fixed upstream version exists yet. Transitive; not used for FamilyClaw's
  own key operations.
- **`atomic-polyfill`** (RUSTSEC-2023-0089) — *unmaintained* warning only, not
  a vulnerability.

Run `cargo audit` yourself to verify this list against the current lockfile.