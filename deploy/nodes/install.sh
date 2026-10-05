#!/usr/bin/env bash
set -euo pipefail
# Run only on a trusted Linux machine; this installs the node's system service.
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
LEO_MASTER=${1:-__LEO_MASTER_ORIGIN__}
# Keep the chosen origin visible in the command and validate it before fetching
# or executing the root-owned supervisor. Never follow manager redirects.
LEO_MASTER=$(python3 - "$LEO_MASTER" <<'PY'
import sys
import urllib.parse

value = sys.argv[1]
parsed = urllib.parse.urlsplit(value)
loopback = parsed.hostname in ('localhost', '127.0.0.1', '::1')
if not parsed.hostname or not (parsed.scheme == 'https' or parsed.scheme == 'http' and loopback) or parsed.username or parsed.password or parsed.path not in ('', '/') or parsed.query or parsed.fragment:
    sys.exit('Use an HTTPS manager origin (HTTP is allowed only on loopback for local tests).')
print(value.rstrip('/'))
PY
)
[[ $(id -u) == 0 ]] || { echo 'Run this installer with sudo.' >&2; exit 1; }
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || { echo 'Linux x86-64 is required.' >&2; exit 1; }
for command in docker python3 curl systemctl; do
  command -v "$command" >/dev/null || { echo "Install $command before installing the node." >&2; exit 1; }
done
[[ -c /dev/kvm && -c /dev/net/tun ]] || { echo 'KVM and /dev/net/tun must be enabled.' >&2; exit 1; }
[[ ! -f /var/lib/leo-node/data/node/identity.json ]] || { echo 'Node already installed; use systemctl restart leo-node to restart it.' >&2; exit 1; }
install -d -m 0700 /var/lib/leo-node /var/lib/leo-node/data /var/lib/leo-node/state
LEO_FREE_KB=$(df -Pk /var/lib/leo-node | awk 'NR==2 {print $4}')
[[ "$LEO_FREE_KB" -ge 16777216 ]] || { echo 'At least 16 GiB free is required for the runtime and recovery staging.' >&2; exit 1; }
docker info >/dev/null
ensure_compressed_swap
install -d -m 0755 /opt/leo-node
LEO_INSTALL_TEMP=$(mktemp /opt/leo-node/install.XXXXXX)
trap 'rm -f "$LEO_INSTALL_TEMP"' EXIT
curl --fail --silent --show-error --proto '=https,http' --max-time 30 "${LEO_MASTER%/}/internal/nodes/host.py" > "$LEO_INSTALL_TEMP"
python3 -m py_compile "$LEO_INSTALL_TEMP"
install -m 0755 "$LEO_INSTALL_TEMP" /opt/leo-node/host.py
python3 /opt/leo-node/host.py install "$LEO_MASTER"
cat > /etc/systemd/system/leo-node.service <<'UNIT'
[Unit]
Description=Leo trusted execution node
Requires=docker.service
After=network-online.target docker.service
Wants=network-online.target
[Service]
Type=simple
ExecStart=/usr/bin/python3 /opt/leo-node/host.py run
Restart=on-failure
RestartSec=10
TimeoutStopSec=300
KillMode=mixed
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now leo-node.service
echo 'Node installed. Open Leo → Nodes to choose which agents may use it and to adjust its resource ceilings.'
