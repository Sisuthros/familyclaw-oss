#!/usr/bin/env bash
# Docker deployment smoke test (release-checklist Docker proof).
# Run once the Docker daemon is up (Docker Desktop running):
#   bash scripts/docker-smoke.sh
#
# Proves: image builds, container runs, /healthz answers 200.
set -euo pipefail

IMAGE="familyclaw-gateway:local"

echo "═══ 1/3  docker build ═══"
docker build -t "$IMAGE" .

echo "═══ 2/3  docker run (detached) ═══"
CID=$(docker run -d --rm -p 8787:8787 "$IMAGE")
echo "  container: $CID"
# Give the gateway a moment to bind.
sleep 4

echo "═══ 3/3  curl /healthz ═══"
ok=0
for i in $(seq 1 10); do
  if curl -fsS http://127.0.0.1:8787/healthz >/dev/null 2>&1; then ok=1; break; fi
  sleep 2
done
if [ "$ok" -eq 1 ]; then
  echo "  ✅ /healthz responded: $(curl -fsS http://127.0.0.1:8787/healthz)"
  echo "  ✅ /readyz: $(curl -fsS http://127.0.0.1:8787/readyz 2>&1 || echo '(check)')"
else
  echo "  ❌ /healthz did not respond"
fi

echo "═══ cleanup ═══"
docker stop "$CID" >/dev/null 2>&1 || true
[ "$ok" -eq 1 ] && echo "  ✅ DOCKER SMOKE PASSED" || { echo "  ❌ DOCKER SMOKE FAILED"; exit 1; }
