"""Installation updates retain and retry the candidate without data migration or rollback."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

from supervisor_fixture import write_supervisors

OLD = 'ghcr.io/leo91000/cairn@sha256:' + '1' * 64
NEW = 'ghcr.io/leo91000/cairn@sha256:' + '2' * 64


class Updates(unittest.TestCase):
    def test_candidate_is_retained_and_retry_preserves_data_and_operator_settings(self):
        for failure in ('', 'ublk', 'unapproved', 'digest', 'lease', 'stop', 'up', 'health', 'unresponsive'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)

                class Beacon(http.server.BaseHTTPRequestHandler):
                    def do_GET(self):
                        self.send_response(200)
                        self.end_headers()
                        image = NEW if failure != 'unapproved' else 'ghcr.io/leo91000/cairn:latest'
                        self.wfile.write(json.dumps({'image': image}).encode())

                    def log_message(self, *_):
                        pass

                server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Beacon)
                threading.Thread(target=server.serve_forever, daemon=True).start()
                origin = f'http://127.0.0.1:{server.server_port}'
                (root / 'installation.json').write_text(json.dumps({'image': OLD, 'origin': origin}))
                (root / 'data').mkdir()
                (root / 'data/kept').write_text('retained conversation')
                (root / 'data/maintenance-token').write_text('fixture-private-token')
                (root / '.env').write_text('')
                (root / 'compose.json').write_text(json.dumps({'services': {
                    'manager': {'image': OLD, 'environment': {'CAIRN_NODE_IMAGE': OLD}, 'mem_limit': '6g'},
                    'runner': {'image': OLD, 'mem_limit': '18g', 'environment': {'OPERATOR_SETTING': '${KEEP_ME}'}},
                    'garage': {'image': 'retained-storage'},
                }}))
                binaries = root / 'bin'
                binaries.mkdir()
                docker = binaries / 'docker'
                docker.write_text(r'''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['CAIRN_INSTALLATION_ROOT'])
args = sys.argv[1:]
failure = os.environ['FAILURE']
with (root / 'events').open('a') as events:
    events.write(json.dumps(args) + '\n')
if 'inspect' in args:
    print(json.dumps([{'RepoDigests': [] if failure == 'digest' else [args[-1]], 'Config': {'Env': ['APP_RUNTIME_ID=fixture']}}]))
if failure == 'stop' and 'stop' in args or failure == 'up' and 'up' in args:
    sys.exit(1)
if 'ps' in args:
    image = json.loads((root / 'compose.json').read_text())['services']['manager']['image']
    if failure == 'foreign-manager':
        image = 'foreign-image'
    print(json.dumps({'Image': image, 'State': 'running', 'Health': 'unhealthy'}))
if 'exec' in args:
    launches = sum('up' in json.loads(event) for event in (root / 'events').read_text().splitlines())
    unavailable = failure == 'candidate-unavailable' and launches < 2
    if 'deployment-lease' in args[-1] and (failure in ('lease', 'foreign-lease', 'foreign-manager') or unavailable):
        sys.exit(1)
    if failure == 'unresponsive' and 'deployment-lease' not in args[-1]:
        sys.exit(1)
    healthy = failure != 'health' and (failure != 'candidate-unavailable' or launches >= 2)
    print(json.dumps({'healthy': healthy}))
''')
                docker.chmod(0o755)
                installer, _ = write_supervisors(root)

                def update(fault):
                    return subprocess.run(['python3', str(installer), origin, '--update'],
                                          env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                                               'CAIRN_INSTALLATION_ROOT': directory, 'FAILURE': fault},
                                          capture_output=True, text=True, timeout=30)

                try:
                    result = update(failure)
                    self.assertEqual(result.returncode, int(bool(failure)), result.stderr)
                    config = json.loads((root / 'installation.json').read_text())
                    self.assertEqual(config['image'], OLD if failure else NEW)
                    if failure in ('lease', 'stop', 'up', 'health', 'unresponsive'):
                        self.assertEqual(config['pendingImage'], NEW)
                    events = [json.loads(line) for line in (root / 'events').read_text().splitlines()] if (root / 'events').exists() else []
                    starts = [event for event in events if 'up' in event]
                    self.assertLessEqual(len(starts), 1, 'A failed candidate must never launch a rollback')
                    if failure in ('ublk', 'unapproved', 'digest', 'lease'):
                        self.assertFalse(any('stop' in event for event in events))
                    if failure not in ('ublk', 'unapproved'):
                        if failure in ('health', 'unresponsive'):
                            for refused in ('foreign-lease', 'foreign-manager'):
                                rejected = update(refused)
                                self.assertNotEqual(rejected.returncode, 0)
                                after = [json.loads(line) for line in (root / 'events').read_text().splitlines()]
                                self.assertEqual(sum('up' in event for event in after), len(starts))
                            retry = update('candidate-unavailable')
                        else:
                            retry = update('')
                        self.assertEqual(retry.returncode, 0, retry.stderr)
                        config = json.loads((root / 'installation.json').read_text())
                        self.assertEqual(config['image'], NEW)
                        self.assertNotIn('pendingImage', config)
                        compose = json.loads((root / 'compose.json').read_text())
                        self.assertEqual(compose['services']['manager']['image'], NEW)
                        self.assertEqual(compose['services']['runner']['image'], NEW)
                        self.assertEqual(compose['services']['manager']['mem_limit'], '6g')
                        self.assertEqual(compose['services']['runner']['mem_limit'], '18g')
                        self.assertEqual(compose['services']['runner']['environment']['OPERATOR_SETTING'], '${KEEP_ME}')
                        self.assertEqual(compose['services']['garage']['image'], 'retained-storage')
                    self.assertEqual((root / 'data/kept').read_text(), 'retained conversation')
                    self.assertNotIn('fixture-private-token', result.stdout + result.stderr)
                finally:
                    server.shutdown()
                    server.server_close()


if __name__ == '__main__':
    unittest.main()
