# Docker kill/restart verification

Script: [`scripts/docker-kill-restart-verify.ps1`](../scripts/docker-kill-restart-verify.ps1)
(or `.sh`). Requires Docker + `FAMILYCLAW_GATEWAY_TOKEN` (non-loopback fail-closed).

```powershell
$env:FAMILYCLAW_GATEWAY_TOKEN = -join ((1..32) | ForEach-Object { "{0:x}" -f (Get-Random -Max 16) })
.\scripts\docker-kill-restart-verify.ps1
# Expect: VERIFIED <UTC> docker kill/restart with volume + token
```

Paste the `VERIFIED …` line into the pilot evidence locker / commercial
quickstart when you run it on your host. This repo does not claim a hosted
CI Docker kill/restart pass until that stamp exists for a named environment.
