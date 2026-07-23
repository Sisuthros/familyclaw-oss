#!/usr/bin/env pwsh
# Run competitor crash matrices and regenerate bench-competitors/MATRIX.md (Windows).
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
if (-not $Root) { $Root = (Resolve-Path "$PSScriptRoot\..").Path }
$Py = if ($env:PYTHON) { $env:PYTHON } else { "python" }
$CrashPoints = @("clean", "before_write", "mid_replay")

function Invoke-Cycle([string]$Dir, [string]$CrashPoint) {
  $Workdir = "$Root\bench-competitors\$Dir\_runs\$CrashPoint"
  & $Py "$Root\bench-competitors\$Dir\crash_harness.py" cycle `
    --crash-point $CrashPoint --workdir $Workdir
}

function Invoke-CompetitorMatrix([string]$Dir) {
  Write-Host "=== $Dir ==="
  foreach ($cp in $CrashPoints) {
    Invoke-Cycle -Dir $Dir -CrashPoint $cp
  }
}

function Invoke-LangGraphMatrix() {
  $Venv = $null
  if (Test-Path "$Root\bench-competitors\langgraph\.venv\Scripts\python.exe") {
    $Venv = "$Root\bench-competitors\langgraph\.venv\Scripts\python.exe"
  } elseif (Test-Path "$Root\bench-competitors\langgraph\.venv\bin\python") {
    $Venv = "$Root\bench-competitors\langgraph\.venv\bin\python"
  }
  if (-not $Venv) {
    Write-Host "=== langgraph ==="
    Write-Host "(skip langgraph - no .venv; see bench-competitors/langgraph/README.md)"
    return
  }

  Write-Host "=== langgraph ==="
  foreach ($cp in $CrashPoints) {
    $Workdir = "$Root\bench-competitors\langgraph\_runs\$cp"
    $Output = & $Venv "$Root\bench-competitors\langgraph\crash_harness.py" cycle `
      --crash-point $cp --workdir $Workdir
    $Output | ForEach-Object { Write-Host $_ }
    $Output | Set-Content -Encoding UTF8 "$Workdir\cycle_stdout.txt"
    @'
import json
import sys
from pathlib import Path

stdout_path = Path(sys.argv[1])
report_path = Path(sys.argv[2])
text = stdout_path.read_text(encoding="utf-8")
marker = "CYCLE_REPORT "
start = text.find(marker)
if start < 0:
    raise SystemExit(f"missing {marker!r} in {stdout_path}")
payload = text[start + len(marker):].strip()
report = json.loads(payload)
report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
'@ | & $Py - "$Workdir\cycle_stdout.txt" "$Workdir\cycle_report.json"
  }
}

function Get-FamilyClawCell() {
  if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    return "run separately"
  }

  Write-Host "=== familyclaw ==="
  & cargo build -p familyclaw-agent --bin continuity_daemon
  & cargo run -p familyclaw-bench --bin bench -- s1
  $Scorecard = "$Root\crates\familyclaw-bench\out\scorecard.json"
  if (-not (Test-Path $Scorecard)) {
    throw "missing scorecard.json after cargo run"
  }

  $Overcount = @'
import json
import sys
from pathlib import Path

card = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
for scenario in card.get("scenarios", []):
    if scenario.get("id") == "s1_crash_matrix":
        metrics = scenario.get("metrics", {})
        value = metrics.get("side_effect_overcount")
        if value is None:
            raise SystemExit("missing side_effect_overcount in scorecard")
        print(int(value))
        raise SystemExit(0)
raise SystemExit("missing s1_crash_matrix in scorecard")
'@ | & $Py - $Scorecard

  return ('{0} (`familyclaw`)' -f $Overcount)
}

Invoke-CompetitorMatrix openclaw
Invoke-CompetitorMatrix hermes
Invoke-LangGraphMatrix
$FamilyClawCell = Get-FamilyClawCell
& $Py "$Root\bench-competitors\matrix_summary.py" `
  --root $Root --familyclaw-cell $FamilyClawCell
Write-Host "Wrote $Root\bench-competitors\MATRIX.md"
