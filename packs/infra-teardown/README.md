# Pack: infra-teardown

**Story:** an agent tears down idle cloud resources. A double-fire is an outage.

## 30-minute path

```powershell
.\packs\infra-teardown\scripts\run_demo.ps1
```

## What this proves

High-risk writes require approval + payload-hash binding. Crash-safe dispatch
keeps teardown at-most-once. Time Machine dry-run has no dispatch path.

See [WORKFLOW.md](WORKFLOW.md).
