# FamilyClaw - Booth export (Windows). Prepare a PUBLIC-SAFE demo tree.
#
#   powershell -File scripts/booth-export.ps1 [-OutDir <path>]
#
# Produces a clean demo folder that is SAFE to put on a public booth machine:
#   1. builds the release demo binaries,
#   2. exports the tracked working tree via `git archive` (NO .git directory, so
#      the private git history — which leaks Layer B names — is NOT carried),
#   3. copies the prebuilt binaries into the export so the demo runs even with a
#      broken toolchain or no network,
#   4. records the source commit in booth/COMMIT.txt.
#
# The export contains NO git history. Never copy the .git directory to a booth.
#
# NOTE: intentionally ASCII-only for Windows PowerShell 5.1.

param([string]$OutDir = "booth")

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)
$sha = (git rev-parse --short HEAD)
$full = (git rev-parse HEAD)

Write-Host "=== FamilyClaw booth export (commit $sha) ===" -ForegroundColor Magenta

# 1. Build release binaries.
Write-Host "Building release demo binaries..." -ForegroundColor Cyan
cargo build --release -p familyclaw-agent --example two_agents_memory
cargo build --release -p familyclaw-agent --bin crash_replay
cargo build --release -p familyclaw-bench --bin bench
if ($LASTEXITCODE -ne 0) { throw "release build failed" }

# 2. Clean the target dir and export the tracked tree WITHOUT .git.
if (Test-Path $OutDir) { Remove-Item -Recurse -Force $OutDir }
New-Item -ItemType Directory -Path $OutDir | Out-Null
Write-Host "Exporting tracked tree via git archive (no .git)..." -ForegroundColor Cyan
git archive --format=tar HEAD | tar -x -C $OutDir
if (-not (Test-Path (Join-Path $OutDir "Cargo.toml"))) { throw "git archive export failed" }

# 3. Copy prebuilt binaries into the export.
$bin = Join-Path $OutDir "bin"
New-Item -ItemType Directory -Path $bin -Force | Out-Null
Copy-Item "target/release/crash_replay.exe" $bin
Copy-Item "target/release/bench.exe" $bin
Copy-Item "target/release/examples/two_agents_memory.exe" $bin

# 4. Record the source commit + usage.
@"
FamilyClaw booth export
source commit: $full
exported:      (stamp at export time)

Prebuilt binaries in bin/ (no toolchain or network needed):
  bin\two_agents_memory.exe          # flagship continuity demo
  bin\crash_replay.exe full          # durable crash-replay proof
  bin\bench.exe all                  # 8-scenario deterministic scorecard

This folder has NO .git directory. Do not run 'git log' here (there is no
history to leak). Safe for a public booth machine.
"@ | Set-Content (Join-Path $OutDir "COMMIT.txt")

# 5. Privacy assertion: there must be no .git in the export.
if (Test-Path (Join-Path $OutDir ".git")) {
    throw "SAFETY FAIL: .git present in export — do NOT use this on a booth."
}

Write-Host ""
Write-Host "Booth export ready at: $OutDir" -ForegroundColor Green
Write-Host "  No .git (history not carried). Prebuilt binaries in $OutDir\bin." -ForegroundColor Green
Write-Host "  Fallback demo (no toolchain needed):" -ForegroundColor Green
Write-Host "    $OutDir\bin\crash_replay.exe full"
