"""Exercise the public installer entry point; no VM boot is needed."""
import os
import json
import http.server
import threading
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]


class Installer(unittest.TestCase):
    def test_non_root_explains_sudo_before_writing_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = (REPO / 'deploy/installations/install.sh').read_text().replace('__LEO_OFFICIAL_ORIGIN__', "'https://leo.example.test'")
            result = subprocess.run(['bash', '-s'], input=script, text=True, capture_output=True,
                                    env={**os.environ, 'LEO_INSTALLATION_ROOT': str(root / 'installation')})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('sudo', result.stderr)
            self.assertFalse((root / 'installation').exists())


    def test_configuration_hands_claim_code_to_manager_and_preserves_identity_on_rerun(self):
        class Official(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps({'image': 'ghcr.io/leo91000/leo-agent-manager@sha256:' + '1' * 64}).encode())

            def log_message(self, *_):
                pass

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / 'bin'
            binaries.mkdir()
            docker = binaries / 'docker'
            docker.write_text("""#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['LEO_INSTALLATION_ROOT'])
args = sys.argv[1:]
if 'up' in args:
    identity = root / 'data/installation-relay/identity.json'
    identity.parent.mkdir(parents=True, exist_ok=True)
    if not identity.exists():
        claim = dict(line.split('=', 1) for line in (root / 'claim.env').read_text().splitlines())
        assert claim['LEO_INSTALLATION_CLAIM_CODE'] == 'a' * 64
        identity.write_text(json.dumps({'installationId': 'fixture-installation', 'token': 'fixture-only'}))
if 'exec' in args:
    sys.exit(0)
""")
            docker.chmod(0o755)
            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Official)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                env = {**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                       'LEO_INSTALLATION_ROOT': str(root / 'installation')}
                command = ['python3', str(REPO / 'deploy/installations/host.py'),
                           f'http://127.0.0.1:{server.server_port}', '--claim-code', 'a' * 64]
                result = subprocess.run(command, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                installation = root / 'installation'
                config = json.loads((installation / 'data/storage-s3.json').read_text())
                self.assertEqual(config['endpoint'], 'http://garage:3900')
                self.assertEqual(config['bucket'], 'leo-disks')
                self.assertEqual((installation / 'data/storage-s3.json').stat().st_mode & 0o777, 0o600)
                compose = json.loads((installation / 'compose.json').read_text())
                self.assertFalse(any('ports' in service for service in compose['services'].values()))
                self.assertFalse((installation / 'claim.env').exists())
                identity = (installation / 'data/installation-relay/identity.json').read_bytes()
                credentials = (installation / 'garage.env').read_bytes()
                result = subprocess.run(command, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((installation / 'data/installation-relay/identity.json').read_bytes(), identity)
                self.assertEqual((installation / 'garage.env').read_bytes(), credentials)
                self.assertNotIn('a' * 64, result.stdout + result.stderr)
            finally:
                server.shutdown()
                server.server_close()


if __name__ == '__main__':
    unittest.main()
