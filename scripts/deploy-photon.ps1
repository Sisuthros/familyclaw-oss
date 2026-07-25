<#
.SYNOPSIS
    Deploy gate for agent_delta's familyclaw-gateway.exe: refuses to run against
    a dirty repo, builds a release binary, backs up the running exe with a
    datestamp, replaces it, restarts via _assistant-run.bat, and verifies
    /healthz + /readyz before declaring success.

.DESCRIPTION
    This script is deliberately conservative. It will not run at all if
    `git status --porcelain` in the repo is non-empty -- an uncommitted,
    unreviewed tree must never be what ends up running against agent_delta's
    live gateway. Every destructive step (build, backup, copy, restart,
    health wait) is gated behind -WhatIf support via SupportsShouldProcess,
    so `-WhatIf` gives a full dry-run preview without touching anything.

    Deploys are deliberate, not automatic: this script does not run on a
    schedule and should only be invoked by a human who has just reviewed
    what's on HEAD.

.PARAMETER RepoRoot
    Path to the familyclaw workspace. Defaults to E:\Familyclaw.

.PARAMETER agent_delta
    Path to agent_delta's operational directory. Defaults to E:\agent_delta.

.PARAMETER HealthTimeoutSec
    How long to wait for /healthz to return 200 after restart, in seconds.

.EXAMPLE
    powershell -File scripts\deploy-agent_delta.ps1 -WhatIf
    Dry-run: shows exactly what would happen without building, backing up,
    copying, or restarting anything.

.EXAMPLE
    powershell -File scripts\deploy-agent_delta.ps1
    Full deploy: build, backup, copy, restart, verify.
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$RepoRoot = 'E:\Familyclaw',
    [string]$agent_delta = 'E:\agent_delta',
    [string]$ExeName = 'familyclaw-gateway.exe',
    [string]$BaseUrl = 'http://127.0.0.1:8789',
    [int]$HealthTimeoutSec = 60
)

$ErrorActionPreference = 'Stop'

function Write-Step {
    param([string]$Message)
    $ts = Get-Date -Format 'yyyy-MM-dd HH:mm:ss'
    Write-Host "[$ts] $Message"
}

function Fail {
    param([string]$Message)
    Write-Host "DEPLOY FAILED: $Message" -ForegroundColor Red
    exit 1
}

# --- 1) Dirty-tree guard --------------------------------------------------
# A deploy must only ever ship what is actually committed and reviewable
# on HEAD. This check always runs, even under -WhatIf, since it is
# read-only and is exactly the information -WhatIf exists to preview.
if (-not (Test-Path (Join-Path $RepoRoot '.git'))) {
    Fail "$RepoRoot does not look like a git repository (no .git dir)."
}

Push-Location $RepoRoot
try {
    $status = git status --porcelain
    if ($LASTEXITCODE -ne 0) {
        Fail "git status failed (exit $LASTEXITCODE) in $RepoRoot."
    }
    if ($status) {
        Write-Host "Refusing to deploy: working tree in $RepoRoot is dirty." -ForegroundColor Red
        Write-Host "Commit or stash the following before deploying:" -ForegroundColor Red
        Write-Host ($status | Out-String)
        exit 1
    }
    $commitHash    = (git rev-parse HEAD).Trim()
    $commitSubject = (git log -1 --format=%s).Trim()
} finally {
    Pop-Location
}
Write-Step "Repo clean. Deploying commit $commitHash ($commitSubject)"

# --- 2) Build release binary ----------------------------------------------
$builtExe  = Join-Path $RepoRoot 'target\release\familyclaw-gateway.exe'
$targetExe = Join-Path $agent_delta $ExeName

if ($PSCmdlet.ShouldProcess($RepoRoot, 'cargo build --release -p familyclaw-gateway --features ollama')) {
    Push-Location $RepoRoot
    try {
        Write-Step "Building release binary (cargo build --release -p familyclaw-gateway --features ollama)..."
        & cargo build --release -p familyclaw-gateway --features ollama
        if ($LASTEXITCODE -ne 0) { Fail "cargo build exited with code $LASTEXITCODE." }
    } finally {
        Pop-Location
    }
    if (-not (Test-Path $builtExe)) { Fail "build succeeded but $builtExe was not found." }
    Write-Step "Build OK: $builtExe"
} else {
    Write-Step "-WhatIf: would run cargo build --release -p familyclaw-gateway --features ollama"
}

# --- 3) Backup the currently-running exe with a datestamp -----------------
$stamp     = Get-Date -Format 'yyyyMMdd-HHmmss'
$backupExe = Join-Path $agent_delta "$ExeName.bak-$stamp"

if ($PSCmdlet.ShouldProcess($targetExe, "back up to $backupExe")) {
    if (Test-Path $targetExe) {
        Copy-Item -Path $targetExe -Destination $backupExe -Force
        Write-Step "Backed up existing exe to $backupExe"
    } else {
        Write-Step "No existing exe at $targetExe (first deploy?) -- nothing to back up."
    }
} else {
    Write-Step "-WhatIf: would back up $targetExe to $backupExe"
}

# --- 4) Copy the new exe into place ----------------------------------------
if ($PSCmdlet.ShouldProcess($builtExe, "copy to $targetExe")) {
    if (-not (Test-Path $builtExe)) { Fail "$builtExe not found -- run without -WhatIf first, or check the build step." }
    Copy-Item -Path $builtExe -Destination $targetExe -Force
    Write-Step "Copied new exe to $targetExe"
} else {
    Write-Step "-WhatIf: would copy $builtExe to $targetExe"
}

# --- 5) Restart via _assistant-run.bat -----------------------------------------
$startScript = Join-Path $agent_delta '_assistant-run.bat'

if ($PSCmdlet.ShouldProcess($startScript, 'stop old process and restart gateway')) {
    Get-Process familyclaw-gateway -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Step "Stopping existing process PID $($_.Id)"
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
    Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', $startScript -WindowStyle Hidden
    Write-Step "Started $startScript"
} else {
    Write-Step "-WhatIf: would stop the running gateway process and restart it via $startScript"
}

# --- 6) Verify /healthz 200 + /readyz ready:true ---------------------------
if ($PSCmdlet.ShouldProcess($BaseUrl, 'wait for /healthz 200 and /readyz ready:true')) {
    $healthy = $false
    $deadline = (Get-Date).AddSeconds($HealthTimeoutSec)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 2
        try {
            $h = Invoke-WebRequest -Uri "$BaseUrl/healthz" -TimeoutSec 5 -UseBasicParsing
            if ($h.StatusCode -eq 200) { $healthy = $true; break }
        } catch {}
    }
    if (-not $healthy) {
        Write-Host "DEPLOY FAILED: /healthz did not return 200 within ${HealthTimeoutSec}s." -ForegroundColor Red
        Write-Host "The pre-deploy backup is at $backupExe -- restore manually if needed." -ForegroundColor Yellow
        exit 1
    }
    Write-Step "/healthz OK (200)"

    $ready = $false
    $readyDetail = ''
    try {
        $r = Invoke-WebRequest -Uri "$BaseUrl/readyz" -TimeoutSec 30 -UseBasicParsing
        if ($r.StatusCode -eq 200) {
            $body = $r.Content | ConvertFrom-Json
            $ready = [bool]$body.ready
            $readyDetail = $r.Content
        } else {
            $readyDetail = "HTTP $($r.StatusCode)"
        }
    } catch {
        $readyDetail = $_.Exception.Message
    }
    if (-not $ready) {
        Write-Host "DEPLOY FAILED: /readyz did not report ready:true." -ForegroundColor Red
        Write-Host "Response: $readyDetail" -ForegroundColor Red
        Write-Host "The pre-deploy backup is at $backupExe -- restore manually if needed." -ForegroundColor Yellow
        exit 1
    }
    Write-Step "/readyz OK (ready:true)"
} else {
    Write-Step "-WhatIf: would wait up to ${HealthTimeoutSec}s for /healthz 200 and /readyz ready:true"
}

Write-Host ''
Write-Host "DEPLOY OK: commit $commitHash ($commitSubject) is live on $BaseUrl." -ForegroundColor Green