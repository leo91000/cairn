#!/usr/bin/env bash
set -euo pipefail
# Real Beacon, manager and Garage; the Docker adapter never boots a VM.
cd "$(dirname "$0")/.."
fixture=$(mktemp -d)
name="cairn-installer-test-$$"
cleanup() {
  docker rm -f "$name-garage" "$name-postgres" >/dev/null 2>&1 || true
  docker network rm "$name" >/dev/null 2>&1 || true
  docker run --rm -v "$fixture:/fixture" cairn-installer-fixture sh -c 'rm -rf /fixture/*' >/dev/null 2>&1 || true
  rm -rf "$fixture"
}
trap cleanup EXIT
# Cargo identifies the current executable even when a restored cache contains
# integration-test binaries from several feature sets or older builds.
storage_test=$(node scripts/test-backend.mjs -p cairn-beacon \
  --test storage_continuity --no-run --message-format=json | python3 -c '
import json, sys
artifacts = [json.loads(line) for line in sys.stdin]
executables = [artifact["executable"] for artifact in artifacts
               if artifact.get("reason") == "compiler-artifact"
               and artifact["target"]["name"] == "storage_continuity"
               and artifact["target"]["kind"] == ["test"]
               and artifact.get("executable")]
assert len(executables) == 1, "Cargo must identify the storage continuity executable"
print(executables[0])
')
docker build -t cairn-installer-fixture -f tests/Dockerfile.installation-installer .
docker network create "$name" >/dev/null
docker run -d --name "$name-postgres" --network "$name" --network-alias postgres \
  -e POSTGRES_USER=cairn -e POSTGRES_PASSWORD=test-only -e POSTGRES_DB=cairn_beacon_test \
  postgres:17-alpine >/dev/null
args=(--rm --network "$name" -v "$PWD:/repo:ro" -v "$fixture:/fixture" \
  -v "$storage_test:/fixture/storage-continuity:ro" \
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
docker run "${args[@]}" cairn-installer-fixture cat /fixture/installation/garage.env | \
  docker run -d --name "$name-garage" --network "$name" --network-alias garage \
  --env-file /proc/self/fd/0 \
  -v "$fixture/installation/garage.toml:/etc/garage.toml:ro" \
  -v "$fixture/installation/garage:/var/lib/garage" \
  dxflrs/garage@sha256:866bd13ed2038ba7e7190e840482bc27234c4afaf77be8cfa439ae088c1e4690 \
  /garage server --single-node --default-bucket >/dev/null
ready=false
for attempt in {1..60}; do
  if docker exec "$name-garage" /garage bucket info cairn-disks >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 1
done
[[ "$ready" == true ]] || { echo 'Fixture Garage did not become ready' >&2; exit 1; }
docker run "${args[@]}" cairn-installer-fixture python tests/installation_installer_container.py verify
