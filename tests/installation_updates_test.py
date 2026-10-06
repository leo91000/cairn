"""Supervisor CLI: approved digest, replacement, rollback and interrupted recovery."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

REPO = Path(__file__).resolve().parents[1]
OLD = 'ghcr.io/leo91000/leo-agent-manager@sha256:' + '1' * 64
NEW = 'ghcr.io/leo91000/leo-agent-manager@sha256:' + '2' * 64


class Updates(unittest.TestCase):
    def test_approved_update_and_failed_health_restore_previous_image_without_losing_data(self):
        for failure in ('', 'health', 'digest', 'unapproved', 'interrupted', 'interrupted-stopped', 'lease', 'expired-lease'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                class Official(http.server.BaseHTTPRequestHandler):
                    def do_GET(self):
                        self.send_response(200)
                        self.end_headers()
                        self.wfile.write(json.dumps({'image': NEW if failure != 'unapproved' else 'ghcr.io/leo91000/leo-agent-manager:latest'}).encode())

                    def log_message(self, *_):
                        pass

                server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Official)
                threading.Thread(target=server.serve_forever, daemon=True).start()
                origin = f'http://127.0.0.1:{server.server_port}'
                config = {'image': OLD, 'origin': origin}
                if failure in ('interrupted', 'interrupted-stopped', 'expired-lease'):
                    config.update(pendingImage=NEW, leaseOwner='00000000-0000-4000-8000-000000000001')
                if failure in ('interrupted-stopped', 'expired-lease'):
                    config['leaseAcquired'] = True
                (root / 'installation.json').write_text(json.dumps(config))
                (root / 'data').mkdir()
                (root / 'data/kept').write_text('conversation and storage must survive')
                (root / 'data/maintenance-token').write_text('fixture-private-token')
                (root / '.env').write_text('')
                (root / 'compose.json').write_text(json.dumps({
                    'name': 'leo-installation',
                    'services': {
                        'manager': {'image': OLD, 'environment': {'LEO_NODE_IMAGE': OLD}, 'mem_limit': '6g'},
                        'runner': {'image': OLD, 'mem_limit': '18g'},
                        'garage': {'image': 'retained-storage'},
                    },
                }))
                binaries = root / 'bin'
                binaries.mkdir()
                docker = binaries / 'docker'
                docker.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['LEO_INSTALLATION_ROOT'])
args = sys.argv[1:]
with (root / 'events').open('a') as events:
    events.write(json.dumps(args) + '\\n')
if 'ps' in args:
    print(json.dumps({'State': 'exited' if os.environ['FAILURE'] == 'interrupted-stopped' else 'running'}))
if 'inspect' in args:
    if os.environ['FAILURE'] == 'digest':
        print(json.dumps([{'Id': 'sha256:bad', 'RepoDigests': [], 'Config': {'Env': ['APP_RUNTIME_ID=fixture']}}]))
    else:
        print(json.dumps([{'Id': 'sha256:fixture', 'RepoDigests': [args[-1]], 'Config': {'Env': ['APP_RUNTIME_ID=fixture']}}]))
if 'exec' in args:
    if 'deployment-lease' in args[-1] and os.environ['FAILURE'] in ('lease', 'expired-lease'):
        sys.exit(1)
    if 'health' in args[-1] and os.environ['FAILURE'] == 'health':
        image = json.loads((root / 'compose.json').read_text())['services']['manager']['image']
        if image.endswith('2' * 64):
            sys.exit(1)
    print(json.dumps({'paused': True, 'activeRuns': 0}))
''')
                docker.chmod(0o755)
                try:
                    result = subprocess.run(['python3', str(REPO / 'deploy/installations/host.py'), origin, '--update'],
                                            env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                                                 'LEO_INSTALLATION_ROOT': directory, 'FAILURE': failure},
                                            capture_output=True, text=True, timeout=90)
                    if failure in ('lease', 'expired-lease'):
                        result = subprocess.run(['python3', str(REPO / 'deploy/installations/host.py'), origin, '--update'],
                                                env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                                                     'LEO_INSTALLATION_ROOT': directory, 'FAILURE': failure},
                                                capture_output=True, text=True, timeout=90)
                    self.assertEqual(result.returncode, 0 if failure in ('', 'health', 'interrupted', 'interrupted-stopped') else 1, result.stderr)
                    updated = json.loads((root / 'installation.json').read_text())
                    self.assertEqual(updated['image'], NEW if not failure else OLD)
                    self.assertEqual((root / 'data/kept').read_text(), 'conversation and storage must survive')
                    self.assertNotIn('fixture-private-token', result.stdout + result.stderr)
                    events = [json.loads(line) for line in (root / 'events').read_text().splitlines()] if (root / 'events').exists() else []
                    if failure in ('digest', 'unapproved', 'lease', 'expired-lease'):
                        self.assertFalse(any('stop' in event or 'up' in event for event in events))
                    else:
                        self.assertTrue(any('deployment-lease' in event[-1] for event in events))
                        self.assertTrue(any('stop' in event for event in events))
                        compose = json.loads((root / 'compose.json').read_text())
                        self.assertEqual(compose['services']['manager']['image'], updated['image'])
                        self.assertEqual(compose['services']['runner']['image'], updated['image'])
                        self.assertEqual(compose['services']['manager']['mem_limit'], '6g')
                        self.assertEqual(compose['services']['runner']['mem_limit'], '18g')
                        self.assertEqual(compose['services']['garage']['image'], 'retained-storage')
                        self.assertNotIn('pendingImage', updated)
                finally:
                    server.shutdown()
                    server.server_close()


if __name__ == '__main__':
    unittest.main()
