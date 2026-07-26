<#
.SYNOPSIS
    Family-wide launcher: hosts the whole family's gateways from ONE manifest
    and ONE command, instead of hand-registering a separate Scheduled Task per
    agent.

.DESCRIPTION
    Today "run the whole family" means a human manually creates N Scheduled
    Tasks, one per agent, each with its own copy-pasted arguments and its own
    per-agent watcher script. That does not scale past a handful of agents
    and is easy to get out of sync (wrong port, stale env path, forgotten
    agent).

    This script reads a single JSON manifest (scripts\family.manifest.example.json
    is the template - copy it outside the repo and fill in real paths) listing
    every family member, and launches one per-agent watcher/supervisor child
    process per listed agent from that ONE config + ONE command. A human (or
    a single Scheduled Task running this launcher) now manages ONE process
    tree instead of N independently registered tasks.

    This is a process **supervisor of supervisors** (a fleet launcher), not a
    new gateway feature - the gateway binary itself still binds one channel
    per process. See docs\design\multi-agent-single-process-gateway.md for
    the deeper, not-yet-built "single OS process hosts N agents + channels
    natively" design and why it is out of scope for this slice.

    Per-agent supervision (restart-on-death, /healthz polling) is delegated
    to an external watcher script (Layer B / operator-local, e.g. a copy of
    the family's `familyclaw-supervise.ps1`, kept outside this repo like all
    per-agent secrets/config) — this launcher does not reimplement that
    loop, it fans a single command out into N of them. Point -SuperviseScript
    at your watcher, or set FAMILYCLAW_SUPERVISE_SCRIPT.

.PARAMETER Manifest
    Path to the family manifest JSON (see family.manifest.example.json for
    the schema). Required for -Launch / -DryRun.

.PARAMETER Agents
    Optional filter: only launch/stop/status these agent names (pass as a
    PowerShell array, e.g. -Agents agent_alpha,agent_beta). Default: all agents in the
    manifest (or, for -Stop/-Status, all agents in the state file).

.PARAMETER DryRun
    Validate the manifest + resolve every env file path (expanding
    %VARS%) and print the planned launch for each agent, WITHOUT starting
    any process or requiring a real -SuperviseScript. Safe to run without
    real secrets - this is the primary way to test this script.

.PARAMETER Stop
    Stop every PID recorded in the launcher's state file (from a previous
    non-DryRun launch) and clear the state file.

.PARAMETER Status
    Print liveness (process alive/dead) for every agent recorded in the
    state file, without starting or stopping anything.

.PARAMETER SuperviseScript
    Path to the per-agent watcher/supervisor script (e.g.
    familyclaw-supervise.ps1). Required for a real (non-DryRun) launch.
    Falls back to $env:FAMILYCLAW_SUPERVISE_SCRIPT if not passed.

.PARAMETER GatewayExe
    Optional: forwarded to the watcher script as -GatewayExe for each agent.

.PARAMETER StateFile
    Where launched-agent PIDs are recorded so -Stop / -Status can find them.
    Default: <repo>\ops\logs\family-launcher.pids.json (ops\ is operator-local
    and gitignored - see .gitignore).

.EXAMPLE
    # Validate the manifest without touching any process (no secrets needed):
    powershell -ExecutionPolicy Bypass -File E:\Familyclaw\scripts\familyclaw-family-launcher.ps1 `
        -Manifest E:\Familyclaw\scripts\family.manifest.example.json -DryRun

.EXAMPLE
    # Launch the whole family from one real manifest:
    powershell -ExecutionPolicy Bypass -File E:\Familyclaw\scripts\familyclaw-family-launcher.ps1 `
        -Manifest C:\Users\operator\.config\familyclaw\family.manifest.json `
        -SuperviseScript E:\Familyclaw\ops\familyclaw-supervise.ps1

.EXAMPLE
    # Launch only two members:
    ...\familyclaw-family-launcher.ps1 -Manifest <path> -SuperviseScript <path> -Agents agent_alpha,agent_beta

.EXAMPLE
    # Check on everything the launcher started:
    ...\familyclaw-family-launcher.ps1 -Status

.EXAMPLE
    # Stop the whole family:
    ...\familyclaw-family-launcher.ps1 -Stop

.NOTES
    This script does NOT register anything with Task Scheduler. Register ONE
    Scheduled Task that runs this launcher (adapt ops\AUTOSTART.md "Option A"
    to call this script instead of the per-agent watcher directly) to get
    "the whole family autostarts from one task" instead of one task per agent.
#>

[CmdletBinding(DefaultParameterSetName = 'Launch')]
param(
    [Parameter(ParameterSetName = 'Launch')]
    [string]$Manifest,

    [Parameter(ParameterSetName = 'Launch')]
    [Parameter(ParameterSetName = 'Stop')]
    [Parameter(ParameterSetName = 'Status')]
    [string[]]$Agents,

    [Parameter(ParameterSetName = 'Launch')]
    [switch]$DryRun,

    [Parameter(ParameterSetName = 'Launch')]
    [string]$GatewayExe,

    [Parameter(ParameterSetName = 'Stop', Mandatory = $true)]
    [switch]$Stop,

    [Parameter(ParameterSetName = 'Status', Mandatory = $true)]
    [switch]$Status,

    [string]$SuperviseScript,
    [string]$StateFile
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot   # ...\Familyclaw\scripts -> ...\Familyclaw

if (-not $SuperviseScript -and $env:FAMILYCLAW_SUPERVISE_SCRIPT) {
    $SuperviseScript = $env:FAMILYCLAW_SUPERVISE_SCRIPT
}
# -SuperviseScript is only required for a REAL launch (not -DryRun/-Stop/-Status),
# so it is validated lazily below, right before it would be used.

$LogDir = Join-Path $RepoRoot "ops\logs"
if (-not $StateFile) {
    if (-not (Test-Path $LogDir)) { New-Item -ItemType Directory -Path $LogDir -Force | Out-Null }
    $StateFile = Join-Path $LogDir "family-launcher.pids.json"
}
$stateFileDir = Split-Path -Parent $StateFile
if ($stateFileDir -and -not (Test-Path $stateFileDir)) {
    New-Item -ItemType Directory -Path $stateFileDir -Force | Out-Null
}

$LauncherLog = Join-Path $LogDir "family-launcher.log"
function Write-Log {
    param([string]$Message, [ValidateSet("INFO", "WARN", "ERROR")][string]$Level = "INFO")
    $ts = (Get-Date).ToString("yyyy-MM-dd HH:mm:ss.fff zzz")
    $line = "[$ts] [$Level] [family-launcher] $Message"
    try {
        if (-not (Test-Path $LogDir)) { New-Item -ItemType Directory -Path $LogDir -Force | Out-Null }
        Add-Content -Path $LauncherLog -Value $line -Encoding UTF8
    } catch { }
    switch ($Level) {
        "ERROR" { Write-Host $line -ForegroundColor Red }
        "WARN"  { Write-Host $line -ForegroundColor Yellow }
        default { Write-Host $line }
    }
}

function Read-Manifest {
    param([string]$Path)
    if (-not $Path) {
        throw "Manifest is required for -Launch / -DryRun. See family.manifest.example.json."
    }
    if (-not (Test-Path $Path)) {
        throw "Manifest not found: $Path"
    }
    $raw = Get-Content -Path $Path -Raw
    $parsed = $raw | ConvertFrom-Json
    $agentList = @($parsed.agents)
    if ($agentList.Count -eq 0) {
        throw "Manifest '$Path' has no agents[] entries."
    }
    # Unary comma: PowerShell's `return` unwinds a 0- or 1-element array back
    # to $null / a bare scalar through the output pipeline. The comma forces
    # the caller to always receive a real array, even for a single agent.
    return , $agentList
}

function Resolve-EnvPath {
    param([string]$Raw)
    # Expand both %VAR% (cmd-style, used in the example manifest for
    # cross-shell readability) and PowerShell $env: forms.
    [System.Environment]::ExpandEnvironmentVariables($Raw)
}

function Get-PropOrDefault {
    param($Obj, [string]$Name, $Default)
    if ($Obj.PSObject.Properties.Name -contains $Name) {
        $v = $Obj.$Name
        if ($null -ne $v) { return $v }
    }
    return $Default
}

function Select-Agents {
    param($AllAgents, [string[]]$Filter)
    # See the comment in Read-Manifest: `return` unwinds 0/1-element arrays,
    # so every returned collection here is force-wrapped with the comma operator.
    $all = @($AllAgents)
    if (-not $Filter -or @($Filter).Count -eq 0) { return , $all }
    $wanted = @($Filter | ForEach-Object { $_.ToLowerInvariant() })
    $matched = @($all | Where-Object { $wanted -contains $_.name.ToLowerInvariant() })
    return , $matched
}

function Save-State {
    param($Entries)
    $list = @($Entries)
    # `@() | ConvertTo-Json` emits NOTHING (not "[]"), so piping an empty
    # array into Set-Content never invokes it and the file is left
    # untouched - write the literal empty-array JSON directly instead.
    if ($list.Count -eq 0) {
        Set-Content -Path $StateFile -Value "[]" -Encoding UTF8
        return
    }
    # ConvertTo-Json renders a single-element array as a bare object
    # (`{...}`) unless -AsArray is available; wrap with -AsArray when
    # supported (PS7+) or fall back to manual bracket-wrapping (PS 5.1).
    if ($list.Count -eq 1) {
        if ((Get-Command ConvertTo-Json).Parameters.ContainsKey('AsArray')) {
            $list | ConvertTo-Json -Depth 5 -AsArray | Set-Content -Path $StateFile -Encoding UTF8
        } else {
            $inner = $list | ConvertTo-Json -Depth 5
            Set-Content -Path $StateFile -Value "[$inner]" -Encoding UTF8
        }
        return
    }
    $list | ConvertTo-Json -Depth 5 | Set-Content -Path $StateFile -Encoding UTF8
}

function Load-State {
    # Every branch returns via the comma operator (see Read-Manifest comment)
    # so callers always get a real array, even empty or single-element.
    if (-not (Test-Path $StateFile)) { return , @() }
    $raw = Get-Content -Path $StateFile -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) { return , @() }
    $parsed = $raw | ConvertFrom-Json
    if ($null -eq $parsed) { return , @() }
    # ConvertFrom-Json returns a single PSCustomObject (not an array) when the
    # JSON array had exactly one element - normalize to an array either way.
    return , @($parsed)
}

# =========================== -Status ===========================
if ($Status) {
    $state = Load-State
    if ($state.Count -eq 0) {
        Write-Log "no launcher state found at '$StateFile' (nothing launched via this script yet)" "WARN"
        exit 0
    }
    $selected = Select-Agents -AllAgents $state -Filter $Agents
    foreach ($e in $selected) {
        $proc = Get-Process -Id $e.pid -ErrorAction SilentlyContinue
        $alive = $null -ne $proc
        $line = "{0,-10} pid={1,-8} alive={2}" -f $e.name, $e.pid, $alive
        Write-Host $line
    }
    exit 0
}

# =========================== -Stop ===========================
if ($Stop) {
    $state = Load-State
    if ($state.Count -eq 0) {
        Write-Log "no launcher state found at '$StateFile' - nothing to stop" "WARN"
        exit 0
    }
    $selected = Select-Agents -AllAgents $state -Filter $Agents
    foreach ($e in $selected) {
        try {
            Stop-Process -Id $e.pid -Force -ErrorAction Stop
            Write-Log "stopped $($e.name) (pid $($e.pid))" "INFO"
        } catch {
            Write-Log "could not stop $($e.name) (pid $($e.pid)): $($_.Exception.Message)" "WARN"
        }
    }
    $stoppedNames = @($selected | ForEach-Object { $_.name })
    $keep = @($state | Where-Object { $stoppedNames -notcontains $_.name })
    Save-State -Entries $keep
    exit 0
}

# =========================== Launch / DryRun ===========================
$agentEntries = Read-Manifest -Path $Manifest
$selected = Select-Agents -AllAgents $agentEntries -Filter $Agents
if ($selected.Count -eq 0) {
    throw "No agents matched. Manifest has: $((($agentEntries | ForEach-Object { $_.name }) -join ', ')). Filter was: $($Agents -join ', ')"
}

if (-not $DryRun -and (-not $SuperviseScript -or -not (Test-Path $SuperviseScript))) {
    throw "A real launch needs -SuperviseScript pointing at your per-agent watcher (or `$env:FAMILYCLAW_SUPERVISE_SCRIPT). Got: '$SuperviseScript'. Use -DryRun to validate the manifest without one."
}

Write-Log "manifest   : $Manifest" "INFO"
Write-Log "agents     : $((($selected | ForEach-Object { $_.name }) -join ', '))" "INFO"
Write-Log "mode       : $(if ($DryRun) { 'DRY RUN (no processes started)' } else { 'LAUNCH' })" "INFO"

$plan = @()
foreach ($a in $selected) {
    $name = Get-PropOrDefault -Obj $a -Name "name" -Default $null
    if (-not $name) { throw "Manifest entry missing 'name'." }
    $envFileRaw = Get-PropOrDefault -Obj $a -Name "envFile" -Default $null
    if (-not $envFileRaw) { throw "Agent '$name' is missing 'envFile' in the manifest." }
    $envFile = Resolve-EnvPath -Raw $envFileRaw
    $healthCheck = [bool](Get-PropOrDefault -Obj $a -Name "healthCheck" -Default $true)
    $enabled = [bool](Get-PropOrDefault -Obj $a -Name "enabled" -Default $true)

    if (-not $enabled) {
        Write-Log "$name : disabled in manifest - skipping" "INFO"
        continue
    }

    $envExists = Test-Path $envFile
    $envFileStatus = "MISSING"
    if ($envExists) { $envFileStatus = "OK" }
    Write-Log "$name : envFile='$envFile' [$envFileStatus] healthCheck=$healthCheck" "INFO"

    if (-not $envExists -and -not $DryRun) {
        throw "Agent '$name': env file not found at '$envFile'. Fix the manifest or create the file (see .env.example). Refusing to launch a partial family - the whole point of this script is to avoid silently-missing members."
    }

    $plan += [pscustomobject]@{
        name        = $name
        envFile     = $envFile
        healthCheck = $healthCheck
    }
}

if ($DryRun) {
    Write-Log "dry run complete - $($plan.Count) agent(s) validated, 0 processes started" "INFO"
    exit 0
}

$launched = @()
foreach ($p in $plan) {
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$SuperviseScript`"", "-Agent", $p.name, "-EnvFile", "`"$($p.envFile)`"")
    if (-not $p.healthCheck) { $argList += "-NoHealthCheck" }
    if ($GatewayExe) { $argList += @("-GatewayExe", "`"$GatewayExe`"") }

    Write-Log "launching supervisor for $($p.name): powershell $($argList -join ' ')" "INFO"
    $proc = Start-Process -FilePath "powershell.exe" -ArgumentList $argList `
        -WorkingDirectory $RepoRoot -WindowStyle Hidden -PassThru

    $launched += [pscustomobject]@{
        name      = $p.name
        pid       = $proc.Id
        startedAt = (Get-Date).ToString("o")
    }
    Write-Log "$($p.name) supervisor started, pid $($proc.Id)" "INFO"
}

# Merge with any pre-existing state for agents not in this run (so a partial
# `-Agents agent_alpha` launch does not forget agents launched in an earlier run).
$existing = Load-State
$launchedNames = @($launched | ForEach-Object { $_.name })
$keepExisting = @($existing | Where-Object { $launchedNames -notcontains $_.name })
Save-State -Entries (@($keepExisting) + @($launched))

Write-Log "launched $($launched.Count) agent supervisor(s). State: $StateFile" "INFO"
