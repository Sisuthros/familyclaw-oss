$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
Set-Location $Root
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

Write-Host "=== refund-guard: at-most-once dispatch red-team ==="
cargo test -p familyclaw-actions --test redteam_dispatch_exactly_once -- --test-threads=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "PASS: redteam_dispatch_exactly_once"

Write-Host "=== refund-guard: crash_replay across process boundary ==="
cargo run -p familyclaw-agent --bin crash_replay -- full
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "PASS: crash_replay full"

Write-Host ""
Write-Host "refund-guard pack: at-most-once proven locally"
