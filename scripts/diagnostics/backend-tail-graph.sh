#!/usr/bin/env bash
set -euo pipefail

# This observer compiles both graphs. It runs no Rust test and grants no test credit.
case "$GRAPH_SCOPE" in
  package|workspace) ;;
  *) exit 64 ;;
esac
expected=fbd0b96e7450948e5f8b7829bdff157733291413
test "$(git rev-parse HEAD)" = "$expected"
test -z "$(git status --porcelain)"
mkdir backend-tail-evidence
# Evidence is task-owned and excluded only from the observer's dirty-state check.
printf '\n/backend-tail-evidence/\n' >> .git/info/exclude

python3 - <<'PY'
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess

def git(*args):
    return subprocess.check_output(['git', *args], text=True).strip()

assert os.environ.get('CARGO_PROFILE_DEV_DEBUG') == '0'
assert os.environ.get('CARGO_INCREMENTAL') == '0'
assert not os.environ.get('RUSTFLAGS')
assert not os.environ.get('CARGO_ENCODED_RUSTFLAGS')
value = {
    'qualification': False,
    'scope': os.environ['GRAPH_SCOPE'],
    'source': git('rev-parse', 'HEAD'),
    'tree': git('rev-parse', 'HEAD^{tree}'),
    'dirty': bool(git('status', '--porcelain')),
    'lockSha256': hashlib.sha256(Path('Cargo.lock').read_bytes()).hexdigest(),
    'debug': '0', 'incremental': '0', 'rustflagsEmpty': True,
    'machine': platform.machine(), 'kernel': platform.release(),
    'cpus': os.cpu_count(),
    'imageOS': os.environ.get('ImageOS'),
    'imageVersion': os.environ.get('ImageVersion'),
    'observer': os.environ['GITHUB_SHA'],
}
Path('backend-tail-evidence/source-before.json').write_text(json.dumps(value) + '\n')
PY
rustc -vV > backend-tail-evidence/rustc.txt
cargo --version > backend-tail-evidence/cargo.txt
cargo nextest --version > backend-tail-evidence/nextest.txt
grep -E '^cargo-nextest 0\.9\.149( |$)' backend-tail-evidence/nextest.txt
cargo nextest run --help > backend-tail-evidence/nextest-help.txt
grep -q -- '--no-run' backend-tail-evidence/nextest-help.txt

date -u +'%FT%TZ workspace-start' | tee backend-tail-evidence/times.txt
node scripts/test-backend.mjs --nextest --no-run --test-threads 8 --no-fail-fast --color never --status-level all 2>&1 | tee backend-tail-evidence/workspace-build.log
date -u +'%FT%TZ workspace-finished' | tee -a backend-tail-evidence/times.txt
python3 "$RUNNER_TEMP/backend-tail-observer/backend-tail-fingerprints.py" target/debug/.fingerprint backend-tail-evidence/after-workspace.json

date -u +'%FT%TZ tail-start' | tee -a backend-tail-evidence/times.txt
if [ "$GRAPH_SCOPE" = package ]; then
  node scripts/test-backend.mjs -p cairn-installation --lib --no-run --message-format=json 2>&1 | tee backend-tail-evidence/tail-build.log
else
  node scripts/test-backend.mjs --workspace --lib --no-run --message-format=json 2>&1 | tee backend-tail-evidence/tail-build.log
fi
date -u +'%FT%TZ tail-finished' | tee -a backend-tail-evidence/times.txt
python3 "$RUNNER_TEMP/backend-tail-observer/backend-tail-fingerprints.py" target/debug/.fingerprint backend-tail-evidence/after-tail.json

test "$(git rev-parse HEAD)" = "$expected"
test -z "$(git status --porcelain)"
printf '{"source":"%s","dirty":false,"qualification":false}\n' "$expected" > backend-tail-evidence/source-after.json
