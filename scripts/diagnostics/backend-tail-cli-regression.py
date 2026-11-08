"""Exercise the real pinned nextest parser through the real backend launcher.

Fixture builds are synthetic; Cargo metadata is blocked before any Cargo runs.
This proves argument compatibility only, never build or runtime qualification.
"""

import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


ROOT = Path(os.environ["CAIRN_BACKEND_SOURCE"]).resolve()
PINNED = Path(os.environ["CAIRN_NEXTEST_CLI"]).resolve()
OBSERVER = Path(__file__).with_name("backend-tail-graph.sh")
BINARY_SHA256 = "7f557542ea0d9d7218c2d564987a2cebf367de1d0f9eca45f93f270932a3d9ff"
GUARD = "BACKEND_TAIL_REAL_CLI_REACHED_METADATA_GUARD"


def command(source):
    matches = [line for line in source.splitlines()
               if line.strip().startswith("node scripts/test-backend.mjs --nextest ")]
    if len(matches) != 1:
        raise ValueError("Expected exactly one full workspace command")
    return shlex.split(matches[0].split("2>&1", 1)[0].strip())[2:]


def invoke(arguments):
    with tempfile.TemporaryDirectory(prefix="backend-tail-real-cli-") as temporary:
        directory = Path(temporary)
        cargo = directory / "cargo"
        calls = directory / "calls.jsonl"
        cargo.write_text("""#!/usr/bin/env python3
import json, os
from pathlib import Path
import sys
args = sys.argv[1:]
root = Path(os.environ['CLI_FIXTURE_ROOT'])
with (root / 'calls.jsonl').open('a') as out:
    out.write(json.dumps(args) + '\\n')
if args[0] == 'build':
    name = 'worker_fixture' if '--example' in args else 'cairn'
    executable = root / name
    executable.write_bytes(b'cairn-worker-fixture-hooks-v1')
    print(json.dumps({'reason': 'compiler-artifact', 'target': {'name': name}, 'executable': str(executable)}))
elif args[:2] == ['nextest', 'run']:
    binary = os.environ['CLI_PINNED_BINARY']
    os.execv(binary, [binary, *args])
elif args[0] == 'metadata' or args[:2] == ['--color=never', 'metadata']:
    print('BACKEND_TAIL_REAL_CLI_REACHED_METADATA_GUARD', file=sys.stderr)
    sys.exit(97)
else:
    print('BACKEND_TAIL_FORBIDDEN_CARGO_INVOCATION', file=sys.stderr)
    sys.exit(98)
""")
        cargo.chmod(0o755)
        rustc = directory / 'rustc'
        rustc.write_text("""#!/usr/bin/env python3
import sys
if sys.argv[1:] != ['-vV']:
    sys.exit(98)
print('rustc 1.97.1\\nbinary: rustc\\ncommit-hash: unknown\\ncommit-date: unknown\\nhost: x86_64-unknown-linux-gnu\\nrelease: 1.97.1\\nLLVM version: 22.0.0')
""")
        rustc.chmod(0o755)
        environment = dict(os.environ, PATH=str(directory) + os.pathsep + os.environ['PATH'],
                           CARGO=str(cargo), CLI_FIXTURE_ROOT=str(directory),
                           CLI_PINNED_BINARY=str(PINNED), RUSTC=str(rustc))
        result = subprocess.run(["node", str(ROOT / "scripts/test-backend.mjs"), *arguments],
                                cwd=directory, env=environment, capture_output=True,
                                text=True, timeout=15)
        recorded = [json.loads(line) for line in calls.read_text().splitlines()]
        return result, recorded


class RealPinnedCliTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if hashlib.sha256(PINNED.read_bytes()).hexdigest() != BINARY_SHA256:
            raise ValueError("Pinned official Linux x64 executable digest mismatch")
        version = subprocess.check_output([str(PINNED), "nextest", "--version"], text=True, timeout=5)
        if not version.startswith("cargo-nextest 0.9.149 "):
            raise ValueError("Pinned nextest version mismatch")

    def assert_accepted(self, arguments):
        result, recorded = invoke(arguments)
        self.assertIn(GUARD, result.stderr)
        self.assertNotEqual(result.returncode, 2, "clap rejected generated arguments")
        nextest_calls = [args for args in recorded if args[:2] == ['nextest', 'run']]
        self.assertEqual(nextest_calls, [['nextest', 'run', '--locked', '--workspace', *arguments[1:]]])
        self.assertIn('metadata', recorded[-1][:2])
        self.assertTrue(all(args[0] in ['build', 'nextest', 'metadata']
                            or args[:2] == ['--color=never', 'metadata'] for args in recorded))
        self.assertEqual(len([args for args in recorded if args[0] == 'build']), 2)

    def test_original_probe_conflict_is_reproduced_by_real_cli(self):
        arguments = ['--nextest', '--no-run', '--test-threads', '8', '--no-fail-fast',
                     '--color', 'never', '--status-level', 'all']
        result, recorded = invoke(arguments)
        self.assertEqual(result.returncode, 2)
        self.assertIn("'--no-run' cannot be used with '--no-fail-fast'", result.stderr)
        self.assertFalse(any('metadata' in args[:2] for args in recorded))

    def test_observer_generated_flags_are_accepted_by_real_cli(self):
        self.assert_accepted(command(OBSERVER.read_text()))

    def test_normal_test_flags_remain_accepted_and_keep_no_fail_fast(self):
        workflow = subprocess.check_output(
            ['git', '-C', str(ROOT), 'show', 'fbd0b96e7450948e5f8b7829bdff157733291413:.github/workflows/ci.yaml'],
            text=True, timeout=5)
        arguments = command(workflow)
        self.assertIn('--no-fail-fast', arguments)
        self.assertNotIn('--no-run', arguments)
        self.assert_accepted(arguments)


if __name__ == '__main__':
    unittest.main(verbosity=2)
