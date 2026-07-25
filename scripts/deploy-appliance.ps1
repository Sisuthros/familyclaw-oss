<#
.SYNOPSIS
    Deploy gate for a familyclaw-gateway appliance install: refuses to run
    against a dirty repo, builds a release binary, backs up the running
    exe with a datestamp, replaces it, restarts the appliance, and
    verifies /healthz + /readyz before declaring success.

.DESCRIPTION
    This script is deliberately conservative. It will not run at all if
    `git status --porcelain` in the repo is non-empty -- an uncommitted,
    unreviewed tree must never be what ends up running against a live
    appliance. Every destructive step (build, backup, copy, restart,
    health wait) is gated behind -WhatIf support via SupportsShouldProcess,
    so `-WhatIf` gives a full dry-run preview without touching anything.

    Deploys are deliberate, not automatic: this script does not run on a
    schedule and should only be invoked by a human who has just reviewed
    what's on HEAD. Appliance-specific paths (install directory, start
    script name) are required parameters rather than hardcoded defaults,
    since this script ships in the public repo and must not bake in any
    single operator's local deployment layout.

.PARAMETER RepoRoot
    Path to the familyclaw workspace. Defaults to the current directory.

.PARAMETER ApplianceDir
    Path to the target appliance's operational directory (where its exe,
    logs, and start script live). Required -- no default, since this
    script is public and must not assume any particular operator's layout.

.PARAMETER StartScriptName
    Name of the batch/PowerShell script inside ApplianceDir that starts the
    gateway (sets its env vars and launches the exe). Defaults to 'start.bat'.

.PARAMETER ExeName
    Name of the gateway executable inside ApplianceDir. Defaults to
    'familyclaw-gateway.exe'.

.PARAMETER BaseUrl
    Base URL the appliance's gateway listens on, for the post-deploy
    /healthz + /readyz check. Defaults to http://127.0.0.1:8787.

.PARAMETER HealthTimeoutSec
    How long to wait for /healthz to return 200 after restart, in seconds.

.EXAMPLE
    powershell -File scripts\deploy-appliance.ps1 -ApplianceDir 'D:\my-appliance' -WhatIf
    Dry-run: shows exactly what would happen without building, backing up,
    copying, or restarting anything.

.EXAMPLE
    powershell -File scripts\deploy-appliance.ps1 -ApplianceDir 'D:\my-appliance' -StartScriptName 'run.bat' -BaseUrl 'http://127.0.0.1:8789'
    Full deploy: build, backup, copy, restart, verify.
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$RepoRoot = (Get-Location).Path,
    [Parameter(Mandatory = $true)]
    [string]$ApplianceDir,
    [string]$StartScriptName = 'start.bat',
    [string]$ExeName = 'familyclaw-gateway.exe',
    [string]$BaseUrl = 'http://127.0.0.1:8787',
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
$targetExe = Join-Path $ApplianceDir $ExeName

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
$backupExe = Join-Path $ApplianceDir "$ExeName.bak-$stamp"

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

# --- 4) Stop the running gateway BEFORE overwriting its exe ----------------
# Windows locks a running executable's file. Copying the new build over it
# while the old process is still up throws "the process cannot access the
# file because it is being used by another process" (observed 2026-07-25,
# first real run of this script). Stop here, before the copy, and start
# fresh in step 6 -- that avoids the file-lock race entirely.
if ($PSCmdlet.ShouldProcess('familyclaw-gateway process', 'stop before copying new exe')) {
    Get-Process familyclaw-gateway -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Step "Stopping existing process PID $($_.Id)"
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
} else {
    Write-Step "-WhatIf: would stop the running gateway process before copying the new exe"
}

# --- 5) Copy the new exe into place ----------------------------------------
if ($PSCmdlet.ShouldProcess($builtExe, "copy to $targetExe")) {
    if (-not (Test-Path $builtExe)) { Fail "$builtExe not found -- run without -WhatIf first, or check the build step." }
    Copy-Item -Path $builtExe -Destination $targetExe -Force
    Write-Step "Copied new exe to $targetExe"
} else {
    Write-Step "-WhatIf: would copy $builtExe to $targetExe"
}

# --- 6) Restart via the appliance's start script ----------------------------
$startScript = Join-Path $ApplianceDir $StartScriptName

if ($PSCmdlet.ShouldProcess($startScript, 'start gateway')) {
    if (-not (Test-Path $startScript)) { Fail "$startScript not found -- check -ApplianceDir / -StartScriptName." }
    Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', $startScript -WindowStyle Hidden
    Write-Step "Started $startScript"
} else {
    Write-Step "-WhatIf: would start the gateway via $startScript"
}

# --- 7) Verify /healthz 200 + /readyz ready:true ---------------------------
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