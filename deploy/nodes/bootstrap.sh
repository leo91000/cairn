#!/usr/bin/env bash
# Adds an execution node without a manager address, certificate or incoming
# port. Beacon introduces the node to its manager; they connect directly when
# the networks allow it, and through Beacon otherwise, always over encryption
# that ends on the manager certificate pinned below. Run only on a trusted
# Linux machine.
main() {
  set -euo pipefail
  CAIRN_BEACON_ORIGIN=__CAIRN_BEACON_ORIGIN__
  CAIRN_GATEWAY_SHA256=__CAIRN_GATEWAY_SHA256__
  CAIRN_GATEWAY=http://127.0.0.1:4319
  fail() { echo "Cairn node installer: $*" >&2; exit 1; }

  [[ $# == 3 ]] || fail 'Copy the complete command from Nodes → Add a machine.'
  CAIRN_INSTALLATION_ID=$1
  CAIRN_MANAGER_PIN=$3
  case $2 in
    firecracker) CAIRN_NODE_INSTALLER=install.sh ;;
    native) CAIRN_NODE_INSTALLER=native-install.sh ;;
    *) fail 'Copy the complete command from Nodes → Add a machine.' ;;
  esac
  [[ $(id -u) == 0 ]] || fail 'Run the command with sudo bash.'
  [[ $(uname -s) == Linux ]] || fail 'Linux is required.'
  [[ $(uname -m) == x86_64 ]] || fail 'An x86-64 machine is required.'
  for command in curl sha256sum systemctl; do
    command -v "$command" >/dev/null || fail "Install $command before adding this machine."
  done

  install -d -m 0755 /opt/cairn-gateway
  CAIRN_GATEWAY_TEMP=$(mktemp /opt/cairn-gateway/gateway.XXXXXX)
  CAIRN_INSTALLER_TEMP=$(mktemp /opt/cairn-gateway/installer.XXXXXX)
  trap 'rm -f "$CAIRN_GATEWAY_TEMP" "$CAIRN_INSTALLER_TEMP"' EXIT
  curl --fail --silent --show-error --proto '=https,http' --max-time 120 "$CAIRN_BEACON_ORIGIN/install/node-gateway" > "$CAIRN_GATEWAY_TEMP" \
    || fail 'Cannot download the node gateway. Check outbound HTTPS connectivity.'
  echo "$CAIRN_GATEWAY_SHA256  $CAIRN_GATEWAY_TEMP" | sha256sum --check --status \
    || fail 'Node gateway checksum mismatch. Copy a fresh command from Cairn and retry.'
  chmod 0755 "$CAIRN_GATEWAY_TEMP"
  "$CAIRN_GATEWAY_TEMP" --version >/dev/null 2>&1 \
    || fail 'The node gateway does not run on this machine. Copy a fresh command from Cairn and retry.'

  CAIRN_GATEWAY_CHANGED=false
  if ! cmp -s "$CAIRN_GATEWAY_TEMP" /opt/cairn-gateway/cairn-node-gateway 2>/dev/null; then
    install -m 0755 "$CAIRN_GATEWAY_TEMP" /opt/cairn-gateway/cairn-node-gateway
    CAIRN_GATEWAY_CHANGED=true
  fi
  CAIRN_GATEWAY_CONFIGURED=$(/opt/cairn-gateway/cairn-node-gateway configure "$CAIRN_BEACON_ORIGIN" "$CAIRN_INSTALLATION_ID" "$CAIRN_MANAGER_PIN") || exit 1
  [[ $CAIRN_GATEWAY_CONFIGURED == unchanged ]] || CAIRN_GATEWAY_CHANGED=true
  cat > /etc/systemd/system/cairn-node-gateway.service.new <<'UNIT'
[Unit]
Description=Cairn node gateway to the manager
After=network-online.target
Wants=network-online.target
[Service]
Type=simple
ExecStart=/opt/cairn-gateway/cairn-node-gateway run
User=root
UMask=0077
Restart=always
RestartSec=5
[Install]
WantedBy=multi-user.target
UNIT
  if ! cmp -s /etc/systemd/system/cairn-node-gateway.service.new /etc/systemd/system/cairn-node-gateway.service 2>/dev/null; then
    mv /etc/systemd/system/cairn-node-gateway.service.new /etc/systemd/system/cairn-node-gateway.service
    CAIRN_GATEWAY_CHANGED=true
  else
    rm -f /etc/systemd/system/cairn-node-gateway.service.new
  fi
  systemctl daemon-reload
  systemctl enable cairn-node-gateway.service >/dev/null 2>&1
  # Restarting interrupts node transfers, which retry; skip it when nothing changed.
  if [[ $CAIRN_GATEWAY_CHANGED == true ]]; then
    systemctl restart cairn-node-gateway.service
  else
    systemctl start cairn-node-gateway.service
  fi

  # The installer download doubles as the end-to-end check to the manager.
  CAIRN_STATUS=000
  for _ in $(seq 1 30); do
    CAIRN_STATUS=$(curl --silent --max-time 15 --output "$CAIRN_INSTALLER_TEMP" --write-out '%{http_code}' "$CAIRN_GATEWAY/internal/nodes/$CAIRN_NODE_INSTALLER" || true)
    [[ $CAIRN_STATUS == 200 ]] && break
    [[ $CAIRN_STATUS == 000 ]] || [[ $CAIRN_STATUS == 502 ]] || [[ $CAIRN_STATUS == 503 ]] || break
    sleep 1
  done
  if [[ $CAIRN_STATUS != 200 ]]; then
    CAIRN_REASON=$(python3 -c 'import json,sys
try: print(json.load(open(sys.argv[1])).get("error", ""))
except Exception: print("")' "$CAIRN_INSTALLER_TEMP")
    [[ $CAIRN_STATUS != 000 ]] || CAIRN_REASON='The node gateway did not start. Inspect: journalctl -u cairn-node-gateway'
    fail "Cannot reach the installation (HTTP $CAIRN_STATUS). ${CAIRN_REASON:-Check that the installation is online and up to date.}"
  fi
  bash "$CAIRN_INSTALLER_TEMP" "$CAIRN_GATEWAY"
}

main "$@"
