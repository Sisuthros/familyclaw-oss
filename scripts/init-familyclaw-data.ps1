# Initialize JSON memory store (LocalJsonStore MVP). No RocksDB lock issues.
#
# Usage:
#   powershell -File scripts/init-familyclaw-data.ps1
#   powershell -File scripts/init-familyclaw-data.ps1 -DataDir C:\path\to\data

param(
    [string]$DataDir = $env:FAMILYCLAW_DATA_DIR
)

if (-not $DataDir) {
    $repoRoot = Split-Path $PSScriptRoot -Parent
    $DataDir = Join-Path $repoRoot ".local" "data"
}

New-Item -ItemType Directory -Force -Path $DataDir | Out-Null

$journal = Join-Path $DataDir "journal.jsonl"
$memory = Join-Path $DataDir "memory.json"

if (-not (Test-Path $journal)) {
    New-Item -ItemType File -Path $journal -Force | Out-Null
    Write-Host "Created $journal"
} else {
    Write-Host "Exists $journal"
}

if (-not (Test-Path $memory)) {
    @'
{
  "version": 1,
  "memories": []
}
'@ | Set-Content -Path $memory -Encoding utf8
    Write-Host "Created $memory"
} else {
    Write-Host "Exists $memory"
}

Write-Host "FAMILYCLAW_DATA_DIR ready: $DataDir"
Write-Host "Export: `$env:FAMILYCLAW_DATA_DIR = `"$DataDir`""
