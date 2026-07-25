# Pack: refund-guard

**Story:** an agent issues a refund. A crash mid-dispatch must not pay twice.

## 30-minute path

```powershell
# from repo root
.\packs\refund-guard\scripts\run_demo.ps1
```

Expected: redteam dispatch tests PASS, crash_replay recalls memory across a
process boundary, summary line prints `overcount target = 0`.

## What this proves

FamilyClaw's idempotency-keyed outbox keeps `side_effect_overcount = 0` at
every crash point. Map that to your PSP: each refund call gets a stable
idempotency key bound to the approval payload hash.

## Next

- Wire a real PSP behind an allowlisted skill (Layer B credentials).
- Approve from `/console` or Slack once those channels are live.
- Read [WORKFLOW.md](WORKFLOW.md) for the production seam.
