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
fail() { echo "Cairn node installer: $*" >&2; exit 1; }
[[ $(id -u) == 0 ]] || fail 'Run this installer with sudo.'
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || fail 'Linux x86-64 is required.'
for command in docker python3 curl systemctl; do
  command -v "$command" >/dev/null || fail "Install $command before installing the node."
done
[[ -c /dev/kvm ]] || fail 'KVM is missing. Enable hardware virtualization in the firmware and load the kvm module.'
[[ -c /dev/net/tun ]] || fail 'TUN is missing. Load the tun module (sudo modprobe tun).'
docker info >/dev/null 2>&1 || fail 'Docker is unavailable. Install Docker Engine and start its daemon.'
install -d -m 0755 /opt/cairn-node
CAIRN_INSTALL_TEMP=$(mktemp /opt/cairn-node/install.XXXXXX)
trap 'rm -f "$CAIRN_INSTALL_TEMP"' EXIT
download_supervisor() {
  curl --fail --silent --show-error --proto '=https,http' --max-time 30 "${CAIRN_MASTER%/}/internal/nodes/host.py" > "$CAIRN_INSTALL_TEMP" \
    || fail "Cannot download the node supervisor from $CAIRN_MASTER."
  python3 -m py_compile "$CAIRN_INSTALL_TEMP"
  install -m 0755 "$CAIRN_INSTALL_TEMP" /opt/cairn-node/host.py
}
write_unit() {
  cat > /etc/systemd/system/cairn-node.service <<'UNIT'
[Unit]
Description=Cairn trusted execution node
Requires=docker.service
After=network-online.target docker.service cairn-node-gateway.service
Wants=network-online.target cairn-node-gateway.service
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
}
# A rerun refreshes the supervisor without re-enrolling. Restarting it would stop
# running VMs, so it only starts a stopped service.
if [[ -f /var/lib/cairn-node/data/node/identity.json ]]; then
  download_supervisor
  write_unit
  systemctl enable --now cairn-node.service
  echo 'Node already enrolled; its supervisor was refreshed. Restart cairn-node when it is idle to load it: sudo systemctl restart cairn-node'
  exit 0
fi
install -d -m 0700 /var/lib/cairn-node /var/lib/cairn-node/data /var/lib/cairn-node/state
CAIRN_FREE_KB=$(df -Pk /var/lib/cairn-node | awk 'NR==2 {print $4}')
[[ "$CAIRN_FREE_KB" -ge 16777216 ]] || fail 'At least 16 GiB free disk space is required for the runtime and recovery staging.'
ensure_compressed_swap
download_supervisor
python3 /opt/cairn-node/host.py install "$CAIRN_MASTER"
write_unit
systemctl enable --now cairn-node.service
echo 'Node installed. Open Cairn → Nodes to choose which agents may use it and to adjust its resource ceilings.'
