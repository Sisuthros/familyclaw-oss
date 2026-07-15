# Time Machine demo — self-contained, in-process, no external services.
#
# Runs the flagship "buggy policy, forked fix, dry-run proof" story:
#   1. Builds an original run with a policy bug (approves 2x instead of
#      capping the amount).
#   2. Prints its timeline (what the agent did, and why).
#   3. Forks before the buggy step, replays the prefix, and runs the FIXED
#      policy under a dry-run capture — nothing real is ever sent.
#   4. Prints the timeline diff, the captured dry-run intent, and confirms
#      the original journal is untouched.
#
# Usage: powershell -File scripts/demo-time-machine.ps1 [-Dir <path>]

param(
    [string]$Dir
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

if ($Dir) {
    cargo run -p familyclaw-agent --bin familyclaw -- replay demo --dir $Dir
} else {
    cargo run -p familyclaw-agent --bin familyclaw -- replay demo
}

if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
