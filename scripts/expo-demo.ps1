# FamilyClaw - Expo demo (Windows). ~2-4 minutes on a warm build.
#
#   powershell -File scripts/expo-demo.ps1
#
# A short, live, self-contained showcase for a booth or a talk. No API keys, no
# network, no Python environment, no paid services. It runs the two proofs that
# execute in seconds, then summarizes the crash-safety benchmark from the
# committed artifact (the full LangGraph reproduction is a separate command).
#
# Fails immediately if any step fails.
#
# NOTE: this file is intentionally ASCII-only so Windows PowerShell 5.1 parses
# it correctly regardless of code page (no BOM required).

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

function Invoke-Step {
    param([string]$Label, [scriptblock]$Body)
    Write-Host ""
    Write-Host "=== $Label ===" -ForegroundColor Cyan
    & $Body
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Label (exit $LASTEXITCODE)" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

# 1. Positioning statement.
Write-Host "===============================================================" -ForegroundColor Magenta
Write-Host "  FamilyClaw - a Rust-native reliability runtime for long-running" -ForegroundColor Magenta
Write-Host "  AI agents: memory, coordination, safe external actions, and" -ForegroundColor Magenta
Write-Host "  crash recovery. Every claim below is proven live or reproducible." -ForegroundColor Magenta
Write-Host "===============================================================" -ForegroundColor Magenta

# 2. Flagship continuity demo.
Invoke-Step "1/2  Flagship continuity demo - two live agents on one bus" {
    cargo run -p familyclaw-agent --example two_agents_memory
}

# 3. Durable crash-replay proof (shortest deterministic crash proof).
#    Pure cargo, no Bash: `full` runs write, then spawns verify as a SEPARATE
#    process, proving the memory survived a real process boundary.
Invoke-Step "2/2  Durable crash-replay proof - write, crash, restart, verify" {
    cargo run -p familyclaw-agent --bin crash_replay -- full
}

# 4. LangGraph comparison summary - from the committed, reproducible artifact.
Write-Host ""
Write-Host "=== Crash-safe dispatch benchmark (summary from committed artifact) ===" -ForegroundColor Cyan
Write-Host "  After a process crash, how many money-touching external side effects re-execute?"
Write-Host ""
Write-Host "    Crash point                                  FamilyClaw   LangGraph"
Write-Host "    clean (no crash)                                  0           0"
Write-Host "    before_write (effect done, record not yet)        0           1"
Write-Host "    mid_replay  (re-crash during replay)              0           2"
Write-Host ""
Write-Host "  Honesty note: this measures duplicate external side-effect dispatch under"
Write-Host "  specific crash windows. It is NOT a throughput, latency, usability, or"
Write-Host "  model-quality comparison. Full numbers: bench-competitors/langgraph/RESULTS.md"

# 5. Exact reproduction commands.
Write-Host ""
Write-Host "=== Reproduce everything yourself ===" -ForegroundColor Cyan
Write-Host "  Flagship demo : cargo run -p familyclaw-agent --example two_agents_memory"
Write-Host "  Crash replay  : cargo run -p familyclaw-agent --bin crash_replay -- full"
Write-Host "  Scorecard (8) : cargo run -p familyclaw-bench --bin bench -- all"
Write-Host "  LangGraph bench (needs Python, separate):"
Write-Host "    cd bench-competitors/langgraph; python -m venv .venv;"
Write-Host "      .venv/Scripts/python.exe -m pip install langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0;"
Write-Host "      .venv/Scripts/python.exe crash_harness.py"

# 6. Capability summary.
Write-Host ""
Write-Host "=== FamilyClaw proves ===" -ForegroundColor Green
Write-Host "  * persistent multi-agent continuity"
Write-Host "  * durable crash replay"
Write-Host "  * duplicate-prevented external action dispatch"
Write-Host "  * approval-gated action execution"
Write-Host "  * model failover with cooldown and key rotation"
Write-Host "  * deterministic local verification"
Write-Host ""
Write-Host "Expo demo complete. See docs/EXPO_BRIEF.md for the full brief." -ForegroundColor Green
