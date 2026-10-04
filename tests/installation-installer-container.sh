#!/usr/bin/env bash
set -euo pipefail
# Real official service, manager and Garage; the Docker adapter never boots a VM.
cd "$(dirname "$0")/.."
fixture=$(mktemp -d)
name="leo-installer-test-$$"
cleanup() {
  docker rm -f "$name-garage" "$name-postgres" >/dev/null 2>&1 || true
  docker network rm "$name" >/dev/null 2>&1 || true
  docker run --rm -v "$fixture:/fixture" leo-installer-fixture sh -c 'rm -rf /fixture/*' >/dev/null 2>&1 || true
  rm -rf "$fixture"
}
trap cleanup EXIT
docker build -t leo-installer-fixture -f tests/Dockerfile.installation-installer .
docker network create "$name" >/dev/null
docker run -d --name "$name-postgres" --network "$name" --network-alias postgres \
  -e POSTGRES_USER=leo -e POSTGRES_PASSWORD=test-only -e POSTGRES_DB=leo_official_test \
  postgres:17-alpine >/dev/null
args=(--rm --network "$name" -v "$PWD:/repo:ro" -v "$fixture:/fixture" \
  -e LEO_OFFICIAL_TEST_DATABASE_URL=postgres://leo:test-only@postgres/leo_official_test)
docker run "${args[@]}" leo-installer-fixture python tests/installation_installer_container.py prepare
docker run "${args[@]}" leo-installer-fixture cat /fixture/installation/garage.env | \
  docker run -d --name "$name-garage" --network "$name" --network-alias garage \
  --env-file /proc/self/fd/0 \
  -v "$fixture/installation/garage.toml:/etc/garage.toml:ro" \
  -v "$fixture/installation/garage:/var/lib/garage" \
  dxflrs/garage@sha256:866bd13ed2038ba7e7190e840482bc27234c4afaf77be8cfa439ae088c1e4690 \
  /garage server --single-node --default-bucket >/dev/null
ready=false
for attempt in {1..60}; do
  if docker exec "$name-garage" /garage bucket info leo-disks >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 1
done
[[ "$ready" == true ]] || { echo 'Fixture Garage did not become ready' >&2; exit 1; }
docker run "${args[@]}" leo-installer-fixture python tests/installation_installer_container.py verify
