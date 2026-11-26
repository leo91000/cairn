#!/usr/bin/env bash
set -euo pipefail

docker run --detach --name cairn-beacon-smoke-db \
  --network host \
  --env POSTGRES_USER=cairn --env POSTGRES_PASSWORD=test-only \
  --env POSTGRES_DB=cairn_beacon_test \
  --health-cmd "pg_isready -h 127.0.0.1 -U cairn -d cairn_beacon_test" \
  --health-interval 1s --health-timeout 5s --health-retries 30 \
  postgres:17-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24 postgres -c listen_addresses=127.0.0.1

for _ in $(seq 1 30); do
  if test "$(docker inspect --format '{{.State.Health.Status}}' cairn-beacon-smoke-db)" = healthy; then
    break
  fi
  sleep 1
done
test "$(docker inspect --format '{{.State.Health.Status}}' cairn-beacon-smoke-db)" = healthy
