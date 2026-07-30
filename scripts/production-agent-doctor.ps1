# FamilyClaw production-agent truth gate for Windows / PowerShell 5.1+.
#
# This script validates the configuration that makes a real serving agent useful:
# identity, durable storage, provider resolution, key presence, failover syntax,
# Discord ownership, workspace tool allowlists, the offline gateway doctor, and
# the live readiness/canary endpoints. Secret values are never printed.
#
# Usage:
#   . .\scripts\load-env.ps1 -Path "$env:USERPROFILE\.config\familyclaw\familyclaw.env"
#   powershell -ExecutionPolicy Bypass -File scripts\production-agent-doctor.ps1
#
# Optional:
#   powershell -ExecutionPolicy Bypass -File scripts\production-agent-doctor.ps1 -Fix
#   powershell -ExecutionPolicy Bypass -File scripts\production-agent-doctor.ps1 -SkipLive

[CmdletBinding()]
param(
    [string]$Addr = $(if ($env:FAMILYCLAW_GATEWAY_ADDR) { $env:FAMILYCLAW_GATEWAY_ADDR } else { "127.0.0.1:8787" }),
    [string]$EnvFile = "",
    [switch]$Fix,
    [switch]$SkipLive,
    [switch]$SkipCanary,
    [switch]$SkipOfflineDoctor
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($EnvFile) {
    $loader = Join-Path $PSScriptRoot "load-env.ps1"
    if (-not (Test-Path -LiteralPath $loader -PathType Leaf)) {
        Write-Error "Environment loader not found: $loader"
        exit 2
    }
    . $loader -Path $EnvFile
}

$checks = New-Object 'System.Collections.Generic.List[object]'

function Add-Check {
    param(
        [string]$Name,
        [bool]$Ok,
        [bool]$Required,
        [string]$Detail
    )
    $status = if ($Ok) { "PASS" } elseif ($Required) { "FAIL" } else { "WARN" }
    $checks.Add([pscustomobject]@{
        Status   = $status
        Required = $Required
        Check    = $Name
        Detail   = $Detail
    }) | Out-Null
}

function Get-EnvValue {
    param([string]$Name)
    return [Environment]::GetEnvironmentVariable($Name, "Process")
}

function Test-EnvValue {
    param([string]$Name)
    $value = Get-EnvValue $Name
    return -not [string]::IsNullOrWhiteSpace($value)
}

function Test-ProviderModelId {
    param([string]$Model)
    if ([string]::IsNullOrWhiteSpace($Model)) { return $false }
    return $Model -match '^[^/\s]+/[^/\s]+$'
}

function Get-PathList {
    param(
        [string]$Raw,
        [char]$Separator = [IO.Path]::PathSeparator
    )
    if ([string]::IsNullOrWhiteSpace($Raw)) { return @() }
    return @($Raw.Split($Separator) | ForEach-Object { $_.Trim() } | Where-Object { $_ })
}

function Test-OrCreateDirectory {
    param(
        [string]$CheckName,
        [string]$Path,
        [bool]$Required
    )
    if ([string]::IsNullOrWhiteSpace($Path)) {
        Add-Check $CheckName $false $Required "not configured"
        return
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        if ($Fix) {
            try {
                New-Item -ItemType Directory -Force -Path $Path | Out-Null
            }
            catch {
                Add-Check $CheckName $false $Required "directory creation failed"
                return
            }
        }
        else {
            Add-Check $CheckName $false $Required "directory does not exist"
            return
        }
    }
    try {
        $probe = Join-Path $Path (".familyclaw-doctor-" + [Guid]::NewGuid().ToString("N"))
        [IO.File]::WriteAllText($probe, "probe")
        Remove-Item -LiteralPath $probe -Force
        Add-Check $CheckName $true $Required "exists and is writable"
    }
    catch {
        Add-Check $CheckName $false $Required "exists but is not writable"
    }
}

function Test-WorkspaceRoots {
    param(
        [string]$Variable,
        [string]$CheckPrefix,
        [bool]$Required,
        [char]$Separator = [IO.Path]::PathSeparator
    )
    $raw = Get-EnvValue $Variable
    $roots = Get-PathList -Raw $raw -Separator $Separator
    if ($roots.Count -eq 0) {
        Add-Check $CheckPrefix $false $Required "$Variable is empty; the skill will fail closed"
        return
    }
    $allOk = $true
    foreach ($root in $roots) {
        if (-not (Test-Path -LiteralPath $root -PathType Container)) {
            if ($Fix) {
                try { New-Item -ItemType Directory -Force -Path $root | Out-Null }
                catch { $allOk = $false }
            }
            else {
                $allOk = $false
            }
        }
    }
    $detail = if ($allOk) { "$($roots.Count) scoped root(s) exist" } else { "one or more scoped roots do not exist" }
    Add-Check $CheckPrefix $allOk $Required $detail
}

Write-Host "FamilyClaw production-agent doctor" -ForegroundColor Cyan
Write-Host "Target: http://$Addr" -ForegroundColor DarkGray
Write-Host "Secret values are intentionally redacted." -ForegroundColor DarkGray
Write-Host ""

# Identity and durable state.
$agentName = Get-EnvValue "FAMILYCLAW_AGENT_NAME"
Add-Check "agent identity" (-not [string]::IsNullOrWhiteSpace($agentName)) $true $(if ($agentName) { "configured" } else { "FAMILYCLAW_AGENT_NAME is empty" })
Test-OrCreateDirectory "profile directory" (Get-EnvValue "FAMILYCLAW_PROFILE_DIR") $true
Test-OrCreateDirectory "durable data directory" (Get-EnvValue "FAMILYCLAW_DATA_DIR") $true
Add-Check "gateway bearer token" (Test-EnvValue "FAMILYCLAW_GATEWAY_TOKEN") $true $(if (Test-EnvValue "FAMILYCLAW_GATEWAY_TOKEN") { "configured" } else { "missing; approvals and inject surface are not production-gated" })

# Model and provider resolution.
$primaryModel = Get-EnvValue "FAMILYCLAW_PROVIDER_MODEL"
Add-Check "primary model id" (Test-ProviderModelId $primaryModel) $true $(if (Test-ProviderModelId $primaryModel) { "provider/model syntax" } else { "missing or not provider/model syntax" })

$providerSpec = Get-EnvValue "FAMILYCLAW_PROVIDERS"
if ([string]::IsNullOrWhiteSpace($providerSpec)) {
    Add-Check "provider resolver" $false $true "FAMILYCLAW_PROVIDERS is empty; the agent can start mute"
}
else {
    $entries = @($providerSpec.Split(';') | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    $providerOk = $entries.Count -gt 0
    $providerDetails = New-Object 'System.Collections.Generic.List[string]'
    foreach ($entry in $entries) {
        $parts = @($entry -split '=', 3)
        if ($parts.Count -ne 3) {
            $providerOk = $false
            $providerDetails.Add("malformed entry") | Out-Null
            continue
        }
        $prefix = $parts[0].Trim()
        $baseUrl = $parts[1].Trim()
        $keyNames = @($parts[2].Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ })
        $uri = $null
        $uriOk = [Uri]::TryCreate($baseUrl, [UriKind]::Absolute, [ref]$uri) -and ($uri.Scheme -in @("http", "https"))
        $keysOk = $keyNames.Count -gt 0
        foreach ($keyName in $keyNames) {
            if (-not (Test-EnvValue $keyName)) { $keysOk = $false }
        }
        if ([string]::IsNullOrWhiteSpace($prefix) -or -not $uriOk -or -not $keysOk) {
            $providerOk = $false
        }
        $providerDetails.Add("$prefix endpoint=$uriOk keys=$keysOk") | Out-Null
    }
    Add-Check "provider resolver" $providerOk $true ($providerDetails -join "; ")
}

$fallbackRaw = Get-EnvValue "FAMILYCLAW_FALLBACK_MODELS"
$fallbackModels = @()
if (-not [string]::IsNullOrWhiteSpace($fallbackRaw)) {
    $fallbackModels = @($fallbackRaw.Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ })
}
$fallbackOk = $fallbackModels.Count -gt 0
foreach ($fallback in $fallbackModels) {
    if (-not (Test-ProviderModelId $fallback) -or $fallback -eq $primaryModel) { $fallbackOk = $false }
}
Add-Check "fallback model chain" $fallbackOk $false $(if ($fallbackModels.Count -eq 0) { "not configured; provider failure can silence the turn" } elseif ($fallbackOk) { "$($fallbackModels.Count) valid fallback(s)" } else { "invalid, duplicate-primary, or bare model id" })

$maxTokens = 0
$maxTokensOk = [int]::TryParse((Get-EnvValue "FAMILYCLAW_MAX_TOKENS"), [ref]$maxTokens) -and $maxTokens -ge 2048
Add-Check "LLM output budget" $maxTokensOk $false $(if ($maxTokensOk) { "$maxTokens tokens" } else { "missing, invalid, or below 2048" })

$requestTimeout = 0
$requestTimeoutOk = [int]::TryParse((Get-EnvValue "FAMILYCLAW_REQUEST_TIMEOUT_MS"), [ref]$requestTimeout) -and $requestTimeout -ge 5000 -and $requestTimeout -le 300000
Add-Check "LLM request timeout" $requestTimeoutOk $false $(if ($requestTimeoutOk) { "$requestTimeout ms" } else { "use a bounded value between 5000 and 300000 ms" })

# Tool capability boundary.
Test-WorkspaceRoots "FAMILYCLAW_FS_READ_ALLOW" "fs_read capability" $true
Test-WorkspaceRoots "FAMILYCLAW_FS_READ_TRUSTED" "trusted-read roots" $false
Test-WorkspaceRoots "FAMILYCLAW_FILE_WRITE_ALLOW" "file_write capability" $true

$shellMode = Get-EnvValue "FAMILYCLAW_SHELL_MODE"
$shellModeOk = $shellMode -in @("off", "manual", "smart")
Add-Check "shell execution mode" $shellModeOk $true $(if ($shellModeOk) { $shellMode } else { "must be off, manual, or smart" })
if ($shellMode -ne "off") {
    Test-WorkspaceRoots "FAMILYCLAW_SHELL_CWD_ALLOWLIST" "shell working-directory scope" $true ';'
}

$sandboxEnabled = (Get-EnvValue "FAMILYCLAW_SANDBOX_SKILLS") -in @("1", "true", "TRUE", "True")
Add-Check "third-party skill sandbox" $sandboxEnabled $false $(if ($sandboxEnabled) { "requested; offline doctor must confirm the compiled backend" } else { "disabled" })

# Channel and operator ownership.
$channelKind = Get-EnvValue "FAMILYCLAW_CHANNEL_KIND"
Add-Check "channel kind" ($channelKind -in @("discord", "telegram", "none")) $true $(if ($channelKind) { $channelKind } else { "missing" })
if ($channelKind -eq "discord") {
    Add-Check "Discord bot token" (Test-EnvValue "DISCORD_BOT_TOKEN") $true $(if (Test-EnvValue "DISCORD_BOT_TOKEN") { "configured" } else { "missing; two-way Discord mode cannot connect" })
    $discordChannel = Get-EnvValue "DISCORD_CHANNEL_ID"
    $channelId = [uint64]0
    $discordChannelOk = [uint64]::TryParse($discordChannel, [ref]$channelId) -and $channelId -gt 0
    Add-Check "Discord channel id" $discordChannelOk $true $(if ($discordChannelOk) { "numeric snowflake configured" } else { "missing or not numeric" })
    $ownerRaw = Get-EnvValue "FAMILYCLAW_OWNER_ID"
    $ownerId = [uint64]0
    $ownerOk = [uint64]::TryParse($ownerRaw, [ref]$ownerId) -and $ownerId -gt 0
    Add-Check "Discord operator id" $ownerOk $true $(if ($ownerOk) { "numeric operator gate configured" } else { "missing or invalid; operator DMs are denied" })
}
elseif ($channelKind -eq "telegram") {
    Add-Check "Telegram bot token" (Test-EnvValue "TELEGRAM_BOT_TOKEN") $true $(if (Test-EnvValue "TELEGRAM_BOT_TOKEN") { "configured" } else { "missing" })
    Add-Check "Telegram channel id" (Test-EnvValue "FAMILYCLAW_TELEGRAM_CHANNEL_ID") $true $(if (Test-EnvValue "FAMILYCLAW_TELEGRAM_CHANNEL_ID") { "configured" } else { "missing" })
}

# Reuse the repository's authoritative offline doctor when available.
if (-not $SkipOfflineDoctor) {
    $doctorExit = $null
    $doctorDetail = ""
    $gatewayCommand = Get-Command "familyclaw-gateway" -ErrorAction SilentlyContinue
    try {
        if ($gatewayCommand) {
            $doctorArgs = @("doctor")
            if ($Fix) { $doctorArgs += "--fix" }
            $doctorOutput = & $gatewayCommand.Source @doctorArgs 2>&1 | Out-String
            $doctorExit = $LASTEXITCODE
            $doctorDetail = if ($doctorExit -eq 0) { "installed gateway doctor passed" } else { "installed gateway doctor failed" }
        }
        elseif ((Get-Command "cargo" -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath (Join-Path (Get-Location) "Cargo.toml") -PathType Leaf)) {
            $cargoArgs = @("run", "-q", "-p", "familyclaw-gateway", "--", "doctor")
            if ($Fix) { $cargoArgs += "--fix" }
            $doctorOutput = & cargo @cargoArgs 2>&1 | Out-String
            $doctorExit = $LASTEXITCODE
            $doctorDetail = if ($doctorExit -eq 0) { "cargo gateway doctor passed" } else { "cargo gateway doctor failed" }
        }
        else {
            $doctorDetail = "gateway binary and repository cargo path unavailable"
        }
    }
    catch {
        $doctorExit = 1
        $doctorDetail = "offline doctor invocation failed"
    }
    Add-Check "offline gateway doctor" ($doctorExit -eq 0) $true $doctorDetail
}

# Live proof. This catches a provider that is configured syntactically but cannot
# complete, a model that cannot emit tool_calls, a disconnected channel, or a
# journal that is not writable.
if (-not $SkipLive) {
    $base = "http://$Addr"
    try {
        $health = Invoke-WebRequest -Uri "$base/healthz" -UseBasicParsing -TimeoutSec 15
        Add-Check "live healthz" ($health.StatusCode -eq 200) $true "HTTP $($health.StatusCode)"
    }
    catch {
        Add-Check "live healthz" $false $true "gateway is unreachable"
    }

    try {
        $ready = Invoke-RestMethod -Uri "$base/readyz" -Method Get -TimeoutSec 75
        Add-Check "live readyz" ([bool]$ready.ready) $true $(if ($ready.ready) { "all runtime checks passed" } else { "one or more runtime checks failed" })
        foreach ($check in @($ready.checks)) {
            Add-Check ("readyz: " + [string]$check.name) ([bool]$check.ok) $true ([string]$check.detail)
        }
        # `degraded` = checks the gateway deliberately skipped, with the reason.
        # Not a failure, but never silent: a production deployment that reports
        # anything here is knowingly running below full capability.
        foreach ($reason in @($ready.degraded)) {
            if (-not [string]::IsNullOrWhiteSpace([string]$reason)) {
                Add-Check ("readyz degraded: " + [string]$reason) $false $false "check skipped, capability reduced"
            }
        }
    }
    catch {
        Add-Check "live readyz" $false $true "request failed or returned non-success"
    }

    if (-not $SkipCanary) {
        $headers = @{}
        $token = Get-EnvValue "FAMILYCLAW_GATEWAY_TOKEN"
        if (-not [string]::IsNullOrWhiteSpace($token)) {
            $headers["Authorization"] = "Bearer $token"
        }
        try {
            $canary = Invoke-RestMethod -Uri "$base/canary" -Method Post -Headers $headers -TimeoutSec 150
            Add-Check "live canary" ([bool]$canary.ok) $true $(if ($canary.ok) { "synthetic turn passed in $($canary.latency_ms) ms" } else { "synthetic turn failed" })
            foreach ($check in @($canary.checks)) {
                Add-Check ("canary: " + [string]$check.name) ([bool]$check.ok) $true ([string]$check.detail)
            }
        }
        catch {
            Add-Check "live canary" $false $true "request failed or returned non-success"
        }
    }
}

Write-Host ""
$checks | Format-Table Status, Required, Check, Detail -AutoSize | Out-String | Write-Host

$requiredFailures = @($checks | Where-Object { $_.Required -and $_.Status -eq "FAIL" })
$warnings = @($checks | Where-Object { $_.Status -eq "WARN" })
Write-Host ("Required failures: {0}; warnings: {1}" -f $requiredFailures.Count, $warnings.Count)

if ($requiredFailures.Count -gt 0) {
    Write-Host "PRODUCTION GATE: FAIL" -ForegroundColor Red
    exit 1
}

Write-Host "PRODUCTION GATE: PASS" -ForegroundColor Green
exit 0
