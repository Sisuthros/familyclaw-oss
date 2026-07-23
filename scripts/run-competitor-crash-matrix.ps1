#!/usr/bin/env pwsh
# Run OpenClaw-shaped + Hermes-shaped crash matrices (Windows).
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
if (-not $Root) { $Root = (Resolve-Path "$PSScriptRoot\..").Path }
$Py = if ($env:PYTHON) { $env:PYTHON } else { "python" }

function Invoke-CompetitorMatrix([string]$Dir) {
  Write-Host "=== $Dir ==="
  foreach ($cp in @("clean", "before_write", "mid_replay")) {
    & $Py "$Root\bench-competitors\$Dir\crash_harness.py" cycle `
      --crash-point $cp --workdir "$Root\bench-competitors\$Dir\_runs\$cp"
  }
}

Invoke-CompetitorMatrix openclaw
Invoke-CompetitorMatrix hermes
Write-Host "Done. FamilyClaw: cargo run -p familyclaw-bench -- s1"
