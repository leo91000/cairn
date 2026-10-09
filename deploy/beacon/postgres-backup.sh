#!/usr/bin/env bash
# Operator-only: stop the beacon process before backup or restore.
# Use the actual Coolify container ID/name, not a checkout's Compose project.
set -euo pipefail
umask 077

if [[ $# != 3 || ( $1 != backup && $1 != restore ) ]]; then
  echo 'Usage: postgres-backup.sh backup|restore POSTGRES_CONTAINER PRIVATE_DUMP_PATH' >&2
  exit 1
fi

action=$1
postgres_container=$2
dump_path=$3

if [[ $action == backup ]]; then
  if [[ -e $dump_path ]]; then
    echo 'Backup already exists; choose a new private filename for this release' >&2
    exit 1
  fi

  # Only publish the completed dump. Failure removes the partial private file.
  temporary_dump=$(mktemp "${dump_path}.partial.XXXXXX")
  trap 'rm -f -- "$temporary_dump"' EXIT
  if ! docker exec "$postgres_container" pg_dump -U cairn -d cairn_beacon --format=custom > "$temporary_dump" 2>/dev/null; then
    echo 'Postgres backup failed; no completed dump written' >&2
    exit 1
  fi
  # A hard link publishes atomically without clobbering another recovery point.
  if ! ln -- "$temporary_dump" "$dump_path" 2>/dev/null; then
    echo 'Could not publish backup; check the private directory and choose an unused filename' >&2
    exit 1
  fi
else
  # Restore only into an empty database with the beacon process stopped.
  # Abort on the first SQL error: an existing schema is never silently merged.
  if ! docker exec -i "$postgres_container" pg_restore -U cairn -d cairn_beacon --exit-on-error --no-owner --no-privileges < "$dump_path" 2>/dev/null; then
    echo 'Postgres restore failed; keep beacon stopped and recreate the empty recovery database before retrying' >&2
    exit 1
  fi
fi
