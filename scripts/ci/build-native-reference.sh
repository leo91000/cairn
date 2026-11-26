#!/usr/bin/env bash
set -euo pipefail

sudo apt-get install -y --no-install-recommends libclang-dev
git fetch --no-tags origin "$REFERENCE_SHA"
git worktree add --detach /tmp/cairn-native-reference "$REFERENCE_SHA"
CARGO_TARGET_DIR="$PWD/reference-target" cargo +1.97.1 build --locked --manifest-path /tmp/cairn-native-reference/Cargo.toml --bin cairn --example native_storage_qualifier
mkdir -p reference-native/examples
cp reference-target/debug/cairn reference-native/cairn
cp reference-target/debug/examples/native_storage_qualifier reference-native/examples/native_storage_qualifier

python3 - <<'PYREF'
import hashlib, json, os
from pathlib import Path
binaries = ['cairn', 'examples/native_storage_qualifier']
checksums = {name: hashlib.sha256((Path('reference-native') / name).read_bytes()).hexdigest() for name in binaries}
Path('reference-native/revision.json').write_text(json.dumps(dict(
    headSha=os.environ['REFERENCE_SHA'],
    profile='development, debug=0, incremental=0',
    features='default (ublk required)',
    rust='1.97.1',
    image=os.environ['ImageVersion'],
    buildRun=os.environ['GITHUB_RUN_ID'],
    sha256=checksums,
), indent=2))
PYREF
