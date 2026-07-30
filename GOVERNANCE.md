# FamilyClaw Governance

> **Principle**: *No single point of failure — not in code, not in people.*

---

## Roles

| Role | Responsibility | Current |
|------|----------------|---------|
| **Maintainer** | Merge rights, release tagging, security triage | `@Sisuthros` (creator) |
| **Core Contributor** | Review + approve PRs, benchmark stewardship | — *(open)* |
| **Contributor** | PRs, issues, discussions, docs | Anyone |

**Bus Factor Target**: ≥ 2 Maintainers before v1.0.0

---

## Decision Making

### Routine Decisions (PR reviews, bug fixes, docs)
- **Any Maintainer** can merge after ≥ 1 approval
- **Core Contributor** approval counts as Maintainer if ≥ 2 Core Contributors agree

### Strategic Decisions (breaking changes, new crates, governance changes)
- **Consensus** of all Maintainers required
- **RFC process** for changes affecting public APIs or workspace structure:
  1. Open `RFC-XXX-title.md` in `docs/rfc/`
  2. 7-day discussion period
  3. Maintainer vote (simple majority, min 2 Maintainers)

### Emergency Decisions (security, CI broken, release blocking)
- Any Maintainer can act unilaterally
- Must be reported in next Maintainer sync
- Reversible within 24h if contested

---

## Release Process

| Version | When | Who |
|---------|------|-----|
| **Patch** (`0.x.y`) | Bug fixes, doc updates | Any Maintainer |
| **Minor** (`0.x.0`) | New features, backward-compatible | Maintainer consensus |
| **Major** (`1.0.0`) | Stable API, breaking changes | RFC + Maintainer vote + benchmarks PASS |

### Release Checklist (every release)
```
☐ All CI passes (check + test + bench)
☐ CHANGELOG.md updated
☐ Scorecard.md attached to release
☐ Semver tag pushed (e.g., v0.1.0-alpha)
☐ Draft Release notes published
☐ Binary artifacts uploaded (if applicable)
```

---

## Adding Maintainers

**Criteria** (all required):
1. ≥ 3 merged PRs (non-trivial: features, fixes, benchmarks)
2. ≥ 1 benchmark scenario authored/maintained
3. Demonstrated understanding of Layer A / Layer B boundary
4. Nominated by existing Maintainer, approved by consensus

**Process**:
1. Nominee creates `GOVERNANCE-nominee-<name>.md` with evidence
2. 7-day comment period
3. Maintainer vote (unanimous required for first additional Maintainer)
4. On approval: add to this file, grant GitHub `Maintain` permission

---

## Removing Maintainers

**Voluntary**: PR removing self from this file — merged immediately.

**Involuntary** (last resort):
- ≥ 6 months no activity (commits, reviews, discussions)
- Violation of Code of Conduct (see below)
- Process: Maintainer vote (⅔ majority), 14-day notice, appeal to GitHub Security if contested

---

## Code of Conduct

**FamilyClaw follows the [Contributor Covenant v2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/).**

### Enforcement
- **Maintainers** enforce.
- Reports to: any Maintainer (DM or email) OR GitHub "Report abuse"
- **Sanctions**: warning → temporary ban → permanent removal
- **Appeal**: GitHub Security team (outside project)

---

## Layer A / Layer B Boundary (Non-Negotiable)

| Layer A (OSS) | Layer B (Private) |
|----------------|-------------------|
| `crates/familyclaw-*` | Amplifier integration, family secrets |
| No hardcoded names/keys/paths | Family member identities, API keys |
| Config via env / `familyclaw.toml` | Injected at deploy time |
| **MIT License** | Proprietary / internal |

**Any PR that leaks Layer B into Layer A is blocked.** No exceptions.

---

## Benchmark Integrity

- `bench all` **must** pass on every merge to `main`
- Scorecard is **the** truth — no "it works on my machine"
- New scenarios require: deterministic clock, explicit metrics, `Scorecard` integration
- Regression = release blocker

---

## Security Policy

| Severity | Response Time | Disclosure |
|----------|---------------|------------|
| Critical (RCE, secret leak) | 24h | GitHub Security Advisory + release |
| High (auth bypass, DoS) | 72h | GitHub Security Advisory + release |
| Medium/low | Next minor release | CHANGELOG + advisory |

**Never** discuss unpatched vulnerabilities in public issues.

---

## Current Maintainers

| Name | GitHub | Since | Focus Areas |
|------|--------|-------|-------------|
| Maintainer of record | `@Sisuthros` | 2026-06 | Architecture, benchmarks, Layer boundary |

---

## Amendment

This document can be amended by **Maintainer consensus** (see Strategic Decisions). All amendments recorded in `CHANGELOG.md` under "Governance".

---

*Governance is code. If it's not written down, it doesn't exist.*