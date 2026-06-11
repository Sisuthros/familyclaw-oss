# Initialize JSON memory store for FamilyClaw MVP (Windows).
# Usage: powershell -File scripts/init-familyclaw-data.ps1

param(
    [string]$DataDir = $env:FAMILYCLAW_DATA_DIR
)

if (-not $DataDir) {
    $DataDir = "E:\familyclaw-data"
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
