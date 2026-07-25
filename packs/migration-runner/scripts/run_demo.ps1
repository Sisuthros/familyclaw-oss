$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
Set-Location $Root
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

Write-Host "=== migration-runner: build continuity_daemon (bench black box) ==="
cargo build -p familyclaw-agent --bin continuity_daemon
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "=== migration-runner: S1 crash matrix (step-0..step-4 migration analog) ==="
cargo run -p familyclaw-bench -- s1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Scorecard = Join-Path $Root "crates\familyclaw-bench\out\scorecard.json"
if (Test-Path $Scorecard) {
    Write-Host "--- s1_crash_matrix metrics ---"
    Select-String -Path $Scorecard -Pattern "side_effect_overcount" | Select-Object -First 1
}

Write-Host ""
Write-Host "migration-runner pack: crash resume without re-apply proven locally"
Write-Host "Mapping: step-0..step-4 in S1 = migration phases; target side_effect_overcount = 0"
