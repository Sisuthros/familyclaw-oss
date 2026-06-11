# Load a private Layer B `.env` into the current PowerShell session.
#
# Usage:
#   . .\scripts\load-env.ps1 -Path $env:USERPROFILE\.config\familyclaw\familyclaw.env
#   $env:FAMILYCLAW_ENV_FILE = "..." ; . .\scripts\load-env.ps1

param(
    [string]$Path = $env:FAMILYCLAW_ENV_FILE
)

if (-not $Path) {
    Write-Error @"
No env file path. Copy repo .env.example to a private location, then:
  . .\scripts\load-env.ps1 -Path `$env:USERPROFILE\.config\familyclaw\familyclaw.env
Or set FAMILYCLAW_ENV_FILE.
"@
    exit 1
}

if (-not (Test-Path $Path)) {
    Write-Error "Env file not found: $Path"
    exit 1
}

Get-Content $Path | ForEach-Object {
    $line = $_.Trim()
    if ($line -eq "" -or $line.StartsWith("#")) { return }
    if ($line -match '^([^=]+)=(.*)$') {
        $name = $matches[1].Trim()
        $value = $matches[2].Trim()
        Set-Item -Path "env:$name" -Value $value
    }
}

Write-Host "Loaded env from $Path"
