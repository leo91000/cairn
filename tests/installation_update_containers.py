"""Real manager/runner container replacement and rollback through the supervisor CLI.

The registry seam maps approved references to locally built fixture images. Docker
Compose, mounts, image runtime metadata, health, leases and the official relay are
real. The runner serves readiness only: this test requires no KVM or agent login.
"""
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

REPO = Path(__file__).resolve().parents[1]
DOCKER = shutil.which('docker')
BASE = 'python:3.13-slim-trixie@sha256:3dd7cc108ec1493442514f5c2a871af6af0ec31d768ff6e378a93340c3b3db5f'


def command(args, timeout=180, **kwargs):
    return subprocess.check_output(args, stderr=subprocess.PIPE, timeout=timeout, **kwargs)


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def wait(check):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except (OSError, urllib.error.HTTPError):
            pass
        time.sleep(0.1)
    raise AssertionError('Container fixture did not become ready')


def main():
    database = os.environ['LEO_OFFICIAL_TEST_DATABASE_URL']
    name = 'leo-update-' + uuid.uuid4().hex[:12]
    images = []
    official = None
    messages = []
    manager_port, runner_port, official_port = port(), port(), port()
    origin = f'http://127.0.0.1:{official_port}'
    cookie, csrf = '', ''

    def request(route, body=None):
        req = urllib.request.Request(origin + route, headers={
            'Content-Type': 'application/json', 'Origin': origin,
            'Cookie': cookie, 'X-CSRF-Token': csrf,
        }, data=None if body is None else json.dumps(body).encode())
        with urllib.request.urlopen(req, timeout=10) as response:
            return json.load(response), response.headers

    class Mailbox(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            messages.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{}')

        def log_message(self, *_):
            pass

    mailbox = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Mailbox)
    threading.Thread(target=mailbox.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix=name) as directory:
        root = Path(directory)
        root.chmod(0o755)
        compose_file = root / 'compose.actual.json'
        docker = [DOCKER, 'compose', '-f', str(compose_file)]
        try:
            for folder in ('data', 'home', 'workspaces', 'runner-state', 'bin', 'build'):
                (root / folder).mkdir()
            build = root / 'build'
            shutil.copy(REPO / 'target/debug/leo', build / 'leo')
            node = command(['node', '-p', 'process.execPath']).decode().strip()
            shutil.copy(node, build / 'node')
            (build / 'runner.py').write_text('''import http.server, json, os, signal, sys
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
from pathlib import Path
runtime = os.environ['APP_RUNTIME_ID']
Path('/runner-state/kept').touch(exist_ok=True)
class Ready(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(json.dumps({'status': 'ok', 'backend': 'firecracker', 'runtimeId': runtime}).encode())
    def log_message(self, *_):
        pass
http.server.HTTPServer(('127.0.0.1', int(os.environ['PORT'])), Ready).serve_forever()
''')
            (build / 'Dockerfile').write_text(f'''FROM {BASE}
COPY leo node /usr/local/bin/
COPY runner.py /runner.py
ARG RUNTIME
ENV APP_RUNTIME_ID=$RUNTIME
''')
            registry = {}
            for runtime in ('previous', 'approved', 'failed', 'failed-http'):
                tag = f'{name}:{runtime}'
                command([DOCKER, 'build', '-t', tag, '--build-arg', f'RUNTIME={runtime}', str(build)])
                images.append(tag)
                metadata = json.loads(command([DOCKER, 'image', 'inspect', tag]))[0]
                # Only the private fixture registry is substituted; the supervisor
                # still requires the production repository and an immutable digest.
                reference = 'ghcr.io/leo91000/leo-agent-manager@' + metadata['Id']
                registry[reference] = metadata['Id']
            previous, approved, failed, failed_http = registry
            (root / 'registry.json').write_text(json.dumps(registry))
            (root / 'bin/docker').write_text('''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['LEO_INSTALLATION_ROOT'])
registry = json.loads((root / 'registry.json').read_text())
args = sys.argv[1:]
if args[0] == 'pull':
    assert args[-1] in registry
    sys.exit(0)
if args[:2] == ['image', 'inspect']:
    metadata = json.loads(subprocess.check_output([os.environ['FIXTURE_DOCKER'], 'image', 'inspect', registry[args[-1]]]))
    metadata[0]['RepoDigests'] = [args[-1]]
    print(json.dumps(metadata))
    sys.exit(0)
source = root / 'compose.json'
target = root / 'compose.actual.json'
if 'up' in args or not target.exists():
    config = json.loads(source.read_text())
    for service in ('manager', 'runner'):
        ref = config['services'][service]['image']
        config['services'][service]['image'] = registry[ref]
        if service == 'manager' and ref == list(registry)[2]:
            config['services'][service]['entrypoint'] = ['/bin/sh', '-c', 'exit 1']
        if service == 'manager' and ref == list(registry)[3]:
            config['services'][service]['entrypoint'] = ['python3', '-c', 'import signal, sys, time; signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)); time.sleep(600)']
    target.write_text(json.dumps(config))
args[args.index('-f') + 1] = str(target)
if 'ps' in args:
    result = subprocess.check_output([os.environ['FIXTURE_DOCKER'], *args]).decode().strip()
    if result:
        value = json.loads(result)
        for reference, image in registry.items():
            if value['Image'] == image:
                value['Image'] = reference
        print(json.dumps(value))
    sys.exit(0)
if 'exec' in args:
    args[-1] = args[-1].replace('127.0.0.1:4310', '127.0.0.1:' + os.environ['FIXTURE_MANAGER_PORT'])
sys.exit(subprocess.run([os.environ['FIXTURE_DOCKER'], *args]).returncode)
''')
            (root / 'bin/docker').chmod(0o755)
            healthcheck = {'test': ['CMD', 'node', '-e',
                "fetch('http://127.0.0.1:" + str(runner_port) + "/health').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"],
                'interval': '1s', 'timeout': '2s', 'retries': 5}
            config = {'name': name, 'services': {
                'manager': {
                    'image': previous, 'network_mode': 'host', 'user': f'{os.getuid()}:{os.getgid()}',
                    'entrypoint': ['/usr/local/bin/leo', 'serve'],
                    'environment': {'DATA_DIR': '/data', 'AGENT_HOME': '/home/node',
                        'HOST': '127.0.0.1', 'PORT': str(manager_port), 'WORKER_ENABLED': 'false',
                        'WORKSPACE_ROOTS': '/workspaces', 'PUBLIC_URL': f'http://127.0.0.1:{manager_port}',
                        'RUNNER_URL': f'http://127.0.0.1:{runner_port}', 'LEO_OFFICIAL_ORIGIN': origin,
                        'LEO_NODE_IMAGE': previous},
                    'volumes': [f'{root}/data:/data', f'{root}/home:/home/node', f'{root}/workspaces:/workspaces'],
                },
                'runner': {
                    'image': previous, 'network_mode': 'host', 'user': f'{os.getuid()}:{os.getgid()}',
                    'entrypoint': ['python3', '/runner.py'], 'environment': {'PORT': str(runner_port)},
                    'volumes': [f'{root}/runner-state:/runner-state'], 'healthcheck': healthcheck,
                },
            }}
            (root / 'compose.json').write_text(json.dumps(config))
            (root / 'installation.json').write_text(json.dumps({'origin': origin, 'image': previous}))
            env = {'PATH': str(root / 'bin') + ':' + os.environ['PATH'],
                   'LEO_INSTALLATION_ROOT': str(root), 'FIXTURE_DOCKER': DOCKER,
                   'FIXTURE_MANAGER_PORT': str(manager_port)}

            def service(image):
                return subprocess.Popen([str(REPO / 'target/debug/leo-official')],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env={
                        'LEO_OFFICIAL_DATABASE_URL': database, 'LEO_OFFICIAL_ORIGIN': origin,
                        'LEO_OFFICIAL_LISTEN': f'127.0.0.1:{official_port}',
                        'LEO_OFFICIAL_EMAIL_FROM': 'fixture@example.test', 'LEO_OFFICIAL_EMAIL_KEY': 'fixture-only',
                        'LEO_OFFICIAL_EMAIL_ENDPOINT': f'http://127.0.0.1:{mailbox.server_port}/emails',
                        'LEO_INSTALLATION_IMAGE': image,
                    })

            official = service(approved)
            wait(lambda: request('/api/account/options'))
            challenge, _ = request('/api/account/email-code', {'email': f'{name}@example.test'})
            code = re.search(r'\b\d{8}\b', messages[-1]['text'])[0]
            session, headers = request('/api/account/verify', {'challenge': challenge['challenge'], 'code': code})
            cookie, csrf = headers['Set-Cookie'].split(';')[0], session['csrf']
            claim, _ = request('/api/installations/claim-code', {})
            config['services']['manager']['environment']['LEO_INSTALLATION_CLAIM_CODE'] = claim['code']
            (root / 'compose.json').write_text(json.dumps(config))
            command([str(root / 'bin/docker'), 'compose', '-f', str(root / 'compose.json'), 'up', '-d', '--wait'], env=env)
            installation = wait(lambda: request('/api/installations')[0])[0]['id']
            base = f'/api/installations/{installation}/api'
            wait(lambda: request(base + '/agents'))
            chat, _ = request(base + '/chats', {})
            identity = (root / 'data/installation-relay/identity.json').read_bytes()
            (root / 'workspaces/kept').write_text('retained workspace')
            (root / 'home/kept').write_text('synthetic coding-agent credential')
            (root / 'runner-state/kept').write_text('retained runner state')
            # No one-use claim code remains in recreated containers.
            del config['services']['manager']['environment']['LEO_INSTALLATION_CLAIM_CODE']
            (root / 'compose.json').write_text(json.dumps(config))

            for image, interrupted in ((approved, False), (failed, False), (failed, True), (failed_http, True)):
                if image in (failed, failed_http):
                    official.terminate()
                    official.wait(timeout=15)
                    official = service(image)
                    wait(lambda: request('/api/account/options'))
                before = command(docker + ['ps', '-q', 'manager', 'runner'])
                if interrupted:
                    owner = str(uuid.uuid4())
                    lease_script = """
const fs = require('fs');
fetch('http://127.0.0.1:%d/internal/deployment-lease', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + fs.readFileSync('/data/maintenance-token', 'utf8').trim() },
    body: JSON.stringify({ owner: %s }),
}).then(r => process.exit(r.ok ? 0 : 1)).catch(() => process.exit(1));
""" % (manager_port, json.dumps(owner))
                    command(docker + ['exec', '-T', 'manager', 'node', '-e', lease_script])
                    journal = json.loads((root / 'installation.json').read_text())
                    journal.update(pendingImage=image, leaseOwner=owner, leaseAcquired=True)
                    (root / 'installation.json').write_text(json.dumps(journal))
                    deployment = json.loads((root / 'compose.json').read_text())
                    deployment['services']['manager'].update(image=image, restart='unless-stopped')
                    deployment['services']['runner']['image'] = image
                    (root / 'compose.json').write_text(json.dumps(deployment))
                    command([str(root / 'bin/docker'), 'compose', '-f', str(root / 'compose.json'), 'up', '-d'], env=env)
                    expected_state = 'running' if image == failed_http else 'restarting'
                    wait(lambda: json.loads(command(docker + ['ps', '--all', '--format', 'json', 'manager']))['State'] == expected_state)
                command(['python3', str(REPO / 'deploy/installations/host.py'), origin, '--update'], env=env, timeout=300)
                after = command(docker + ['ps', '-q', 'manager', 'runner'])
                assert before != after, 'Both updates must replace actual containers'
                current = json.loads((root / 'installation.json').read_text())
                assert current['image'] == approved and not current.get('leaseOwner')
                if image in (failed, failed_http):
                    assert current['failedImage'] == failed
                if image == failed_http:
                    assert current.get('failedImage') != failed_http, 'Interrupted recovery must leave the approved digest eligible for retry'
                wait(lambda: request(base + '/chats/' + chat['id']))
                assert identity == (root / 'data/installation-relay/identity.json').read_bytes()
                assert (root / 'workspaces/kept').read_text() == 'retained workspace'
                assert (root / 'home/kept').read_text() == 'synthetic coding-agent credential'
                assert (root / 'runner-state/kept').read_text() == 'retained runner state'
                health = json.load(urllib.request.urlopen(f'http://127.0.0.1:{manager_port}/health'))
                assert health['runtimeId'] == 'approved' and not health['maintenance']
            print('Real containers: approved replacement and exited/restarting/HTTP-dead-candidate rollback preserve conversations, identity, workspace, credentials and runner state')
        finally:
            if compose_file.exists():
                subprocess.run(docker + ['down', '--timeout', '10'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
            if official:
                official.terminate()
                official.wait(timeout=15)
            for image in images:
                subprocess.run([DOCKER, 'image', 'rm', image], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
            subprocess.run([DOCKER, 'run', '--rm', '-v', f'{root}:/fixture', BASE,
                            'sh', '-c', 'rm -rf /fixture/*'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
            mailbox.shutdown()
            mailbox.server_close()


if __name__ == '__main__':
    main()
