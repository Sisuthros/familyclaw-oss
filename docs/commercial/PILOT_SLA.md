# Pilot SLA (founding engagements)

Applies to **Reliability Review** and **Reliability Sprint** customers during
the engagement window. This is a pilot SLA, not a 24/7 enterprise on-call
contract.

| Severity | Definition | First response | Workaround / update |
|---|---|---|---|
| Sev-1 | Gateway down in staging **or** duplicate external side effect observed | 4 business hours | Same day |
| Sev-2 | Approval path broken; auth fail-closed blocking operators | 1 business day | 2 business days |
| Sev-3 | Docs / non-blocking defects | 3 business days | Next sprint |

**Business hours:** Mon–Fri 09:00–17:00 Europe/Helsinki (excl. Finnish public holidays).

**Escalation:** Sev-1 → a GitHub issue labelled `SEV1` with subject `SEV1` + GitHub
Security Advisory if the issue is a vulnerability.

**Out of scope:** LLM provider outages, customer Layer B misconfiguration,
multi-region HA (single-tenant appliance model — see `docs/BACKUP_RESTORE.md`).
