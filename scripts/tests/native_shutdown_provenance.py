"""Static source identity only: no binary loading, native probes or execution."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

BASE = '12c6fc54405dac70e889dd0cade882550e759da7'


def git(*args):
    return subprocess.run(['git', *args], check=True, capture_output=True).stdout


def main():
    paths = git('ls-tree', '-r', '--name-only', BASE, '--', 'crates').decode().splitlines()
    evidence = []
    for name in paths:
        reference = git('show', BASE + ':' + name)
        candidate = Path(name).read_bytes()
        observed = candidate
        if name == 'crates/installation/src/worker/checkpoint.rs':
            text = candidate.decode()
            pattern = r'\n    /// Diagnostic-branch-only bridge.*?(?=\n    #\[tokio::test\])'
            match = re.search(pattern, text, flags=re.S)
            if match is None or match.start() <= text.index('#[cfg(test)]\nmod tests'):
                raise RuntimeError('Missing test-only checkpoint observation bridge.')
            observed = (text[:match.start()] + text[match.end():]).encode()
        if reference != observed:
            raise RuntimeError('Production source differs from the frozen reference.')
        evidence.append(dict(path=name, referenceSha256=hashlib.sha256(reference).hexdigest(),
                             reviewedSha256=hashlib.sha256(candidate).hexdigest(), productionEquivalent=True))
    current = git('ls-files', '--', 'crates').decode().splitlines()
    if set(current) != set(paths):
        raise RuntimeError('Production source set changed.')
    contracts = []
    for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                 'scripts/tests/native_chat_fixture.py', 'scripts/tests/node_s3_test.py',
                 'scripts/test-backend.mjs', 'scripts/test-environment.mjs'):
        content = Path(name).read_bytes()
        if content != git('show', BASE + ':' + name):
            raise RuntimeError('Build or fixture contract differs from the frozen reference.')
        contracts.append(dict(path=name, sha256=hashlib.sha256(content).hexdigest()))
    output = Path('native-shutdown-evidence')
    output.mkdir(exist_ok=True)
    (output / 'source-provenance.json').write_text(json.dumps(dict(base=BASE,
        reviewedHead=git('rev-parse', 'HEAD').decode().strip(), paths=evidence, unchangedContracts=contracts,
        artifactReuse=False, build='fresh exact reviewed source, CI only'), indent=2) + '\n')
    print('Frozen production source identity verified; no native execution performed.')


if __name__ == '__main__':
    main()
