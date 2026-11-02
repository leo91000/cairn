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
PRIVATE_DIAGNOSTIC = 'fixture-private-diagnostic-payload'
EXCEPTIONS = {'timeout': 'TimeoutExpired', 'os-error': 'OSError', 'value-error': 'ValueError'}
CANDIDATES = ('pending-healthy', 'candidate-manager-runtime', 'candidate-runner-runtime',
              'candidate-not-ready', 'candidate-unhealthy', 'candidate-runner-image', 'candidate-identity-changed',
              'candidate-release-noack')


class Updates(unittest.TestCase):
    def test_candidate_is_retained_and_retry_preserves_data_and_operator_settings(self):
        for failure in ('', 'ublk', 'unapproved', 'digest', 'lease', 'stop', 'up', 'health', 'unresponsive',
                        *CANDIDATES, *EXCEPTIONS):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)

                class Beacon(http.server.BaseHTTPRequestHandler):
                    def do_GET(self):
                        if self.path.startswith('/health/'):
                            _, _, role, fault, launches = self.path.split('/')
                            initial = launches == '0'
                            unhealthy = fault in ('health', 'foreign-lease', 'refused-lease', 'invalid-ack', 'missing-ack')
                            unhealthy = unhealthy or fault == 'candidate-unavailable' and int(launches) < 2
                            self.send_response(503 if unhealthy else 200)
                            self.end_headers()
                            runtime = 'other' if initial and fault == f'candidate-{role}-runtime' else 'fixture'
                            self.wfile.write(json.dumps({'status': 'ok', 'runtimeId': runtime,
                                'execution': {'ready': not (initial and fault == 'candidate-not-ready')}}).encode())
                            return
                        self.send_response(200)
                        self.end_headers()
                        image = NEW if failure != 'unapproved' else 'ghcr.io/leo91000/cairn:latest'
                        self.wfile.write(json.dumps({'image': image}).encode())

                    def do_POST(self):
                        self.rfile.read(int(self.headers.get('Content-Length', '0')))
                        status = {'/lease-conflict': 409, '/lease-refused': 403}.get(self.path, 200)
                        self.send_response(status)
                        self.end_headers()
                        paused = self.command != 'DELETE'
                        if self.path == '/lease-invalid-ack' or self.path == '/lease-release-noack' and self.command == 'DELETE':
                            paused = not paused
                        value = {} if self.path == '/lease-missing-ack' else {'paused': paused}
                        self.wfile.write(json.dumps(value).encode())

                    do_DELETE = do_POST

                    def log_message(self, *_):
                        pass

                server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Beacon)
                threading.Thread(target=server.serve_forever, daemon=True).start()
                origin = f'http://127.0.0.1:{server.server_port}'
                config = {'image': OLD, 'origin': origin}
                candidate = failure in CANDIDATES
                if candidate:
                    config.update(pendingImage=NEW, leaseOwner='00000000-0000-4000-8000-000000000001',
                                  leaseAcquired=True)
                (root / 'installation.json').write_text(json.dumps(config))
                (root / 'data').mkdir()
                (root / 'data/kept').write_text('retained conversation')
                (root / 'data/maintenance-token').write_text('fixture-private-token')
                (root / '.env').write_text('')
                (root / 'compose.json').write_text(json.dumps({'services': {
                    'manager': {'image': NEW if candidate else OLD,
                                'environment': {'CAIRN_NODE_IMAGE': NEW if candidate else OLD}, 'mem_limit': '6g'},
                    'runner': {'image': NEW if candidate else OLD, 'mem_limit': '18g',
                               'environment': {'OPERATOR_SETTING': '${KEEP_ME}'}},
                    'garage': {'image': 'retained-storage'},
                }}))
                original_compose = (root / 'compose.json').read_bytes()
                binaries = root / 'bin'
                binaries.mkdir()
                docker = binaries / 'docker'
                docker.write_text(r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
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
    launches = sum('up' in json.loads(event) for event in (root / 'events').read_text().splitlines())
    service = args[-1] if args[-1] in ('manager', 'runner') else 'manager'
    image = json.loads((root / 'compose.json').read_text())['services'][service]['image']
    if failure == 'foreign-manager':
        image = 'foreign-image'
    if failure == 'candidate-runner-image' and service == 'runner' and launches == 0:
        image = 'foreign-image'
    identity = service + '-fixture'
    probes = sum('ps' in json.loads(event) and json.loads(event)[-1] == service for event in (root / 'events').read_text().splitlines())
    if failure == 'candidate-identity-changed' and launches == 0 and probes > 1:
        identity += '-replaced'
    healthy = failure.startswith(('candidate-', 'pending-')) and not (failure == 'candidate-unhealthy' and launches == 0)
    print(json.dumps({'ID': identity, 'Image': image, 'State': 'running',
                      'Health': 'healthy' if healthy else 'unhealthy'}))
if 'exec' in args:
    launches = sum('up' in json.loads(event) for event in (root / 'events').read_text().splitlines())
    unavailable = failure == 'candidate-unavailable' and launches < 2
    if 'deployment-lease' in args[-1]:
        if failure in ('lease', 'foreign-manager') or unavailable:
            sys.exit(1)
        route = {'foreign-lease': '/lease-conflict', 'refused-lease': '/lease-refused',
                 'invalid-ack': '/lease-invalid-ack', 'missing-ack': '/lease-missing-ack',
                 'candidate-release-noack': '/lease-release-noack'}.get(failure, '/lease-accepted')
        script = args[-1].replace("'/data/maintenance-token'", json.dumps(str(root / 'data/maintenance-token')))
        script = script.replace("'http://127.0.0.1:4310/internal/deployment-lease'",
                                json.dumps(os.environ['FIXTURE_MANAGER_ORIGIN'] + route))
        sys.exit(subprocess.run(['node', '-e', script]).returncode)
    if failure == 'unresponsive' and 'deployment-lease' not in args[-1]:
        sys.exit(1)
    role = args[args.index('node') - 1]
    script = args[-1]
    for port in (4310, 4311):
        script = script.replace(f'http://127.0.0.1:{port}/health',
                                os.environ['FIXTURE_MANAGER_ORIGIN'] + f'/health/{role}/{failure or "none"}/{launches}')
    sys.exit(subprocess.run(['node', '-e', script]).returncode)
''')
                docker.chmod(0o755)
                installer, _ = write_supervisors(root)
                # Exercise the actual CLI exception boundary without waiting out
                # a timeout or exposing exception command/output values.
                injection = '''
_fixture_run = subprocess.run
def _fixture_observed_run(arguments, **options):
    if 'up' in arguments:
        index = arguments.index('--wait-timeout')
        (ROOT / 'up-budget.json').write_text(json.dumps({
            'wait': arguments[index + 1], 'timeout': options.get('timeout'),
        }))
        fault = os.environ.get('FAILURE')
        private = 'fixture-private-diagnostic-payload'
        if fault == 'timeout':
            raise subprocess.TimeoutExpired([private], 150, output=private, stderr=private)
        if fault == 'os-error':
            raise OSError(5, private)
        if fault == 'value-error':
            raise ValueError(private)
    return _fixture_run(arguments, **options)
subprocess.run = _fixture_observed_run
'''
                source = installer.read_text()
                marker = "if __name__ == '__main__':"
                self.assertEqual(source.count(marker), 1)
                installer.write_text(source.replace(marker, injection + '\n' + marker))

                def update(fault):
                    return subprocess.run(['python3', str(installer), origin, '--update'],
                                          env={**os.environ, 'PATH': str(binaries) + ':' + os.environ['PATH'],
                                               'CAIRN_INSTALLATION_ROOT': directory, 'FAILURE': fault,
                                               'FIXTURE_MANAGER_ORIGIN': origin},
                                          capture_output=True, text=True, timeout=30)

                try:
                    result = update(failure)
                    expected_error = bool(failure) and not candidate or failure == 'candidate-release-noack'
                    self.assertEqual(result.returncode, int(expected_error), result.stderr)
                    config = json.loads((root / 'installation.json').read_text())
                    self.assertEqual(config['image'], OLD if expected_error and not candidate else NEW)
                    if failure in ('lease', 'stop', 'up', 'health', 'unresponsive'):
                        self.assertEqual(config['pendingImage'], NEW)
                    events = [json.loads(line) for line in (root / 'events').read_text().splitlines()] if (root / 'events').exists() else []
                    starts = [event for event in events if 'up' in event]
                    stop_count = sum('stop' in event for event in events)
                    if failure in ('pending-healthy', 'candidate-release-noack'):
                        self.assertEqual(stop_count, 0, 'A healthy candidate must settle without stopping either service')
                        self.assertEqual(len(starts), 0, 'A healthy candidate must settle without another startup')
                        self.assertEqual((root / 'compose.json').read_bytes(), original_compose)
                    elif candidate:
                        self.assertEqual(stop_count, 2, 'Incomplete candidate proof must use the fenced launch path')
                        self.assertEqual(len(starts), 1)
                    if failure == 'candidate-release-noack':
                        self.assertNotIn('pendingImage', config)
                        self.assertIn('leaseOwner', config, 'A missing release acknowledgement must retain its owner')
                    if not failure:
                        self.assertEqual(json.loads((root / 'up-budget.json').read_text()),
                                         {'wait': '240', 'timeout': 300})
                    if failure in EXCEPTIONS:
                        failures = [json.loads(line) for line in result.stderr.splitlines()
                                    if line.startswith('{') and json.loads(line).get('event') == 'installation_update_failed']
                        self.assertEqual(failures, [{'event': 'installation_update_failed',
                                                     'phase': 'compose_up', 'exception': EXCEPTIONS[failure]}])
                        self.assertNotIn(PRIVATE_DIAGNOSTIC, result.stdout + result.stderr)
                    self.assertLessEqual(len(starts), 1, 'A failed candidate must never launch a rollback')
                    if failure in ('ublk', 'unapproved', 'digest', 'lease'):
                        self.assertFalse(any('stop' in event for event in events))
                    if failure not in ('ublk', 'unapproved'):
                        if failure == 'lease':
                            interrupted = update('up')
                            self.assertNotEqual(interrupted.returncode, 0)
                            acknowledged = json.loads((root / 'installation.json').read_text())
                            self.assertTrue(acknowledged.get('leaseAcquired'),
                                            'A retry must save its lease acknowledgement before stopping the manager')
                            self.assertEqual(acknowledged['leaseOwner'], config['leaseOwner'])
                            retry = update('candidate-unavailable')
                        elif failure in ('health', 'unresponsive'):
                            for refused in ('foreign-lease', 'refused-lease', 'invalid-ack', 'missing-ack', 'foreign-manager'):
                                rejected = update(refused)
                                self.assertNotEqual(rejected.returncode, 0)
                                if refused == 'foreign-lease':
                                    self.assertIn('Another deployment owns the installation lease.', rejected.stderr)
                                elif refused == 'refused-lease':
                                    self.assertIn('The manager refused the deployment lease.', rejected.stderr)
                                after = [json.loads(line) for line in (root / 'events').read_text().splitlines()]
                                self.assertEqual(sum('stop' in event for event in after), stop_count)
                                self.assertEqual(sum('up' in event for event in after), len(starts))
                            retry = update('candidate-unavailable')
                        else:
                            retry = update('')
                        self.assertEqual(retry.returncode, 0, retry.stderr)
                        config = json.loads((root / 'installation.json').read_text())
                        self.assertEqual(config['image'], NEW)
                        self.assertNotIn('pendingImage', config)
                        self.assertNotIn('leaseOwner', config)
                        self.assertNotIn('leaseAcquired', config)
                        if failure == 'candidate-release-noack':
                            after = [json.loads(line) for line in (root / 'events').read_text().splitlines()]
                            self.assertFalse(any('stop' in event or 'up' in event for event in after),
                                             'Retrying the release acknowledgement must never relaunch the committed candidate')
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
