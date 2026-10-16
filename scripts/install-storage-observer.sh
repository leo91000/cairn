#!/usr/bin/env bash
set -euo pipefail

[[ $(id -u) == 0 ]] || { echo 'Run with sudo from a trusted Cairn checkout.' >&2; exit 1; }
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../deploy/installations" && pwd)
for command in python3 docker systemctl systemd-analyze; do
  command -v "$command" >/dev/null
done

# This optional operator installation changes only telemetry. It does not stop
# application containers, change their approval or open a listening socket.
install -d -m 0700 /var/lib/cairn-installation/observations/storage
temporary_file=''
trap '[[ -z "$temporary_file" ]] || rm -f -- "$temporary_file"' EXIT
atomic_install() {
  local source=$1 destination=$2 mode=$3
  temporary_file=$(mktemp "${destination}.XXXXXX")
  install -m "$mode" "$source" "$temporary_file"
  mv -f -- "$temporary_file" "$destination"
  temporary_file=''
}
atomic_install "$source_dir/storage-observer.py" /var/lib/cairn-installation/storage-observer.py 0700
atomic_install "$source_dir/storage-observer.service" /etc/systemd/system/cairn-storage-observer.service 0644
atomic_install "$source_dir/storage-observer.timer" /etc/systemd/system/cairn-storage-observer.timer 0644
systemd-analyze verify /etc/systemd/system/cairn-storage-observer.service /etc/systemd/system/cairn-storage-observer.timer
systemctl daemon-reload
systemctl start cairn-storage-observer.service
systemctl enable --now cairn-storage-observer.timer
