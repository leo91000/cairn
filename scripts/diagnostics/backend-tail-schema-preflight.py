"""CI-only, real pinned Cargo fingerprints from a tiny offline workspace."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys


if os.environ.get('GITHUB_ACTIONS') != 'true':
    raise SystemExit('SCHEMA_PREFLIGHT_REQUIRES_DISPOSABLE_CI')

root = Path('target/backend-tail-schema')
root.mkdir(parents=True, exist_ok=False)
evidence = Path('backend-tail-schema-evidence')
evidence.mkdir(exist_ok=False)
files = {
    'Cargo.toml': '[workspace]\nmembers = ["core", "consumer"]\nresolver = "3"\n',
    'core/Cargo.toml': '[package]\nname = "schema-core"\nversion = "0.1.0"\nedition = "2024"\n[features]\ndefault = []\nextra = []\n',
    'core/src/lib.rs': 'pub fn value() -> usize { 1 }\n#[test] fn unit() { assert_eq!(value(), 1); }\n',
    'core/build.rs': '''fn main() {
    println!("cargo::rerun-if-env-changed=CAIRN_COLLECTOR_SYNTHETIC");
    for index in 0..3000 {
        println!("cargo::rerun-if-changed=schema-inputs/input-{index:04}-{}.txt", "x".repeat(60));
    }
}
''',
    'core/tests/integration.rs': '#[test] fn integration() { assert_eq!(schema_core::value(), 1); }\n',
    'core/examples/example.rs': 'fn main() { assert_eq!(schema_core::value(), 1); }\n',
    'consumer/Cargo.toml': '[package]\nname = "schema-consumer"\nversion = "0.1.0"\nedition = "2024"\n[dependencies]\nschema-core = { path = "../core", features = ["extra"] }\n',
    'consumer/src/lib.rs': 'pub fn value() -> usize { schema_core::value() }\n#[test] fn unit() { assert_eq!(value(), 1); }\n',
}
for name, contents in files.items():
    location = root / name
    location.parent.mkdir(parents=True, exist_ok=True)
    location.write_text(contents)
inputs = root / 'core/schema-inputs'
inputs.mkdir()
for index in range(3000):
    (inputs / f'input-{index:04}-{"x" * 60}.txt').touch()

version = subprocess.check_output(['cargo', '--version'], text=True, timeout=5)
if not version.startswith('cargo 1.97.1 '):
    raise SystemExit('SCHEMA_PREFLIGHT_TOOLCHAIN_MISMATCH')
(evidence / 'cargo.txt').write_text(version)
subprocess.run(['cargo', 'generate-lockfile', '--offline'], cwd=root, check=True, timeout=30)
with (evidence / 'compile.jsonl').open('w') as output:
    subprocess.run(['cargo', 'test', '--locked', '--offline', '--workspace', '--all-targets',
                    '--no-run', '--message-format=json'], cwd=root, check=True, timeout=120,
                   stdout=output)

spec = importlib.util.spec_from_file_location('collector', Path(__file__).with_name('backend-tail-fingerprints.py'))
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)
fingerprints = root / 'target/debug/.fingerprint'
# Reproduce the rejected size boundary against the very same real Cargo files,
# then restore the reviewed fixed bound. Neither branch changes the file bytes.
current_limit = collector.MAX_BYTES
try:
    collector.MAX_BYTES = 65536
    try:
        collector.snapshot(fingerprints)
    except collector.ObservationFailure as error:
        legacy_receipt = error.receipt
        if error.receipt['reason'] != 'oversized_fingerprint_file' or error.receipt.get('fileBytes', 0) <= 65536:
            raise SystemExit('SCHEMA_PREFLIGHT_WRONG_LEGACY_REJECTION')
    else:
        raise SystemExit('SCHEMA_PREFLIGHT_DID_NOT_REPRODUCE_SIZE_BOUNDARY')
finally:
    collector.MAX_BYTES = current_limit
(evidence / 'legacy-64k-refusal.json').write_text(json.dumps(legacy_receipt) + '\n')
result = subprocess.run([sys.executable, str(Path(__file__).with_name('backend-tail-fingerprints.py')),
                         str(fingerprints), str(evidence / 'collector.json')], timeout=20)

# All inputs below were emitted by the real Cargo invocation above. Preserve only
# its compiler schema fields and marker metadata, excluding local paths/env data.
raw_schema = []
for directory in fingerprints.iterdir():
    for location in directory.glob('*.json'):
        data = collector.read_regular(location)
        source = json.loads(data)
        marker = collector.read_regular(location.with_suffix(''))
        raw_schema.append({
            'identitySha256': collector.hashlib.sha256(f'{directory.name}/{location.name}'.encode()).hexdigest(),
            'jsonSha256': collector.hashlib.sha256(data).hexdigest(),
            'jsonBytes': len(data),
            'kind': 'build-script' if 'build-script' in location.name else 'test' if location.name.startswith('test-') else 'library',
            'fields': {key: source[key] for key in ['rustc', 'features', 'declared_features', 'target', 'profile', 'path', 'deps', 'config', 'compile_kind']},
            'rustflagsEmpty': source.get('rustflags') == [],
            'markerBytes': len(marker),
            'markerHex16': bool(collector.MARKER.fullmatch(marker)),
        })
(evidence / 'real-cargo-schema.json').write_text(json.dumps(raw_schema) + '\n')
if not any(item['fields']['features'] == '' for item in raw_schema):
    raise SystemExit('SCHEMA_PREFLIGHT_MISSING_SYNTHETIC_BUILD_SCRIPT')
if not any(item['kind'] == 'test' for item in raw_schema):
    raise SystemExit('SCHEMA_PREFLIGHT_MISSING_TEST_PROFILE')
if not any(65536 < item['jsonBytes'] <= current_limit for item in raw_schema):
    raise SystemExit('SCHEMA_PREFLIGHT_MISSING_REAL_LARGE_FINGERPRINT')

if result.returncode != 0:
    raise SystemExit(result.returncode)
projection = json.loads((evidence / 'collector.json').read_text())
if not projection['complete'] or len(projection['fingerprints']) != len(raw_schema):
    raise SystemExit('SCHEMA_PREFLIGHT_INCOMPLETE')
print(json.dumps({'qualification': False, 'schemaQualified': True, 'realFingerprintCount': len(raw_schema),
                  'buildScriptCount': sum(item['kind'] == 'build-script' for item in raw_schema),
                  'largestFingerprintBytes': max(item['jsonBytes'] for item in raw_schema),
                  'legacy64kRejected': True, 'currentMaxBytes': current_limit}))
