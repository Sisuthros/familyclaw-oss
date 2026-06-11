# E2E: Telegram + agent_alpha (MVP demo)
# Requires E:\familyclaw-profiles\.env with TELEGRAM_BOT_TOKEN and related vars.
#
# Usage:
#   . .\scripts\load-env.ps1
#   .\scripts\e2e-telegram-agent_alpha.ps1
#   .\scripts\e2e-telegram-agent_alpha.ps1 -StartGateway

param(
    [switch]$StartGateway
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not $env:FAMILYCLAW_DATA_DIR) {
    Write-Host "FAMILYCLAW_DATA_DIR unset — initializing default JSON store..."
    & "$PSScriptRoot\init-familyclaw-data.ps1"
    $env:FAMILYCLAW_DATA_DIR = "E:\familyclaw-data"
}

Write-Host "=== Gateway doctor ==="
cargo run -p familyclaw-gateway -- doctor
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "Doctor failed. Copy and fill secrets:"
    Write-Host "  Copy-Item E:\familyclaw-profiles\.env.example E:\familyclaw-profiles\.env"
    Write-Host "  . .\scripts\load-env.ps1"
    exit 1
}

if ($StartGateway) {
    Write-Host ""
    Write-Host "=== Starting gateway (Ctrl+C to stop) ==="
    Write-Host "Send a Telegram message to your bot; agent_alpha profile: $env:FAMILYCLAW_AGENT_NAME"
    cargo run -p familyclaw-gateway
} else {
    Write-Host ""
    Write-Host "Doctor OK. Start gateway with:"
    Write-Host "  .\scripts\e2e-telegram-agent_alpha.ps1 -StartGateway"
}
