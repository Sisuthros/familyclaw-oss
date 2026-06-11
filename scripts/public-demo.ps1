# Public release demo (Layer A) — no API keys, no private profiles, no channels.
#
# Usage: powershell -File scripts/public-demo.ps1
#        powershell -File scripts/public-demo.ps1 -Full   # includes bench compare

param(
    [switch]$Full
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

Write-Host "=== FamilyClaw public demo (Layer A) ==="
Write-Host ""

Write-Host "1/4 minimal-gateway (10s, in-memory) ..."
cargo run -p minimal-gateway -- --duration 10
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host ""
Write-Host "2/4 workspace tests ..."
cargo test --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host ""
Write-Host "3/4 continuity bench (6 scenarios) ..."
cargo run -p familyclaw-bench --bin bench -- all
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

if ($Full) {
    Write-Host ""
    Write-Host "4/4 comparative bench ..."
    cargo run -p familyclaw-bench --bin bench -- compare
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

Write-Host ""
Write-Host "Public demo complete."
Write-Host "Next: copy .env.example to a private path for your own agents (Layer B)."
Write-Host "See docs/QUICKSTART.md and docs/LAYER_BOUNDARY.md"
