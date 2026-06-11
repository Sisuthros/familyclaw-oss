# Load Layer B `.env` into the current PowerShell session.
# Usage: . .\scripts\load-env.ps1
#        . .\scripts\load-env.ps1 -Path E:\familyclaw-profiles\.env

param(
    [string]$Path = "E:\familyclaw-profiles\.env"
)

if (-not (Test-Path $Path)) {
    Write-Error "Env file not found: $Path (copy from .env.example first)"
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
