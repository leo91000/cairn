#!/usr/bin/env bash
# >>> compressed swap
# Guest RAM shares one cgroup limit. Without swap, a peak above it stalls every
# VM instead of slowing them down; zram compresses those pages in memory.
ensure_compressed_swap() {
  local swaps=${LEO_PROC_SWAPS:-/proc/swaps} etc=${LEO_ETC_DIR:-/etc}
  if grep -q '^/dev/zram' "$swaps" 2>/dev/null; then
    return 0
  fi
  if ! command -v apt-get >/dev/null; then
    echo 'Leo installer: enable zram compressed swap so memory peaks slow VMs down instead of stalling them.' >&2
    return 0
  fi
  if ! DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zram-tools >/dev/null; then
    apt-get update -qq >/dev/null || true
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zram-tools >/dev/null || {
      echo 'Leo installer: could not install zram-tools; continuing without compressed swap.' >&2
      return 0
    }
  fi
  printf 'ALGO=zstd\nPERCENT=25\nPRIORITY=100\n' > "$etc/default/zramswap"
  echo 'vm.swappiness=100' > "$etc/sysctl.d/99-leo-zram.conf"
  sysctl -q -p "$etc/sysctl.d/99-leo-zram.conf" || true
  systemctl enable zramswap >/dev/null 2>&1 || true
  systemctl restart zramswap || echo 'Leo installer: zramswap did not start; continuing without compressed swap.' >&2
}
# <<< compressed swap

main() {
  set -euo pipefail
  LEO_OFFICIAL_ORIGIN=__LEO_OFFICIAL_ORIGIN__
  LEO_HOST_SHA256=__LEO_HOST_SHA256__
  LEO_NODE_HOST_SHA256=__LEO_NODE_HOST_SHA256__
  fail() { echo "Leo installer: $*" >&2; exit 1; }

  [[ $(id -u) == 0 ]] || fail 'Run the command with sudo bash.'
  [[ $(uname -s) == Linux ]] || fail 'Linux is required.'
  [[ $(uname -m) == x86_64 ]] || fail 'An x86-64 machine is required.'

  for command in docker python3 curl systemctl; do
    command -v "$command" >/dev/null || fail "Install $command before installing Leo."
  done

  [[ -c /dev/kvm ]] || fail 'KVM is missing. Enable hardware virtualization and load the kvm module.'
  [[ -c /dev/net/tun ]] || fail 'TUN is missing. Load the tun module (sudo modprobe tun).'
  [[ -c /dev/fuse ]] || fail 'FUSE is missing. Install fuse3 and load the fuse module.'

  docker info >/dev/null 2>&1 || fail 'Docker is unavailable. Start the Docker daemon.'
  docker compose version >/dev/null 2>&1 || fail 'Install the Docker Compose plugin.'
  ensure_compressed_swap

  LEO_INSTALLATION_ROOT=${LEO_INSTALLATION_ROOT:-/var/lib/leo-installation}
  LEO_DISK_PATH=$LEO_INSTALLATION_ROOT
  while [[ ! -d "$LEO_DISK_PATH" ]]; do LEO_DISK_PATH=$(dirname "$LEO_DISK_PATH"); done
  LEO_FREE_KB=$(df -Pk "$LEO_DISK_PATH" | awk 'NR==2 {print $4}')
  [[ "$LEO_FREE_KB" -ge 16777216 ]] || fail 'At least 16 GiB free disk space is required for runtime images and recovery staging.'

  install -d -m 0700 "$LEO_INSTALLATION_ROOT"
  LEO_INSTALL_TEMP=$(mktemp "$LEO_INSTALLATION_ROOT/install.XXXXXX")
  trap 'rm -f "$LEO_INSTALL_TEMP"' EXIT

  curl --fail --silent --show-error --proto '=https' --max-time 30 "${LEO_OFFICIAL_ORIGIN}/install/host.py" > "$LEO_INSTALL_TEMP" || fail 'Cannot download the installer. Check outbound HTTPS connectivity.'
  python3 - "$LEO_INSTALL_TEMP" "$LEO_HOST_SHA256" <<'PYTHON' || fail 'Installer checksum mismatch. Download a fresh command from the official app and retry.'
import hashlib
from pathlib import Path
import sys
sys.exit(0 if hashlib.sha256(Path(sys.argv[1]).read_bytes()).hexdigest() == sys.argv[2] else 1)
PYTHON
  python3 -m py_compile "$LEO_INSTALL_TEMP"
  install -m 0700 "$LEO_INSTALL_TEMP" "$LEO_INSTALLATION_ROOT/host.py"
  curl --fail --silent --show-error --proto '=https' --max-time 30 "${LEO_OFFICIAL_ORIGIN}/install/node-host.py" > "$LEO_INSTALL_TEMP" || fail 'Cannot download the shared node supervisor.'
  python3 - "$LEO_INSTALL_TEMP" "$LEO_NODE_HOST_SHA256" <<'PYTHON' || fail 'Node supervisor checksum mismatch.'
import hashlib
from pathlib import Path
import sys
sys.exit(0 if hashlib.sha256(Path(sys.argv[1]).read_bytes()).hexdigest() == sys.argv[2] else 1)
PYTHON
  python3 -m py_compile "$LEO_INSTALL_TEMP"
  install -m 0700 "$LEO_INSTALL_TEMP" "$LEO_INSTALLATION_ROOT/node-host.py"
  if [[ $# == 2 && $1 == --claim-code ]]; then
    export LEO_INSTALLATION_CLAIM_CODE=$2
  elif [[ $# != 0 ]]; then
    fail 'Copy the complete command from Add an installation.'
  fi
  python3 "$LEO_INSTALLATION_ROOT/host.py" "$LEO_OFFICIAL_ORIGIN"
  python3 - "$LEO_INSTALLATION_ROOT" "$LEO_OFFICIAL_ORIGIN" <<'PYTHON'
from pathlib import Path
import shlex
import sys
root, origin = sys.argv[1:]
# A timer survives reboots and shares the installer's nonblocking lock. The
# supervisor runs only on the host; containers never receive the Docker socket.
command = ' '.join(shlex.quote(value).replace('%', '%%') for value in ['/usr/bin/python3', str(Path(root) / 'host.py'), origin, '--update'])
Path('/etc/systemd/system/leo-installation-update.service').write_text(f'''[Unit]
Description=Leo approved installation update
Requires=docker.service
After=network-online.target docker.service
Wants=network-online.target
[Service]
Type=oneshot
Environment="LEO_INSTALLATION_ROOT={root.replace('%', '%%')}"
ExecStart={command}
TimeoutStartSec=3000
''')
Path('/etc/systemd/system/leo-installation-update.timer').write_text('''[Unit]
Description=Check the official approved Leo installation release
[Timer]
OnBootSec=2min
OnUnitActiveSec=5min
RandomizedDelaySec=30
[Install]
WantedBy=timers.target
''')
PYTHON
  systemctl daemon-reload
  systemctl enable --now leo-installation-update.timer
}

main "$@"
