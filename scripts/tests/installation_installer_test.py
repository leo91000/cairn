"""Exercise the public installer entry point; no VM boot is needed."""
import os
import json
import http.server
import importlib.util
import threading
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from supervisor_fixture import write_supervisors

REPO = Path(__file__).resolve().parents[2]


class Installer(unittest.TestCase):
    def test_legacy_storage_owner_repair_preserves_private_bytes_and_refuses_links(self):
        spec = importlib.util.spec_from_file_location('installation_host', REPO / 'deploy/installations/host.py')
        host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(host)
        with tempfile.TemporaryDirectory() as directory:
            host.ROOT = Path(directory)
            storage = host.ROOT / 'data/storage-s3.json'
            storage.parent.mkdir()
            storage.write_bytes(b'opaque legacy credentials')
            storage.chmod(0o600)
            with patch.object(host.os, 'getuid', return_value=0), patch.object(host.os, 'chown') as chown:
                host.repair_legacy_storage_owner()
                chown.assert_called_once_with(storage, 1000, 1000, follow_symlinks=False)
                self.assertEqual(storage.read_bytes(), b'opaque legacy credentials')
                self.assertEqual(storage.stat().st_mode & 0o777, 0o600)
                storage.unlink()
                chown.reset_mock()
                host.repair_legacy_storage_owner()
                self.assertFalse(storage.exists())
                chown.assert_not_called()
                target = host.ROOT / 'other-private-file'
                target.write_bytes(b'other private data')
                storage.symlink_to(target)
                with self.assertRaisesRegex(RuntimeError, 'regular file'):
                    host.repair_legacy_storage_owner()
                chown.assert_not_called()
                self.assertEqual(target.read_bytes(), b'other private data')

    def test_truncated_download_never_starts_the_installer(self):
        script = (REPO / 'deploy/installations/install.sh').read_text()
        partial = script[:script.index('python3 -m py_compile')]
        result = subprocess.run(['bash', '-s'], input=partial, text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('Run the command', result.stderr)
        self.assertEqual(result.stdout, '')

    def test_partial_startup_clears_claim_from_existing_manager(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / 'bin'
            binaries.mkdir()
            docker = binaries / 'docker'
            docker.write_text("""#!/usr/bin/env python3
import os, pathlib, sys
root = pathlib.Path(os.environ['CAIRN_INSTALLATION_ROOT'])
if 'up' in sys.argv:
    if '--no-deps' not in sys.argv:
        sys.exit(1)
    assert (root / 'claim.env').read_text() == ''
    (root / 'cleaned').touch()
""")
            docker.chmod(0o755)
            (root / 'installation.json').write_text(json.dumps({
                'origin': 'https://cairn.example.test',
                'image': 'ghcr.io/leo91000/cairn@sha256:' + '1' * 64,
            }))
            result = subprocess.run(
                ['python3', str(write_supervisors(Path(directory))[0]),
                 'https://cairn.example.test', '--claim-code', 'a' * 64],
                env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                     'CAIRN_INSTALLATION_ROOT': directory}, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue((root / 'cleaned').exists())
            self.assertFalse((root / 'claim.env').exists())

    def test_failed_download_does_not_leave_a_claim_code_on_disk(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / 'bin'
            binaries.mkdir()
            docker = binaries / 'docker'
            docker.write_text('#!/bin/sh\nexit 1\n')
            docker.chmod(0o755)
            installation = root / 'installation'
            installation.mkdir()
            (installation / 'installation.json').write_text(json.dumps({
                'origin': 'https://cairn.example.test',
                'image': 'ghcr.io/leo91000/cairn@sha256:' + '1' * 64,
            }))
            result = subprocess.run(
                ['python3', str(write_supervisors(Path(directory))[0]),
                 'https://cairn.example.test', '--claim-code', 'a' * 64],
                env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                     'CAIRN_INSTALLATION_ROOT': str(installation)}, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((installation / 'claim.env').exists())
            self.assertNotIn('a' * 64, result.stdout + result.stderr)

    def test_incomplete_identity_is_preserved_and_explains_recovery(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            identity = root / 'data/installation-relay/identity.json'
            identity.parent.mkdir(parents=True)
            identity.touch(mode=0o600)
            result = subprocess.run(
                ['python3', str(write_supervisors(Path(directory))[0]),
                 'https://cairn.example.test', '--claim-code', 'a' * 64],
                env={**os.environ, 'CAIRN_INSTALLATION_ROOT': directory},
                capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('incomplete', result.stderr)
            self.assertIn('Restore', result.stderr)
            self.assertEqual(identity.read_bytes(), b'')
            self.assertFalse((root / 'claim.env').exists())

    def test_invalid_claim_code_has_an_actionable_message(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                ['python3', str(write_supervisors(Path(directory))[0]),
                 'https://cairn.example.test', '--claim-code', 'invalid'],
                env={**os.environ, 'CAIRN_INSTALLATION_ROOT': directory},
                capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Copy a new command', result.stderr)
            self.assertNotIn('invalid', result.stderr)

    def test_non_root_explains_sudo_before_writing_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = (REPO / 'deploy/installations/install.sh').read_text().replace('__CAIRN_BEACON_ORIGIN__', "'https://cairn.example.test'")
            result = subprocess.run(['bash', '-s'], input=script, text=True, capture_output=True,
                                    env={**os.environ, 'CAIRN_INSTALLATION_ROOT': str(root / 'installation')})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('sudo', result.stderr)
            self.assertFalse((root / 'installation').exists())


    def test_configuration_hands_claim_code_to_manager_and_preserves_identity_on_rerun(self):
        class Beacon(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps({'image': 'ghcr.io/leo91000/cairn@sha256:' + '1' * 64}).encode())

            def log_message(self, *_):
                pass

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / 'bin'
            binaries.mkdir()
            docker = binaries / 'docker'
            docker.write_text("""#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['CAIRN_INSTALLATION_ROOT'])
args = sys.argv[1:]
if 'up' in args:
    # Docker refuses a NanoCPUs limit exceeding the fixture host's two CPUs.
    config = json.loads((root / 'compose.json').read_text())
    assert all(service.get('cpus', 2) <= 2 for service in config['services'].values())
    identity = root / 'data/installation-relay/identity.json'
    identity.parent.mkdir(parents=True, exist_ok=True)
    if not identity.exists():
        claim = dict(line.split('=', 1) for line in (root / 'claim.env').read_text().splitlines())
        assert claim['CAIRN_INSTALLATION_CLAIM_CODE'] == 'a' * 64
        identity.touch(mode=0o600)
        identity.write_text(json.dumps({'origin': os.environ['FIXTURE_BEACON_ORIGIN'], 'installationId': '00000000-0000-4000-8000-000000000001', 'token': 'fixture-only'}))
if 'exec' in args:
    sys.exit(0)
""")
            docker.chmod(0o755)
            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Beacon)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                env = {**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                       'CAIRN_INSTALLATION_ROOT': str(root / 'installation'),
                       'FIXTURE_BEACON_ORIGIN': f'http://127.0.0.1:{server.server_port}'}
                command = ['python3', str(write_supervisors(Path(directory))[0]),
                           f'http://127.0.0.1:{server.server_port}', '--claim-code', 'a' * 64]
                result = subprocess.run(command, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                installation = root / 'installation'
                self.assertFalse((installation / 'data/storage-s3.json').exists())
                self.assertFalse((installation / 'garage.env').exists())
                self.assertFalse((installation / 'garage.toml').exists())
                self.assertFalse((installation / 'garage').exists())
                compose = json.loads((installation / 'compose.json').read_text())
                self.assertEqual(set(compose['services']), {'manager', 'runner'})
                self.assertEqual(set(compose['services']['manager']['depends_on']), {'runner'})
                self.assertFalse(any('ports' in service for service in compose['services'].values()))
                self.assertFalse((installation / 'claim.env').exists())
                identity = (installation / 'data/installation-relay/identity.json').read_bytes()
                # Reinstalling an older installation retains operator-owned S3
                # configuration and data, even if they can no longer be parsed.
                legacy = {
                    'garage.env': b'legacy environment',
                    'garage.toml': b'legacy configuration',
                    'garage/data/block': b'legacy object',
                    'data/storage-s3.json': b'legacy storage configuration',
                }
                for name, contents in legacy.items():
                    path = installation / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(contents)
                garage = {
                    'image': 'legacy-storage-image', 'restart': 'unless-stopped',
                    'env_file': ['garage.env'],
                    'volumes': ['./garage.toml:/etc/garage.toml:ro', './garage:/var/lib/garage'],
                    'healthcheck': {'test': ['CMD', '/garage', 'bucket', 'info', 'cairn-disks']},
                }
                compose['services']['garage'] = garage
                dependency = {'condition': 'service_healthy'}
                compose['services']['manager']['depends_on']['garage'] = dependency
                (installation / 'compose.json').write_text(json.dumps(compose))
                result = subprocess.run(command, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((installation / 'data/installation-relay/identity.json').read_bytes(), identity)
                for name, contents in legacy.items():
                    self.assertEqual((installation / name).read_bytes(), contents)
                retained = json.loads((installation / 'compose.json').read_text())
                self.assertEqual(retained['services']['garage'], garage)
                self.assertEqual(retained['services']['manager']['depends_on']['garage'], dependency)
                self.assertEqual(retained['services']['manager']['image'], compose['services']['manager']['image'])
                self.assertNotIn('a' * 64, result.stdout + result.stderr)
            finally:
                server.shutdown()
                server.server_close()


if __name__ == '__main__':
    unittest.main()
