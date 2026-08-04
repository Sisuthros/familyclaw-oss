<#
.SYNOPSIS
  FamilyClaw Installer (Windows) — KERROS A (OSS) binary + KERROS B (private) service template.
  Windows-vastine install.sh:lle. Käyttää Scheduled Taskia systemd:n sijaan.

.DESCRIPTION
  Tarkistaa Rust-toolchainin, synkkaa repon, kääntää gateway-binäärin, asentaa sen,
  luo run-skripti-templaten (Layer B -arvot env:stä, EI gitiin) ja valinnaisesti
  rekisteröi At-logon Scheduled Taskin. Vastaa <5 min cold start -tavoitetta (G5).

.PARAMETER Agent
  Agentin nimi (FAMILYCLAW_AGENT_NAME). Oletus: familyclaw.

.PARAMETER Prefix
  Asennuskansio binäärille. Oletus: $env:LOCALAPPDATA\FamilyClaw.

.PARAMETER RegisterTask
  Rekisteröi At-logon Scheduled Taskin (Hermes-perheen mallin mukaisesti).

.PARAMETER RepoOnly
  Vain kloonaa/päivitä repo, älä käännä/asenna.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File install.ps1 -Agent familyclaw -RegisterTask

.NOTES
  Layer B (EI koskaan gitiin) — aseta nämä run-skriptiin / ympäristöön:
    FAMILYCLAW_PROFILE_DIR, DISCORD_BOT_TOKEN, FAMILYCLAW_REPLY_TARGET,
    FAMILYCLAW_PROVIDER_MODEL, FAMILYCLAW_FALLBACK_MODELS, <PROVIDER>_API_KEY
#>
[CmdletBinding()]
param(
    [string]$Agent = "familyclaw",
    [string]$Prefix = "$env:LOCALAPPDATA\FamilyClaw",
    [switch]$RegisterTask,
    [switch]$RepoOnly
)

$ErrorActionPreference = "Stop"
$RepoUrl  = "https://github.com/Sisuthros/familyclaw-oss.git"
$RepoDir  = "$env:LOCALAPPDATA\familyclaw-source"
$BinName  = "familyclaw-gateway.exe"
$BinDir   = Join-Path $Prefix "bin"

function Log-Info  { param($m) Write-Host "[INFO] $m"  -ForegroundColor Blue }
function Log-Ok    { param($m) Write-Host "[OK]   $m"  -ForegroundColor Green }
function Log-Warn  { param($m) Write-Host "[WARN] $m"  -ForegroundColor Yellow }
function Log-Error { param($m) Write-Host "[ERROR] $m" -ForegroundColor Red; exit 1 }

Write-Host "==============================================="
Write-Host "  FamilyClaw Gateway Installer (Windows)"
Write-Host "  Repo:   $RepoUrl"
Write-Host "  Agent:  $Agent"
Write-Host "  Prefix: $Prefix"
Write-Host "==============================================="

# ── Check Rust ──────────────────────────────────────────────────────
function Test-Rust {
    Log-Info "Checking Rust toolchain..."
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Log-Error "Rust/cargo not found. Install from https://rustup.rs then re-run."
    }
    $ver = (rustc --version) -split ' '
    Log-Ok "Rust $($ver[1]) available"
}

# ── Clone / Update repo ─────────────────────────────────────────────
function Sync-Repo {
    Log-Info "Syncing repository..."
    if (Test-Path (Join-Path $RepoDir ".git")) {
        git -C $RepoDir fetch --quiet origin
        git -C $RepoDir reset --quiet --hard origin/main
        Log-Ok "Repository updated"
    } else {
        git clone --quiet $RepoUrl $RepoDir
        Log-Ok "Repository cloned"
    }
}

# ── Build ───────────────────────────────────────────────────────────
function Build-Binary {
    Log-Info "Building $BinName (release)..."
    Push-Location $RepoDir
    try {
        # PYTHONUTF8 ei tarvita; Rust-build on natiivi. --locked = toistettava.
        cargo build --release -p familyclaw-gateway --locked
        if ($LASTEXITCODE -ne 0) { Log-Error "cargo build failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
    Log-Ok "Build complete"
}

# ── Install binary ──────────────────────────────────────────────────
function Install-Binary {
    Log-Info "Installing binary to $BinDir..."
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    Copy-Item (Join-Path $RepoDir "target\release\$BinName") (Join-Path $BinDir $BinName) -Force
    Log-Ok "Binary installed at $BinDir\$BinName"
}

# ── Create run-script template (Layer B) ────────────────────────────
function New-RunTemplate {
    Log-Info "Creating run-script template..."
    $runScript = Join-Path $Prefix "run-$Agent.cmd"
    $bin = Join-Path $BinDir $BinName
    # KERROS B: template — TÄYTÄ omat yksityiset arvot. ÄLÄ committaa gitiin.
    $content = @"
@echo off
REM FamilyClaw run-script — $Agent (private config template; keep out of git)
REM Fill in your private values below. Secrets can also be read from a .env file.
set FAMILYCLAW_AGENT_NAME=$Agent
set FAMILYCLAW_CHANNEL_KIND=discord
REM Bind to localhost by default. Use 0.0.0.0 ONLY when you explicitly intend
REM to expose the gateway on the network (and have a firewall / reverse proxy
REM in front of it). The default is safe for a single-machine deployment.
set FAMILYCLAW_GATEWAY_ADDR=127.0.0.1:8789
REM --- Fill these in for your own deployment (private config, keep out of git) ---
REM set FAMILYCLAW_PROFILE_DIR=C:\path\to\your\profile
REM set DISCORD_BOT_TOKEN=...
REM set FAMILYCLAW_REPLY_TARGET=...
REM set FAMILYCLAW_PROVIDER_MODEL=your-provider/your-model
REM set FAMILYCLAW_FALLBACK_MODELS=your-provider/fallback-a,your-provider/fallback-b
REM set OPENAI_API_KEY=...
"$bin" serve
"@
    Set-Content -Path $runScript -Value $content -Encoding ASCII
    Log-Ok "Run template: $runScript"
    Log-Warn "Edit $runScript with your Layer B values before starting!"
    return $runScript
}

# ── Register Scheduled Task (optional) ──────────────────────────────
function Register-FamilyClawTask {
    param($RunScript)
    $taskName = "FamilyClaw_$Agent"
    Log-Info "Registering Scheduled Task '$taskName' (At logon)..."
    $action  = New-ScheduledTaskAction -Execute "cmd.exe" -Argument "/c `"$RunScript`""
    $trigger = New-ScheduledTaskTrigger -AtLogOn
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null
    Log-Ok "Scheduled Task '$taskName' registered (runs at logon)"
}

# ── Summary ─────────────────────────────────────────────────────────
function Write-Summary {
    param($RunScript)
    Write-Host ""
    Log-Ok "==============================================="
    Log-Ok "  FamilyClaw Gateway installed!"
    Log-Ok "==============================================="
    Write-Host ""
    Write-Host "Binary:      $BinDir\$BinName"
    Write-Host "Run script:  $RunScript"
    Write-Host ""
    Write-Host "Next steps (Layer B - private, never in repo):"
    Write-Host "  1. Edit $RunScript with your secrets + model config"
    Write-Host "  2. Start:   cmd /c `"$RunScript`""
    Write-Host "  3. Verify:  Invoke-WebRequest http://127.0.0.1:8789/healthz   # -> ok"
    Write-Host "              $BinDir\$BinName doctor"
    Write-Host ""
    Write-Host "Docs: https://github.com/Sisuthros/familyclaw-oss"
}

# ── Main ────────────────────────────────────────────────────────────
Test-Rust
Sync-Repo
if ($RepoOnly) { Log-Ok "Repo synced to $RepoDir"; exit 0 }
Build-Binary
Install-Binary
$run = New-RunTemplate
if ($RegisterTask) { Register-FamilyClawTask -RunScript $run }
Write-Summary -RunScript $run
