# FamilyClaw gateway watchdog — kanarialintu 5 min välein.
# Layer B: kopioi/kytke operaattorin data-hakemistoon (esim. agent_delta-deploy).
#
# Käyttö:
#   .\scripts\watchdog.ps1
# Ympäristö:
#   FAMILYCLAW_GATEWAY_ADDR (oletus 127.0.0.1:8787)
#   FAMILYCLAW_GATEWAY_TOKEN (valinnainen, bearer /canary:lle jos suojattu)

param(
    [string]$Addr = $(if ($env:FAMILYCLAW_GATEWAY_ADDR) { $env:FAMILYCLAW_GATEWAY_ADDR } else { "127.0.0.1:8787" }),
    [int]$IntervalSec = 300
)

$base = "http://$Addr"
$logDir = Join-Path $PSScriptRoot ".." "logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$logFile = Join-Path $logDir "watchdog-canary.log"

function Write-Log([string]$msg) {
    $line = "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $msg"
    Add-Content -Path $logFile -Value $line
    Write-Host $line
}

$headers = @{}
if ($env:FAMILYCLAW_GATEWAY_TOKEN) {
    $headers["Authorization"] = "Bearer $($env:FAMILYCLAW_GATEWAY_TOKEN)"
}

Write-Log "watchdog start — interval ${IntervalSec}s, target $base"

while ($true) {
    try {
        $health = Invoke-WebRequest -Uri "$base/healthz" -UseBasicParsing -TimeoutSec 15
        $ready = Invoke-WebRequest -Uri "$base/readyz" -UseBasicParsing -TimeoutSec 60
        $canary = Invoke-WebRequest -Uri "$base/canary" -Method POST -Headers $headers -UseBasicParsing -TimeoutSec 120
        Write-Log "ok health=$($health.StatusCode) ready=$($ready.StatusCode) canary=$($canary.StatusCode) body=$($canary.Content)"
    }
    catch {
        Write-Log "FAIL $($_.Exception.Message)"
    }
    Start-Sleep -Seconds $IntervalSec
}
