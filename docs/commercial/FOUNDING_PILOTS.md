# Founding pilots

FamilyClaw is seeking **2–3 founding pilots** — not a broad marketplace launch.
Each pilot hardens **one money- or infra-touching workflow** under the
Reliability Sprint (`docs/commercial/ONE_PAGER.md`).

## Ideal workflows

1. **Fintech / refunds** — agent issues refunds or payouts; crash must not double-pay.
2. **Infra teardown / cost cleanup** — agent deletes idle cloud resources; crash must not double-destroy.
3. **Migration agent** — overnight schema/data steps; resume must not re-apply a committed step.

## What the pilot gets

- Working PoC on the customer's staging (or agreed sandbox)
- Crash-window proof for *their* side effect (overcount = 0)
- Backup/restore drill of `FAMILYCLAW_DATA_DIR`
- Pilot SLA (`docs/commercial/PILOT_SLA.md`)
- Option to be named in a public case study (opt-in only)

## What we need from the pilot

- One technical contact during the sprint week
- A single workflow with a clear external side effect
- Permission to publish anonymized metrics (overcount, resume time) if named case study is declined

## How to apply

See contact block in `docs/COMMERCIAL_OFFER.md`. Subject line: `Founding pilot — <workflow>`.

## Status

| Slot | Workflow class | Status |
|---|---|---|
| 1 | Fintech / refunds | **Open** |
| 2 | Infra teardown | **Open** |
| 3 | Migration agent | **Open** |

When a slot is filled, update this table and `docs/commercial/ONE_PAGER.md`
("production deployments: N") — never claim battle-tested before a live pilot.
