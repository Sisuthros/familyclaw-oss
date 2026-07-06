# Public release demo (Layer A) — no API keys, no private profiles, no channels.
#
# Default mode is fast (~2-4 min on a warm build): the flagship continuity demo,
# a durable crash-replay proof, and the 8-scenario continuity scorecard. It does
# NOT run the full workspace test suite.
#
#   powershell -File scripts/public-demo.ps1          # fast public demo
#   powershell -File scripts/public-demo.ps1 -Full    # full verification (slow)
#
# -Full adds: the entire workspace test suite, the --all-features test suite,
# the comparative LangGraph benchmark, and the Layer B leak audit.

param(
    [switch]$Full
)

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

Write-Host "=== FamilyClaw public demo (Layer A) ===" -ForegroundColor Magenta
Write-Host "No API keys, no network, no private profiles, no paid services."

# 1. Flagship continuity demo — two live agents on the bus.
Invoke-Step "1/3  Flagship continuity demo (two_agents_memory)" {
    cargo run -p familyclaw-agent --example two_agents_memory
}

# 2. Durable crash-replay proof — two-process, write then restart-and-verify.
#    Pure cargo, no Bash: `full` writes, then spawns verify as a separate process.
Invoke-Step "2/3  Durable crash-replay proof" {
    cargo run -p familyclaw-agent --bin crash_replay -- full
}

# 3. Continuity scorecard — 8 deterministic scenarios (s1..s8).
Invoke-Step "3/3  Continuity scorecard (8 scenarios)" {
    cargo run -p familyclaw-bench --bin bench -- all
}

if ($Full) {
    Write-Host ""
    Write-Host "--- Full verification (this is slow) ---" -ForegroundColor Yellow

    Invoke-Step "Full  Workspace test suite" {
        cargo test --workspace --features discord
    }
    Invoke-Step "Full  All-features test suite" {
        cargo test --workspace --all-features
    }
    Invoke-Step "Full  Comparative benchmark (vs LangGraph harness)" {
        cargo run -p familyclaw-bench --bin bench -- compare
    }
    # Layer B leak audit is a Bash script (no pure-cargo equivalent). It is
    # OPTIONAL and gated behind Bash availability so it never aborts the demo on
    # a Windows box without Git-Bash/WSL.
    if (Get-Command bash -ErrorAction SilentlyContinue) {
        Invoke-Step "Full  Layer B leak audit" {
            bash scripts/audit-layer-b.sh
        }
    } else {
        Write-Host "  [skip] Layer B leak audit needs Bash (Git-Bash/WSL)." -ForegroundColor DarkGray
        Write-Host "         Run manually in Git-Bash: bash scripts/audit-layer-b.sh" -ForegroundColor DarkGray
    }
}

Write-Host ""
Write-Host "Public demo complete." -ForegroundColor Green
Write-Host "Scorecard output: crates/familyclaw-bench/out/SCORECARD.md"
Write-Host "Next: see docs/EXPO_BRIEF.md, STATUS.md, and docs/QUICKSTART.md."
