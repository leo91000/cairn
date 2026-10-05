#!/usr/bin/env bash
main() {
  set -euo pipefail
  LEO_OFFICIAL_ORIGIN=__LEO_OFFICIAL_ORIGIN__
  fail() { echo "Leo installer: $*" >&2; exit 1; }
  
  [[ $(id -u) == 0 ]] || fail 'Run the command with sudo bash.'
  [[ $(uname -s) == Linux ]] || fail 'Linux is required.'
  [[ $(uname -m) == x86_64 ]] || fail 'An x86-64 machine is required.'
  
  for command in docker python3 curl; do
    command -v "$command" >/dev/null || fail "Install $command before installing Leo."
  done
  
  [[ -c /dev/kvm ]] || fail 'KVM is missing. Enable hardware virtualization and load the kvm module.'
  [[ -c /dev/net/tun ]] || fail 'TUN is missing. Load the tun module (sudo modprobe tun).'
  [[ -c /dev/fuse ]] || fail 'FUSE is missing. Install fuse3 and load the fuse module.'
  
  docker info >/dev/null 2>&1 || fail 'Docker is unavailable. Start the Docker daemon.'
  docker compose version >/dev/null 2>&1 || fail 'Install the Docker Compose plugin.'
  
  LEO_INSTALLATION_ROOT=${LEO_INSTALLATION_ROOT:-/var/lib/leo-installation}
  LEO_DISK_PATH=$LEO_INSTALLATION_ROOT
  while [[ ! -d "$LEO_DISK_PATH" ]]; do LEO_DISK_PATH=$(dirname "$LEO_DISK_PATH"); done
  LEO_FREE_KB=$(df -Pk "$LEO_DISK_PATH" | awk 'NR==2 {print $4}')
  [[ "$LEO_FREE_KB" -ge 16777216 ]] || fail 'At least 16 GiB free disk space is required for runtime images and recovery staging.'
  
  install -d -m 0700 "$LEO_INSTALLATION_ROOT"
  LEO_INSTALL_TEMP=$(mktemp "$LEO_INSTALLATION_ROOT/install.XXXXXX")
  trap 'rm -f "$LEO_INSTALL_TEMP"' EXIT
  
  curl --fail --silent --show-error --proto '=https,http' --max-time 30 "${LEO_OFFICIAL_ORIGIN}/install/host.py" > "$LEO_INSTALL_TEMP" || fail 'Cannot download the installer. Check outbound HTTPS connectivity.'
  python3 -m py_compile "$LEO_INSTALL_TEMP"
  install -m 0700 "$LEO_INSTALL_TEMP" "$LEO_INSTALLATION_ROOT/host.py"
  python3 "$LEO_INSTALLATION_ROOT/host.py" "$LEO_OFFICIAL_ORIGIN" "$@"
}

main "$@"
