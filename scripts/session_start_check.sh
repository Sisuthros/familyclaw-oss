#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$ROOT" ]; then
  echo "ERROR: not inside a git repository"
  exit 1
fi

cd "$ROOT"
echo "Repository root: $ROOT"

if [ ! -f "Cargo.toml" ]; then
  echo "ERROR: Cargo.toml not found at repository root"
  exit 1
fi

echo "OK: running from FamilyClaw repo root"
