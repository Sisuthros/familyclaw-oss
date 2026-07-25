$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
Set-Location $Root
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

Write-Host "=== infra-teardown: Time Machine dry-run (replay demo) ==="
cargo run -p familyclaw-agent -- replay demo
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "PASS: replay demo (dry-run capture)"

Write-Host "=== infra-teardown: approval gate spot-check (shell_exec) ==="
cargo test -p familyclaw-actions shell_exec:: -- --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "PASS: write-external skills require approval"

Write-Host ""
Write-Host "infra-teardown pack: dry-run + approval gates demonstrated locally"
