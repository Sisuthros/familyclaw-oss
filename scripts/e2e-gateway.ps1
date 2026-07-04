# E2E: gateway doctor + optional serve (Layer B -- private .env required).
#
# Prerequisites:
#   1. Copy .env.example to a private path and fill secrets
#   2. Create profile: $PROFILE_DIR\$FAMILYCLAW_AGENT_NAME\SOUL.md
#   3. . .\scripts\load-env.ps1 -Path <your-private-env>
#
# Usage:
#   .\scripts\e2e-gateway.ps1
#   .\scripts\e2e-gateway.ps1 -StartGateway

param(
    [switch]$StartGateway
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not $env:FAMILYCLAW_DATA_DIR) {
    Write-Host "FAMILYCLAW_DATA_DIR unset -- initializing .local/data ..."
    & "$PSScriptRoot\init-familyclaw-data.ps1"
    $repoRoot = Split-Path $PSScriptRoot -Parent
    $env:FAMILYCLAW_DATA_DIR = Join-Path $repoRoot ".local" "data"
}

Write-Host "=== Gateway doctor ==="
cargo run -p familyclaw-gateway -- doctor
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "Doctor failed. For a public in-repo demo (no keys), run:"
    Write-Host "  .\scripts\public-demo.ps1"
    Write-Host ""
    Write-Host "For gateway E2E, load a private env file first:"
    Write-Host "  . .\scripts\load-env.ps1 -Path `$env:USERPROFILE\.config\familyclaw\familyclaw.env"
    exit 1
}

if ($StartGateway) {
    Write-Host ""
    Write-Host "=== Starting gateway (Ctrl+C to stop) ==="
    Write-Host "Agent profile: $env:FAMILYCLAW_AGENT_NAME"
    cargo run -p familyclaw-gateway
} else {
    Write-Host ""
    Write-Host "Doctor OK. Start gateway with:"
    Write-Host "  .\scripts\e2e-gateway.ps1 -StartGateway"
}
