#!/usr/bin/env bash
set -euo pipefail
# Run only on a trusted Linux machine; this installs the node's system service.
LEO_MASTER=__LEO_MASTER_ORIGIN__
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
echo 'Node installed. Configure its resource ceilings and agent access in Leo → Nodes.'
