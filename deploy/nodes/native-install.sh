#!/usr/bin/env bash
set -euo pipefail
# Explicit native enrollment; no drivers, desktop services or global packages change.
fail() { echo "Cairn native node installer: $*" >&2; exit 1; }
[[ $(id -u) == 0 ]] || fail 'Run this installer with sudo.'
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || fail 'Linux x86-64 is required.'
for command in python3 curl systemctl useradd; do
  command -v "$command" >/dev/null || fail "Install $command before enrolling this native node."
done
for command in mkfs.ext4 e2fsck resize2fs; do
  command -v "$command" >/dev/null || fail 'Install e2fsprogs before enrolling this native node.'
done
CAIRN_NATIVE_ORIGIN=${1:?Pass the direct manager HTTPS origin.}
CAIRN_NATIVE_ORIGIN=$(python3 - "$CAIRN_NATIVE_ORIGIN" <<'PY'
import sys
import urllib.parse
value = urllib.parse.urlsplit(sys.argv[1])
if (value.scheme != 'https' and not (value.scheme == 'http' and value.hostname in ('localhost', '127.0.0.1', '::1'))) or not value.hostname or value.username or value.password or value.path not in ('', '/') or value.query or value.fragment:
    sys.exit('Use a manager HTTPS origin without a path, query or credentials.')
print(sys.argv[1].rstrip('/'))
PY
)
install -d -m 0755 /opt/cairn-native
CAIRN_NATIVE_TEMP=$(mktemp /opt/cairn-native/install.XXXXXX)
trap 'rm -f "$CAIRN_NATIVE_TEMP"' EXIT
curl --fail --silent --show-error --proto '=https,http' --max-time 30 "$CAIRN_NATIVE_ORIGIN/internal/nodes/native-host.py" > "$CAIRN_NATIVE_TEMP" \
  || fail "Cannot download the native supervisor from $CAIRN_NATIVE_ORIGIN."
python3 -m py_compile "$CAIRN_NATIVE_TEMP"
install -m 0755 "$CAIRN_NATIVE_TEMP" /opt/cairn-native/host.py
# A rerun refreshes the supervisor without re-enrolling. Restarting it would stop
# native work, so it only starts a stopped service.
if [[ -e /var/lib/cairn-native/data/node/identity.json ]]; then
  systemctl enable --now cairn-native-node.service
  echo 'Native node already enrolled; its supervisor was refreshed. Restart cairn-native-node when it is idle to load it: sudo systemctl restart cairn-native-node'
  exit 0
fi
python3 /opt/cairn-native/host.py install "$CAIRN_NATIVE_ORIGIN"
