"""Public shell installer in a disposable Linux container, without KVM.

Docker lifecycle commands and loopback download transport are adapted. The real
Compose parser validates the generated configuration on the controller. Claiming and
relay use real binaries and Postgres; no VM is booted.
"""
import http.server
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

ROOT = Path('/fixture/installation')
ORIGIN = 'http://127.0.0.1:48151'
IMAGE = 'ghcr.io/leo91000/cairn@sha256:' + '1' * 64
MAIL = []


def request(route, body=None, cookie='', csrf='', method=None):
    headers = {'Content-Type': 'application/json', 'Origin': ORIGIN,
               'Cookie': cookie, 'X-CSRF-Token': csrf}
    req = urllib.request.Request(ORIGIN + route, headers=headers, method=method,
                                 data=None if body is None else json.dumps(body).encode())
    try:
        with urllib.request.urlopen(req, timeout=45) as response:
            data = response.read()
            return json.loads(data) if data else None, response.headers
    except urllib.error.HTTPError as error:
        detail = error.read().decode()
        raise AssertionError(f'{method or req.get_method()} {route}: HTTP {error.code}: {detail}') from error


def wait_for(check):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        try:
            if check():
                return
        except (OSError, urllib.error.HTTPError, KeyError, AssertionError):
            pass
        time.sleep(0.2)
    raise AssertionError('The real Beacon or installation did not become ready')


def run_installer(code=''):
    if os.environ.get('INSTALLER_VERIFY'):
        with urllib.request.urlopen(ORIGIN + '/install.sh') as response:
            script = response.read().decode()
    else:
        import hashlib
        checksum = hashlib.sha256(Path('deploy/installations/host.py').read_bytes()).hexdigest()
        script = Path('deploy/installations/install.sh').read_text().replace(
            '__CAIRN_BEACON_ORIGIN__', repr(ORIGIN)).replace('__CAIRN_HOST_SHA256__', checksum).replace(
                '__CAIRN_NODE_HOST_SHA256__', hashlib.sha256(Path('deploy/nodes/host.py').read_bytes()).hexdigest())
    # Compressed swap is a host setting covered by compressed_swap_test.py.
    swaps = Path('/fixture/swaps')
    swaps.write_text('Filename Type Size Used Priority\n/dev/zram0 partition 1024 0 100\n')
    result = subprocess.run(['bash', '-s', '--', '--claim-code', code], input=script,
                            capture_output=True, text=True, timeout=240, env={
                                **os.environ, 'CAIRN_INSTALLATION_ROOT': str(ROOT),
                                'PATH': '/fixture/bin:' + os.environ['PATH'],
                                'CAIRN_PROC_SWAPS': str(swaps),
                            })
    assert code == '' or code not in result.stdout + result.stderr, 'Claim leaked in output'
    return result


def main(mode):
    if mode == 'compose':
        config = json.loads(subprocess.check_output([
            '/usr/local/bin/docker-compose', '--project-directory', str(ROOT),
            '-f', str(ROOT / 'compose.json'), 'config', '--format', 'json',
        ], env={**os.environ, 'CAIRN_INSTALLATION_CLAIM_CODE': 'a' * 64}))
        services = config['services']
        assert set(services) == {'manager', 'runner'}
        assert services['manager']['environment']['CAIRN_INSTALLATION_CLAIM_CODE'] == 'a' * 64
        assert services['manager']['user'] == '1000:1000'
        assert services['manager']['depends_on']['runner']['condition'] == 'service_healthy'
        assert not any(service.get('ports') for service in services.values())
        for name in ('data', 'home', 'workspaces'):
            assert (ROOT / name).stat().st_uid == 1000
        print('Container: real Docker Compose parser, healthcheck, claim substitution, dependencies and uid passed')
        return

    # Devices are inert character placeholders: no ioctl or VM boot is performed.
    Path('/dev/net').mkdir(exist_ok=True)
    for device in ('/dev/kvm', '/dev/net/tun'):
        if not Path(device).exists():
            os.mknod(device, stat.S_IFCHR | 0o600, os.makedev(1, 3))
    Path('/etc/systemd/system').mkdir(parents=True, exist_ok=True)
    binaries = Path('/fixture/bin')
    binaries.mkdir(exist_ok=True)
    docker = binaries / 'docker'
    docker.write_text('''#!/usr/bin/env python3
import json, os, pathlib, signal, subprocess, sys, time, urllib.request
root = pathlib.Path('/fixture/installation')
args = sys.argv[1:]
if 'info' in args and os.environ.get('INSTALLER_DOCKER_DOWN'):
    sys.exit(1)
if 'version' in args and os.environ.get('INSTALLER_COMPOSE_MISSING'):
    sys.exit(1)
if 'inspect' in args:
    print(json.dumps([{'RepoDigests': [args[-1]], 'Config': {'Env': ['APP_RUNTIME_ID=' + args[-1].split(':')[-1]]}}]))
if 'stop' in args and args[-1] == 'manager':
    pid_file = root / 'manager.pid'
    if pid_file.exists():
        os.kill(int(pid_file.read_text()), signal.SIGTERM)
        time.sleep(1)
        pid_file.unlink()
if 'up' in args and os.environ.get('INSTALLER_VERIFY'):
    pid_file = root / 'manager.pid'
    if pid_file.exists():
        os.kill(int(pid_file.read_text()), signal.SIGTERM)
        time.sleep(1)
    code = ''
    claim = root / 'claim.env'
    if claim.exists():
        code = dict(line.split('=', 1) for line in claim.read_text().splitlines()).get('CAIRN_INSTALLATION_CLAIM_CODE', '')
    env = {**os.environ, 'DATA_DIR': str(root / 'data'), 'AGENT_HOME': str(root / 'home'),
           'WORKSPACE_ROOTS': str(root / 'workspaces'), 'WORKER_ENABLED': 'false', 'NODE_ENV': 'test',
           'HOST': '127.0.0.1', 'PORT': '48152', 'CAIRN_BEACON_ORIGIN': 'http://127.0.0.1:48151',
           'CAIRN_INSTALLATION_CLAIM_CODE': code,
           'APP_RUNTIME_ID': json.loads((root / 'compose.json').read_text())['services']['manager']['image'].split(':')[-1]}
    if os.environ.get('INSTALLER_FAIL_IMAGE') == env['APP_RUNTIME_ID']:
        env['APP_RUNTIME_ID'] = 'wrong-runtime'
    # Synthetic child credentials only; isolate from any controller agent broker.
    for name in list(env):
        if name.startswith(('CAIRN_AUTH_', 'CODEX_', 'OPENAI_', 'ANTHROPIC_')):
            env.pop(name)
    log = open(root / 'manager.log', 'ab')
    process = subprocess.Popen(['/repo/target/debug/cairn', 'serve'], env=env, stdout=log, stderr=log, start_new_session=True)
    pid_file.write_text(str(process.pid))
    if '--wait' in args:
        for attempt in range(100):
            try:
                urllib.request.urlopen('http://127.0.0.1:48152/health', timeout=2)
                break
            except Exception:
                time.sleep(0.1)
        else:
            sys.exit(1)
if 'exec' in args and os.environ.get('INSTALLER_VERIFY'):
    script = args[-1].replace('127.0.0.1:4310', '127.0.0.1:48152').replace('/data/maintenance-token', str(root / 'data/maintenance-token'))
    sys.exit(subprocess.run(['/usr/local/bin/node', '-e', script]).returncode)
''')
    docker.chmod(0o755)
    # Production downloads must require HTTPS. This isolated loopback fixture has
    # no TLS server; adapt only its transport, without touching product behavior.
    curl = binaries / 'curl'
    curl.write_text('''#!/usr/bin/env python3
import subprocess, sys
args = sys.argv[1:]
assert args[args.index('--proto') + 1] == '=https'
assert args[-1] in ('http://127.0.0.1:48151/install/host.py', 'http://127.0.0.1:48151/install/node-host.py')
args[args.index('--proto') + 1] = '=http'
sys.exit(subprocess.run(['/usr/bin/curl', *args]).returncode)
''')
    curl.chmod(0o755)
    for name, contents in {
        'uname': '#!/bin/sh\nif [ "$1" = -s ]; then echo "${INSTALLER_OS:-Linux}"; else echo "${INSTALLER_ARCH:-x86_64}"; fi\n',
        'systemctl': '#!/bin/sh\nexit 0\n',
        'df': '#!/bin/sh\necho "Filesystem 1024-blocks Used Available Capacity Mounted"\necho "fixture 67108864 0 ${INSTALLER_FREE_KB:-33554432} 0% /fixture"\n',
    }.items():
        binary = binaries / name
        binary.write_text(contents)
        binary.chmod(0o755)

    class Beacon(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            if self.path == '/install/release':
                self.wfile.write(json.dumps({'image': IMAGE}).encode())
            else:
                body = Path('deploy/nodes/host.py' if self.path == '/install/node-host.py' else 'deploy/installations/host.py').read_bytes()
                if os.environ.get('INSTALLER_TAMPER_HOST'):
                    body += b'\n# mismatched download\n'
                self.wfile.write(body)

        def log_message(self, *_):
            pass

    class Mailbox(Beacon):
        def do_POST(self):
            MAIL.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{}')

    if mode == 'prepare':
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 48151), Beacon)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            for variable, value, message in [('INSTALLER_OS', 'Darwin', 'Linux'),
                                              ('INSTALLER_ARCH', 'aarch64', 'x86-64'),
                                              ('INSTALLER_FREE_KB', '1024', '16 GiB'),
                                              ('INSTALLER_DOCKER_DOWN', '1', 'Start the Docker daemon'),
                                              ('INSTALLER_COMPOSE_MISSING', '1', 'Compose plugin')]:
                os.environ[variable] = value
                result = run_installer()
                assert result.returncode != 0 and message in result.stderr, result.stderr
                del os.environ[variable]
            for device, message in [('/dev/kvm', 'KVM'), ('/dev/net/tun', 'TUN')]:
                Path(device).unlink()
                result = run_installer()
                assert result.returncode != 0 and message in result.stderr, result.stderr
                os.mknod(device, stat.S_IFCHR | 0o600, os.makedev(1, 3))
            existing_host = (ROOT / 'host.py').read_bytes() if (ROOT / 'host.py').exists() else None
            os.environ['INSTALLER_TAMPER_HOST'] = '1'
            result = run_installer()
            assert result.returncode != 0 and 'checksum' in result.stderr, result.stderr
            assert ((ROOT / 'host.py').read_bytes() if (ROOT / 'host.py').exists() else None) == existing_host
            del os.environ['INSTALLER_TAMPER_HOST']
            result = run_installer()
            assert result.returncode == 0, result.stderr
            config = json.loads((ROOT / 'compose.json').read_text())
            assert not any('ports' in service for service in config['services'].values())
            assert not (ROOT / 'claim.env').exists()
            assert (ROOT / 'node-host.py').read_bytes() == Path('deploy/nodes/host.py').read_bytes()
            assert 'OnUnitActiveSec=5min' in Path('/etc/systemd/system/cairn-installation-update.timer').read_text()
            assert '--update' in Path('/etc/systemd/system/cairn-installation-update.service').read_text()
            assert not (ROOT / 'data/storage-s3.json').exists()
            assert not (ROOT / 'garage.env').exists()
            assert not (ROOT / 'garage.toml').exists()
            assert not (ROOT / 'garage').exists()
            # A pre-local-disk installation may have stopped between its private
            # config write and chown. Its saved image/service must remain usable.
            legacy_storage = ROOT / 'data/storage-s3.json'
            legacy_storage.write_bytes(b'opaque legacy storage credentials')
            legacy_storage.chmod(0o600)
            os.chown(legacy_storage, 0, 0)
            config['services']['garage'] = {'image': 'retained-legacy-storage-image'}
            (ROOT / 'compose.json').write_text(json.dumps(config))
            result = run_installer()
            assert result.returncode == 0, result.stderr
            assert legacy_storage.stat().st_uid == 1000
            assert legacy_storage.stat().st_mode & 0o777 == 0o600
            assert legacy_storage.read_bytes() == b'opaque legacy storage credentials'
            retained = json.loads((ROOT / 'compose.json').read_text())
            assert retained['services']['garage'] == config['services']['garage']
            # Leave the fresh-install fixture for the following real parser and
            # manager/Beacon phases; only the synthetic legacy input is removed.
            del config['services']['garage']
            (ROOT / 'compose.json').write_text(json.dumps(config))
            legacy_storage.unlink()
            print('Container: KVM/TUN diagnostics, manager and local runner configuration passed')
        finally:
            server.shutdown()
        return

    mailbox = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Mailbox)
    threading.Thread(target=mailbox.serve_forever, daemon=True).start()
    env = {**os.environ,
           'CAIRN_BEACON_DATABASE_URL': os.environ['CAIRN_BEACON_TEST_DATABASE_URL'],
           'CAIRN_BEACON_ORIGIN': ORIGIN, 'CAIRN_BEACON_LISTEN': '127.0.0.1:48151',
           'CAIRN_BEACON_WEB_DIR': '/repo/dist', 'CAIRN_INSTALLATION_IMAGE': IMAGE,
           'CAIRN_BEACON_EMAIL_FROM': 'fixture@example.test', 'CAIRN_BEACON_EMAIL_KEY': 'fixture-only',
           'CAIRN_BEACON_EMAIL_ENDPOINT': f'http://127.0.0.1:{mailbox.server_port}/emails'}
    os.environ['INSTALLER_VERIFY'] = '1'
    beacon = subprocess.Popen(['/repo/target/debug/cairn-beacon'], env=env,
                                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    def beacon_ready():
        if beacon.poll() is not None:
            raise RuntimeError('Fixture Beacon exited: ' + beacon.communicate()[1].decode())
        return request('/api/account/options')[0] is not None

    try:
        wait_for(beacon_ready)
        challenge, _ = request('/api/account/email-code', {'email': 'installer@example.test'})
        import re
        code = re.search(r'\b\d{8}\b', MAIL[-1]['text'])[0]
        session, headers = request('/api/account/verify', {'challenge': challenge['challenge'], 'code': code})
        cookie = headers['Set-Cookie'].split(';')[0]
        csrf = session['csrf']
        claim, _ = request('/api/installations/claim-code', {}, cookie, csrf)
        result = run_installer(claim['code'])
        assert result.returncode == 0 and 'installed and claimed' in result.stdout, result.stderr
        session, _ = request('/api/account/session', cookie=cookie)
        assert len(session['installations']) == 1
        installation = session['installations'][0]['id']
        base = f'/api/installations/{installation}/api'
        wait_for(lambda: request(base + '/chats', cookie=cookie)[0] == [])
        print('Container: real claim and relay passed', flush=True)
        identity = (ROOT / 'data/installation-relay/identity.json').read_bytes()
        result = run_installer(claim['code'])
        assert result.returncode == 0, result.stderr
        assert identity == (ROOT / 'data/installation-relay/identity.json').read_bytes()
        assert not (ROOT / 'claim.env').exists()
        session, _ = request('/api/account/session', cookie=cookie)
        assert len(session['installations']) == 1
        wait_for(lambda: request(base + '/chats', cookie=cookie)[0] == [])
        print('Container: real claim, relay and idempotent rerun passed')
        # The Docker lifecycle adapter replaces only container orchestration.
        # The supervisor executes real Node health/lease calls against the manager.
        for digit, fail in [('b', False), ('c', True)]:
            candidate = 'ghcr.io/leo91000/cairn@sha256:' + digit * 64
            beacon.terminate()
            beacon.wait(timeout=15)
            beacon = subprocess.Popen(['/repo/target/debug/cairn-beacon'],
                                        env={**env, 'CAIRN_INSTALLATION_IMAGE': candidate},
                                        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            wait_for(beacon_ready)
            update_env = {**os.environ, 'PATH': '/fixture/bin:' + os.environ['PATH'],
                          'CAIRN_INSTALLATION_ROOT': str(ROOT)}
            if fail:
                update_env['INSTALLER_FAIL_IMAGE'] = digit * 64
            result = subprocess.run(['python3', str(ROOT / 'host.py'), ORIGIN, '--update'],
                                    env=update_env, capture_output=True, text=True, timeout=120)
            if fail:
                assert result.returncode != 0, 'A wrong-runtime candidate must not be committed or rolled back'
                pending = json.loads((ROOT / 'installation.json').read_text())
                assert pending['image'].endswith('b' * 64)
                assert pending['pendingImage'] == candidate and pending['leaseAcquired']
                deployment = json.loads((ROOT / 'compose.json').read_text())
                assert all(deployment['services'][name]['image'] == candidate for name in ('manager', 'runner'))
                update_env.pop('INSTALLER_FAIL_IMAGE')
                result = subprocess.run(['python3', str(ROOT / 'host.py'), ORIGIN, '--update'],
                                        env=update_env, capture_output=True, text=True, timeout=120)
            assert result.returncode == 0, result.stderr
            committed = json.loads((ROOT / 'installation.json').read_text())
            assert committed['image'] == candidate
            assert not committed.get('pendingImage') and not committed.get('leaseOwner')
            health = json.loads(urllib.request.urlopen('http://127.0.0.1:48152/health').read())
            assert health['runtimeId'] == digit * 64 and not health['maintenance']
            assert identity == (ROOT / 'data/installation-relay/identity.json').read_bytes()
            wait_for(lambda: request(base + '/chats', cookie=cookie)[0] == [])
        print('Container: approved update, wrong-runtime candidate retry, real deployment lease and retained identity passed')
    finally:
        pid = ROOT / 'manager.pid'
        if pid.exists():
            os.kill(int(pid.read_text()), signal.SIGTERM)
        beacon.terminate()
        beacon.wait(timeout=15)
        mailbox.shutdown()


if __name__ == '__main__':
    main(sys.argv[1])
