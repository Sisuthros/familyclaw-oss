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

You will receive a response within 72 hours acknowledging receipt and providing an estimated timeline for a fix.

## Scope

Security-sensitive areas include:

- **Layer A / Layer B boundary leaks** — The core security property of FamilyClaw. No private identities, keys, souls, calibrations, or `.env` files must ever enter the public repository. CI enforces this via `layer-b-audit`.

- **Profile loading** — Runtime profiles (SOUL.md, calibrations) are loaded from private filesystem locations; ensure paths are never logged or serialized.

- **API keys and token handling** — LLM client (`familyclaw-agent/src/llm.rs`) accepts keys at runtime only; keys are never hardcoded or committed.

- **Sandbox execution** — WASM sandbox (`familyclaw-sandbox`) uses Wasmtime with fuel metering and deny-by-default capabilities. Verify sandbox isolation before deploying untrusted code.

- **Durable journal integrity** — `FileJournal` uses append-only JSONL with fsync. Corruption detection is built into replay; verify journal integrity on startup.

- **Channel adapters** — Discord, Telegram, WhatsApp, Signal adapters handle external tokens. Ensure webhook URLs and bot tokens are loaded from environment, never committed.

## Responsible Disclosure

- We follow coordinated vulnerability disclosure.
- Fixes will be released on `main` branch with a security advisory.
- No CVE assignment planned until 1.0 release.