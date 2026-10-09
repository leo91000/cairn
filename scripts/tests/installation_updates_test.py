"""Supervisor CLI: approved digest, replacement, rollback and interrupted recovery."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

REPO = Path(__file__).resolve().parents[2]
OLD = 'ghcr.io/leo91000/cairn@sha256:' + '1' * 64
NEW = 'ghcr.io/leo91000/cairn@sha256:' + '2' * 64


class Updates(unittest.TestCase):
    def test_approved_update_and_failed_health_restore_previous_image_without_losing_data(self):
        for failure in ('', 'health', 'runner-unhealthy', 'stop-once', 'up-once', 'exec-once', 'digest', 'unapproved', 'interrupted', 'interrupted-stopped', 'interrupted-created', 'interrupted-missing', 'interrupted-restarting', 'interrupted-crash-loop', 'interrupted-unhealthy', 'interrupted-exec-once', 'interrupted-foreign-restarting', 'lease', 'expired-lease'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                class Beacon(http.server.BaseHTTPRequestHandler):
                    def do_GET(self):
                        self.send_response(200)
                        self.end_headers()
                        self.wfile.write(json.dumps({'image': NEW if failure != 'unapproved' else 'ghcr.io/leo91000/cairn:latest'}).encode())

                    def log_message(self, *_):
                        pass

                server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Beacon)
                threading.Thread(target=server.serve_forever, daemon=True).start()
                origin = f'http://127.0.0.1:{server.server_port}'
                config = {'image': OLD, 'origin': origin}
                if failure in ('interrupted', 'interrupted-stopped', 'interrupted-created', 'interrupted-missing', 'interrupted-restarting', 'interrupted-crash-loop', 'interrupted-unhealthy', 'interrupted-exec-once', 'interrupted-foreign-restarting', 'expired-lease'):
                    config.update(pendingImage=NEW, leaseOwner='00000000-0000-4000-8000-000000000001')
                if failure in ('interrupted-stopped', 'interrupted-created', 'interrupted-missing', 'interrupted-restarting', 'interrupted-crash-loop', 'interrupted-unhealthy', 'interrupted-exec-once', 'interrupted-foreign-restarting', 'expired-lease'):
                    config['leaseAcquired'] = True
                (root / 'installation.json').write_text(json.dumps(config))
                (root / 'data').mkdir()
                (root / 'data/kept').write_text('conversation and storage must survive')
                (root / 'data/maintenance-token').write_text('fixture-private-token')
                (root / '.env').write_text('')
                initial_image = NEW if failure in ('interrupted-unhealthy', 'interrupted-exec-once', 'interrupted-created', 'interrupted-crash-loop') else OLD
                (root / 'compose.json').write_text(json.dumps({
                    'name': 'cairn-installation',
                    'services': {
                        'manager': {'image': initial_image, 'environment': {'CAIRN_NODE_IMAGE': initial_image}, 'mem_limit': '6g'},
                        'runner': {'image': initial_image, 'mem_limit': '18g'},
                        'garage': {'image': 'retained-storage'},
                    },
                }))
                binaries = root / 'bin'
                binaries.mkdir()
                docker = binaries / 'docker'
                docker.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['CAIRN_INSTALLATION_ROOT'])
args = sys.argv[1:]
with (root / 'events').open('a') as events:
    events.write(json.dumps(args) + '\\n')
if 'stop' in args and os.environ['FAILURE'] == 'stop-once' and not (root / 'stop-refused').exists():
    (root / 'stop-refused').touch()
    sys.exit(1)
if 'up' in args:
    image = json.loads((root / 'compose.json').read_text())['services']['manager']['image']
    if image.endswith('2' * 64) and os.environ['FAILURE'] in ('up-once', 'runner-unhealthy'):
        if os.environ['FAILURE'] == 'runner-unhealthy' or not (root / 'up-refused').exists():
            (root / 'up-refused').touch()
            sys.exit(1)
if 'ps' in args:
    failure = os.environ['FAILURE']
    if failure != 'interrupted-missing':
        state = 'restarting' if 'restarting' in failure else 'exited' if failure == 'interrupted-stopped' else 'created' if failure == 'interrupted-created' else 'running'
        image = 'ghcr.io/leo91000/cairn@sha256:' + ('2' if failure in ('interrupted-restarting', 'interrupted-crash-loop', 'interrupted-unhealthy', 'interrupted-exec-once', 'interrupted-created') else '1') * 64
        if failure == 'interrupted-crash-loop':
            # Between restarts, Docker briefly reports a crash-looping candidate running.
            observed = root / 'crash-loop-observed'
            state = 'running' if args[-1] == 'runner' or not observed.exists() else 'restarting'
            if args[-1] == 'manager':
                observed.touch()
        if failure in ('up-once', 'runner-unhealthy'):
            image = json.loads((root / 'compose.json').read_text())['services'][args[-1]]['image']
            state = 'created' if failure == 'runner-unhealthy' and args[-1] == 'manager' else 'running'
        health = 'unhealthy' if failure == 'runner-unhealthy' and args[-1] == 'runner' else ''
        print(json.dumps({'State': state, 'Image': image, 'Health': health}))
if 'inspect' in args:
    if os.environ['FAILURE'] == 'digest':
        print(json.dumps([{'Id': 'sha256:bad', 'RepoDigests': [], 'Config': {'Env': ['APP_RUNTIME_ID=fixture']}}]))
    else:
        print(json.dumps([{'Id': 'sha256:fixture', 'RepoDigests': [args[-1]], 'Config': {'Env': ['APP_RUNTIME_ID=fixture']}}]))
if 'exec' in args:
    manager = json.loads((root / 'compose.json').read_text())['services']['manager']['image']
    if os.environ['FAILURE'] == 'interrupted-crash-loop' and manager.endswith('2' * 64):
        sys.exit(1)
    if 'deployment-lease' in args[-1]:
        if os.environ['FAILURE'] in ('lease', 'expired-lease', 'interrupted-exec-once', 'interrupted-foreign-restarting'):
            sys.exit(1)
        if os.environ['FAILURE'] in ('interrupted-restarting', 'interrupted-unhealthy', 'interrupted-created') and '"POST"' in args[-1]:
            sys.exit(1)
    if 'health' in args[-1] and os.environ['FAILURE'] in ('exec-once', 'interrupted-exec-once') and not (root / 'exec-refused').exists():
        (root / 'exec-refused').touch()
        sys.exit(1)
    if 'health' in args[-1] and os.environ['FAILURE'] in ('health', 'interrupted-unhealthy'):
        image = json.loads((root / 'compose.json').read_text())['services']['manager']['image']
        if image.endswith('2' * 64):
            print(json.dumps({'healthy': False}))
            sys.exit(0)
    print(json.dumps({'healthy': True}))
''')
                docker.chmod(0o755)
                try:
                    result = subprocess.run(['python3', str(REPO / 'deploy/installations/host.py'), origin, '--update'],
                                            env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                                                 'CAIRN_INSTALLATION_ROOT': directory, 'FAILURE': failure},
                                            capture_output=True, text=True, timeout=90)
                    if failure in ('lease', 'expired-lease'):
                        result = subprocess.run(['python3', str(REPO / 'deploy/installations/host.py'), origin, '--update'],
                                                env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                                                     'CAIRN_INSTALLATION_ROOT': directory, 'FAILURE': failure},
                                                capture_output=True, text=True, timeout=90)
                    self.assertEqual(result.returncode, 0 if failure in ('', 'health', 'runner-unhealthy', 'stop-once', 'up-once', 'exec-once', 'interrupted', 'interrupted-stopped', 'interrupted-created', 'interrupted-missing', 'interrupted-restarting', 'interrupted-crash-loop', 'interrupted-unhealthy') else 1, result.stderr)
                    updated = json.loads((root / 'installation.json').read_text())
                    self.assertEqual(updated['image'], NEW if not failure else OLD)
                    if failure in ('health', 'runner-unhealthy'):
                        self.assertEqual(updated['failedImage'], NEW)
                    self.assertEqual((root / 'data/kept').read_text(), 'conversation and storage must survive')
                    self.assertNotIn('fixture-private-token', result.stdout + result.stderr)
                    events = [json.loads(line) for line in (root / 'events').read_text().splitlines()] if (root / 'events').exists() else []
                    if failure in ('digest', 'unapproved', 'lease', 'expired-lease', 'interrupted-exec-once', 'interrupted-foreign-restarting'):
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
                    if failure in ('interrupted', 'interrupted-created', 'interrupted-unhealthy', 'interrupted-exec-once', 'lease', 'stop-once', 'up-once', 'exec-once'):
                        # A brief outage is not evidence of an unhealthy candidate.
                        for _ in range(2):
                            retry = subprocess.run(['python3', str(REPO / 'deploy/installations/host.py'), origin, '--update'],
                                                   env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                                                        'CAIRN_INSTALLATION_ROOT': directory, 'FAILURE': ''},
                                                   capture_output=True, text=True, timeout=90)
                            self.assertEqual(retry.returncode, 0, retry.stderr)
                        recovered = json.loads((root / 'installation.json').read_text())
                        self.assertEqual(recovered['image'], NEW)
                        self.assertIsNone(recovered.get('failedImage'))
                finally:
                    server.shutdown()
                    server.server_close()


if __name__ == '__main__':
    unittest.main()
