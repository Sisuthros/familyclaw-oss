#!/usr/bin/env pwsh
# Verify Docker volume survives kill + restart (single-tenant appliance).
$ErrorActionPreference = "Stop"
if (-not $env:FAMILYCLAW_GATEWAY_TOKEN) {
  throw "set FAMILYCLAW_GATEWAY_TOKEN before running"
}
Set-Location (Split-Path -Parent $PSScriptRoot)

docker compose up -d --build
Start-Sleep -Seconds 5
Invoke-WebRequest -Uri "http://127.0.0.1:8787/healthz" -UseBasicParsing | Out-Null

$cid = (docker compose ps -q gateway).Trim()
docker kill -s KILL $cid
Start-Sleep -Seconds 2
docker compose up -d
Start-Sleep -Seconds 5
Invoke-WebRequest -Uri "http://127.0.0.1:8787/healthz" -UseBasicParsing | Out-Null
Invoke-WebRequest -Uri "http://127.0.0.1:8787/readyz" -UseBasicParsing | Out-Null

$ts = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
Write-Host "VERIFIED $ts docker kill/restart with volume + token"
