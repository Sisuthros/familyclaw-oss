# FamilyClaw - Expo preflight (Windows). Run this BEFORE the booth opens.
#
#   powershell -File scripts/expo-preflight.ps1
#
# Verifies the machine can run the live demo end-to-end. Prints the commit,
# checks the toolchain, confirms required files exist, builds the demo binaries,
# runs the shortest critical tests, runs the flagship demo, runs the crash
# replay, then reports a single PASS/FAIL and returns a correct exit code.
#
# No API keys, no network, no paid services. Safe to run repeatedly.
#
# NOTE: intentionally ASCII-only so Windows PowerShell 5.1 parses it regardless
# of code page (no BOM required).

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

$fail = 0
function Check {
    param([string]$Label, [scriptblock]$Body)
    Write-Host ""
    Write-Host "=== $Label ===" -ForegroundColor Cyan
    try {
        & $Body
        if ($LASTEXITCODE -ne $null -and $LASTEXITCODE -ne 0) {
            throw "exit $LASTEXITCODE"
        }
        Write-Host "  PASS: $Label" -ForegroundColor Green
    } catch {
        Write-Host "  FAIL: $Label -- $_" -ForegroundColor Red
        $script:fail = 1
    }
}

Write-Host "===============================================================" -ForegroundColor Magenta
Write-Host "  FamilyClaw - Expo preflight" -ForegroundColor Magenta
Write-Host "===============================================================" -ForegroundColor Magenta

# 1. Commit / branch.
Check "Repository commit" {
    $sha = (git rev-parse --short HEAD)
    $branch = (git rev-parse --abbrev-ref HEAD)
    Write-Host "  commit $sha on $branch"
}

# 2. Toolchain.
Check "Rust / Cargo available" {
    cargo --version
    rustc --version
}

# 3. Required files exist.
Check "Required demo files present" {
    $required = @(
        "crates/familyclaw-agent/examples/two_agents_memory.rs",
        "crates/familyclaw-agent/src/bin/crash_replay.rs",
        "crates/familyclaw-bench/src/bin/bench.rs",
        "scripts/expo-demo.ps1",
        "docs/EXPO_BRIEF.md",
        "docs/EXPO_VALIDATION_PROOF.md"
    )
    foreach ($f in $required) {
        if (-not (Test-Path $f)) { throw "missing $f" }
        Write-Host "  ok  $f"
    }
}

# 4. Build the demo binaries (warms the cache so the live demo is fast).
Check "Build demo binaries" {
    cargo build -p familyclaw-agent --example two_agents_memory
    cargo build -p familyclaw-agent --bin crash_replay
    cargo build -p familyclaw-bench --bin bench
}

# 5. Shortest critical tests (durable replay is the load-bearing wedge).
Check "Critical tests (durable replay)" {
    cargo test -p familyclaw-durable
}

# 6. Flagship demo actually runs and self-asserts (exits non-zero on any failure).
Check "Flagship demo (two_agents_memory)" {
    cargo run -p familyclaw-agent --example two_agents_memory
}

# 7. Crash replay actually survives a process boundary.
Check "Durable crash replay (full)" {
    cargo run -p familyclaw-agent --bin crash_replay -- full
}

# 8. Privacy guard: the demo tree must NOT expose git history at the booth.
#    This is a WARNING here (the working repo legitimately has .git); for a
#    booth machine, demo from a `git archive` export with no .git directory.
Write-Host ""
Write-Host "=== Privacy guard (booth machines) ===" -ForegroundColor Cyan
if (Test-Path ".git") {
    Write-Host "  WARN: .git present. On a PUBLIC booth machine, run the demo from a" -ForegroundColor Yellow
    Write-Host "        clean export (git archive) with no .git, and do not run 'git log'." -ForegroundColor Yellow
    Write-Host "        Git history contains private Layer B names." -ForegroundColor Yellow
} else {
    Write-Host "  PASS: no .git in this tree (clean export)." -ForegroundColor Green
}

Write-Host ""
Write-Host "===============================================================" -ForegroundColor Magenta
if ($fail -eq 0) {
    Write-Host "  PREFLIGHT PASS - the machine is demo-ready." -ForegroundColor Green
} else {
    Write-Host "  PREFLIGHT FAIL - fix the FAILs above before the booth opens." -ForegroundColor Red
}
Write-Host "===============================================================" -ForegroundColor Magenta
exit $fail
