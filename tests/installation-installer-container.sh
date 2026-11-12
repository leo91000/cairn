#!/usr/bin/env bash
set -euo pipefail
# Real Beacon and manager; the Docker adapter never boots a VM.
cd "$(dirname "$0")/.."
fixture=$(mktemp -d)
name="cairn-installer-test-$$"
cleanup() {
  docker rm -f "$name-postgres" >/dev/null 2>&1 || true
  docker network rm "$name" >/dev/null 2>&1 || true
  docker run --rm -v "$fixture:/fixture" cairn-installer-fixture sh -c 'rm -rf /fixture/*' >/dev/null 2>&1 || true
  rm -rf "$fixture"
}
trap cleanup EXIT
docker build -t cairn-installer-fixture -f tests/Dockerfile.installation-installer .
docker network create "$name" >/dev/null
docker run -d --name "$name-postgres" --network "$name" --network-alias postgres \
  -e POSTGRES_USER=cairn -e POSTGRES_PASSWORD=test-only -e POSTGRES_DB=cairn_beacon_test \
  postgres:17-alpine >/dev/null
args=(--rm --device /dev/ublk-control --network "$name" -v "$PWD:/repo:ro" -v "$fixture:/fixture" \
  -v "$(node -p process.execPath):/usr/local/bin/node:ro" \
  -e CAIRN_BEACON_TEST_DATABASE_URL=postgres://cairn:test-only@postgres/cairn_beacon_test)
docker run "${args[@]}" cairn-installer-fixture python scripts/tests/installation_updates_test.py
docker run "${args[@]}" cairn-installer-fixture python tests/installation_installer_container.py prepare
# Parse the actual generated Compose with the controller's installed plugin.
# The parser runs as root to read private fixture files; it receives no Docker socket.
compose_plugin=$(docker info --format '{{range .ClientInfo.Plugins}}{{if eq .Name "compose"}}{{.Path}}{{end}}{{end}}')
docker run "${args[@]}" \
  -v "$compose_plugin:/usr/local/bin/docker-compose:ro" \
  cairn-installer-fixture python tests/installation_installer_container.py compose
docker run "${args[@]}" cairn-installer-fixture python tests/installation_installer_container.py verify
