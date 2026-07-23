#!/usr/bin/env bash
# Verify Docker volume survives SIGKILL + restart (single-tenant appliance).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

: "${FAMILYCLAW_GATEWAY_TOKEN:?set FAMILYCLAW_GATEWAY_TOKEN before running}"

docker compose up -d --build
sleep 5
curl -fsS "http://127.0.0.1:8787/healthz" >/dev/null

CID="$(docker compose ps -q gateway)"
docker kill -s KILL "$CID"
sleep 2
docker compose up -d
sleep 5
curl -fsS "http://127.0.0.1:8787/healthz" >/dev/null
curl -fsS "http://127.0.0.1:8787/readyz" >/dev/null

echo "VERIFIED $(date -u +%Y-%m-%dT%H:%M:%SZ) docker kill/restart with volume + token"
