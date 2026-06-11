# Homepage Factory — live LLM smoke (run after LiveTurnExecutor merge)
#
# Prerequisites:
#   - agent_gamma PR merged: LiveTurnExecutor in familyclaw-agent
#   - FAMILYCLAW_PROVIDERS + API key in env
#   - Optional: cargo test -p familyclaw-bridge --test homepage_factory --features live-llm

param(
    [switch]$CompareBench
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

Write-Host "=== FamilyClaw validation ==="
cargo test --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo run -p familyclaw-bench --bin bench -- all
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

if ($CompareBench) {
    cargo run -p familyclaw-bench --bin bench -- compare
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

Write-Host ""
Write-Host "Live Homepage Factory: run after LiveTurnExecutor is merged:"
Write-Host "  cargo test -p familyclaw-bridge --test homepage_factory -- --nocapture"
Write-Host "  (with live executor wired per docs/handoff/agent_gamma_LIVE_TURN_EXECUTOR.md)"
Write-Host ""
Write-Host "Smoke complete (bench all OK)."
