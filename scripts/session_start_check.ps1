$ErrorActionPreference = "Stop"

$root = git rev-parse --show-toplevel
if (-not $root) {
  throw "ERROR: not inside a git repository"
}

Set-Location $root
Write-Host "Repository root: $root"

if (-not (Test-Path "Cargo.toml")) {
  throw "ERROR: Cargo.toml not found at repository root"
}

Write-Host "OK: running from FamilyClaw repo root"
