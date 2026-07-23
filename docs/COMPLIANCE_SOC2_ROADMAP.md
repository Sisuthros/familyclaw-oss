# SOC 2 Type I — control mapping roadmap

Honest status: **no SOC 2 certification yet**. This document maps FamilyClaw
controls to Trust Services Criteria so a buyer can start a Type I engagement
without inventing the control set from scratch.

| TSC theme | FamilyClaw control | Evidence today | Gap to Type I |
|---|---|---|---|
| Security — access | Gateway bearer required off-loopback; operator ACL | `familyclaw-gateway` tests; `docs/ENTERPRISE_AUTH.md` | IdP SSO evidence pack |
| Security — change | CI Layer B audit, fmt, clippy `-D warnings`, deny/audit | `.github/workflows/ci.yml` | Change-ticket linkage |
| Security — vuln | `cargo audit` / `cargo deny`; no `unsafe` | CI jobs | External pentest + bug bounty |
| Availability | Single-node journal + backup/restore runbook | `docs/BACKUP_RESTORE.md` | Documented RTO/RPO with pilot |
| Processing integrity | At-most-once dispatch; approval payload-hash; scorecard | `redteam_dispatch_exactly_once`; bench | Customer workflow attestation |
| Confidentiality | Layer A/B split; redaction in proofs | `scripts/audit-layer-b.sh` | Encryption-at-rest attestation for volumes |
| Privacy | Self-hosted; data residency = customer's region | Deployment docs | DPA template |

## Roadmap order

1. Founding pilot + backup/restore VERIFIED timestamp  
2. External pentest (redacted report public)  
3. Public bug bounty (`docs/BUG_BOUNTY.md`)  
4. Type I readiness review with auditor using this table  

Do not claim "SOC 2 certified" until the report is issued.
