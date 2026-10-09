#!/usr/bin/env bash
set -euo pipefail
# Run only on a trusted Linux machine; this installs the node's system service.
# >>> compressed swap
# Guest RAM shares one cgroup limit. Without swap, a peak above it stalls every
# VM instead of slowing them down; zram compresses those pages in memory.
ensure_compressed_swap() {
  local swaps=${CAIRN_PROC_SWAPS:-/proc/swaps} etc=${CAIRN_ETC_DIR:-/etc}
  if grep -q '^/dev/zram' "$swaps" 2>/dev/null; then
    return 0
  fi
  if ! command -v apt-get >/dev/null; then
    echo 'Cairn installer: enable zram compressed swap so memory peaks slow VMs down instead of stalling them.' >&2
    return 0
  fi
  if ! DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zram-tools >/dev/null; then
    apt-get update -qq >/dev/null || true
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zram-tools >/dev/null || {
      echo 'Cairn installer: could not install zram-tools; continuing without compressed swap.' >&2
      return 0
    }
  fi
  printf 'ALGO=zstd\nPERCENT=25\nPRIORITY=100\n' > "$etc/default/zramswap"
  echo 'vm.swappiness=100' > "$etc/sysctl.d/99-cairn-zram.conf"
  sysctl -q -p "$etc/sysctl.d/99-cairn-zram.conf" || true
  systemctl enable zramswap >/dev/null 2>&1 || true
  systemctl restart zramswap || echo 'Cairn installer: zramswap did not start; continuing without compressed swap.' >&2
}
# <<< compressed swap
CAIRN_MASTER=${1:-__CAIRN_MASTER_ORIGIN__}
# Keep the chosen origin visible in the command and validate it before fetching
# or executing the root-owned supervisor. Never follow manager redirects.
CAIRN_MASTER=$(python3 - "$CAIRN_MASTER" <<'PY'
import sys
import urllib.parse

value = sys.argv[1]
parsed = urllib.parse.urlsplit(value)

loopback = parsed.hostname in ('localhost', '127.0.0.1', '::1')
secure_transport = parsed.scheme == 'https' or (parsed.scheme == 'http' and loopback)
origin_only = (
    not parsed.username
    and not parsed.password
    and parsed.path in ('', '/')
    and not parsed.query
    and not parsed.fragment
)

if not parsed.hostname or not secure_transport or not origin_only:
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
[[ ! -f /var/lib/cairn-node/data/node/identity.json ]] || { echo 'Node already installed; use systemctl restart cairn-node to restart it.' >&2; exit 1; }
install -d -m 0700 /var/lib/cairn-node /var/lib/cairn-node/data /var/lib/cairn-node/state
CAIRN_FREE_KB=$(df -Pk /var/lib/cairn-node | awk 'NR==2 {print $4}')
[[ "$CAIRN_FREE_KB" -ge 16777216 ]] || { echo 'At least 16 GiB free is required for the runtime and recovery staging.' >&2; exit 1; }
docker info >/dev/null
ensure_compressed_swap
install -d -m 0755 /opt/cairn-node
CAIRN_INSTALL_TEMP=$(mktemp /opt/cairn-node/install.XXXXXX)
trap 'rm -f "$CAIRN_INSTALL_TEMP"' EXIT
curl --fail --silent --show-error --proto '=https,http' --max-time 30 "${CAIRN_MASTER%/}/internal/nodes/host.py" > "$CAIRN_INSTALL_TEMP"
python3 -m py_compile "$CAIRN_INSTALL_TEMP"
install -m 0755 "$CAIRN_INSTALL_TEMP" /opt/cairn-node/host.py
python3 /opt/cairn-node/host.py install "$CAIRN_MASTER"
cat > /etc/systemd/system/cairn-node.service <<'UNIT'
[Unit]
Description=Cairn trusted execution node
Requires=docker.service
After=network-online.target docker.service
Wants=network-online.target
[Service]
Type=simple
ExecStart=/usr/bin/python3 /opt/cairn-node/host.py run
Restart=on-failure
RestartSec=10
TimeoutStopSec=300
KillMode=mixed
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now cairn-node.service
echo 'Node installed. Open Cairn → Nodes to choose which agents may use it and to adjust its resource ceilings.'
