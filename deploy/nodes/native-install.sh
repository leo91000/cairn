#!/usr/bin/env bash
set -euo pipefail
# Explicit native enrollment; no drivers, desktop services or global packages change.
[[ $(id -u) == 0 ]] || { echo 'Run this installer with sudo.' >&2; exit 1; }
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || { echo 'Linux x86-64 is required.' >&2; exit 1; }
for command in python3 curl systemctl useradd; do
  command -v "$command" >/dev/null || { echo "Install $command before enrolling this native node." >&2; exit 1; }
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
[[ ! -e /var/lib/cairn-native/data/node/identity.json ]] || { echo 'Native node is already installed.' >&2; exit 1; }
install -d -m 0755 /opt/cairn-native
CAIRN_NATIVE_TEMP=$(mktemp /opt/cairn-native/install.XXXXXX)
trap 'rm -f "$CAIRN_NATIVE_TEMP"' EXIT
curl --fail --silent --show-error --proto '=https,http' --max-time 30 "$CAIRN_NATIVE_ORIGIN/internal/nodes/native-host.py" > "$CAIRN_NATIVE_TEMP"
python3 -m py_compile "$CAIRN_NATIVE_TEMP"
install -m 0755 "$CAIRN_NATIVE_TEMP" /opt/cairn-native/host.py
python3 /opt/cairn-native/host.py install "$CAIRN_NATIVE_ORIGIN"
