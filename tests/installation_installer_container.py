"""Public shell installer in a disposable Linux container, without KVM.

Docker lifecycle commands and loopback download transport are adapted. The real
Compose parser validates the generated configuration on the controller. Claiming,
relay and S3 use real binaries, Postgres and Garage; no VM is booted.
"""
import http.server
import http.client
import json
import os
from pathlib import Path
import signal
import ssl
import stat
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

ROOT = Path('/fixture/installation')
ORIGIN = 'http://127.0.0.1:48151'
IMAGE = 'ghcr.io/leo91000/leo-agent-manager@sha256:' + '1' * 64
MAIL = []
R2_HOST = '1' * 32 + '.r2.cloudflarestorage.com'


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
    raise AssertionError('The real official service or installation did not become ready')


def run_installer(code=''):
    if os.environ.get('INSTALLER_VERIFY'):
        with urllib.request.urlopen(ORIGIN + '/install.sh') as response:
            script = response.read().decode()
    else:
        import hashlib
        checksum = hashlib.sha256(Path('deploy/installations/host.py').read_bytes()).hexdigest()
        script = Path('deploy/installations/install.sh').read_text().replace(
            '__LEO_OFFICIAL_ORIGIN__', repr(ORIGIN)).replace('__LEO_HOST_SHA256__', checksum)
    # Compressed swap is a host setting covered by compressed_swap_test.py.
    swaps = Path('/fixture/swaps')
    swaps.write_text('Filename Type Size Used Priority\n/dev/zram0 partition 1024 0 100\n')
    result = subprocess.run(['bash', '-s', '--', '--claim-code', code], input=script,
                            capture_output=True, text=True, timeout=240, env={
                                **os.environ, 'LEO_INSTALLATION_ROOT': str(ROOT),
                                'PATH': '/fixture/bin:' + os.environ['PATH'],
                                'LEO_PROC_SWAPS': str(swaps),
                            })
    assert code == '' or code not in result.stdout + result.stderr, 'Claim leaked in output'
    return result


def main(mode):
    if mode == 'compose':
        config = json.loads(subprocess.check_output([
            '/usr/local/bin/docker-compose', '--project-directory', str(ROOT),
            '-f', str(ROOT / 'compose.json'), 'config', '--format', 'json',
        ], env={**os.environ, 'LEO_INSTALLATION_CLAIM_CODE': 'a' * 64}))
        services = config['services']
        assert services['garage']['healthcheck']['test'] == ['CMD', '/garage', 'bucket', 'info', 'leo-disks']
        assert services['manager']['environment']['LEO_INSTALLATION_CLAIM_CODE'] == 'a' * 64
        assert services['manager']['user'] == '1000:1000'
        assert services['manager']['depends_on']['garage']['condition'] == 'service_healthy'
        assert services['manager']['depends_on']['runner']['condition'] == 'service_healthy'
        assert not any(service.get('ports') for service in services.values())
        for name in ('data', 'home', 'workspaces'):
            assert (ROOT / name).stat().st_uid == 1000
        print('Container: real Docker Compose parser, healthcheck, claim substitution, dependencies and uid passed')
        return

    # Devices are inert character placeholders: no ioctl or VM boot is performed.
    Path('/dev/net').mkdir(exist_ok=True)
    for device in ('/dev/kvm', '/dev/net/tun', '/dev/fuse'):
        if not Path(device).exists():
            os.mknod(device, stat.S_IFCHR | 0o600, os.makedev(1, 3))
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
if 'up' in args and os.environ.get('INSTALLER_VERIFY'):
    pid_file = root / 'manager.pid'
    if pid_file.exists():
        os.kill(int(pid_file.read_text()), signal.SIGTERM)
        time.sleep(1)
    code = ''
    claim = root / 'claim.env'
    if claim.exists():
        code = dict(line.split('=', 1) for line in claim.read_text().splitlines()).get('LEO_INSTALLATION_CLAIM_CODE', '')
    env = {**os.environ, 'DATA_DIR': str(root / 'data'), 'AGENT_HOME': str(root / 'home'),
           'WORKSPACE_ROOTS': str(root / 'workspaces'), 'WORKER_ENABLED': 'false', 'NODE_ENV': 'test',
           'HOST': '127.0.0.1', 'PORT': '48152', 'LEO_OFFICIAL_ORIGIN': 'http://127.0.0.1:48151',
           'LEO_INSTALLATION_CLAIM_CODE': code}
    # Synthetic child credentials only; isolate from any controller agent broker.
    for name in list(env):
        if name.startswith(('LEO_AUTH_', 'CODEX_', 'OPENAI_', 'ANTHROPIC_')):
            env.pop(name)
    log = open(root / 'manager.log', 'ab')
    process = subprocess.Popen(['/repo/target/debug/leo', 'serve'], env=env, stdout=log, stderr=log, start_new_session=True)
    pid_file.write_text(str(process.pid))
if 'exec' in args and os.environ.get('INSTALLER_VERIFY'):
    try:
        urllib.request.urlopen('http://127.0.0.1:48152/health', timeout=2)
    except Exception:
        sys.exit(1)
''')
    docker.chmod(0o755)
    # Production downloads must require HTTPS. This isolated loopback fixture has
    # no TLS server; adapt only its transport, without touching product behavior.
    curl = binaries / 'curl'
    curl.write_text('''#!/usr/bin/env python3
import subprocess, sys
args = sys.argv[1:]
assert args[args.index('--proto') + 1] == '=https'
assert args[-1] == 'http://127.0.0.1:48151/install/host.py'
args[args.index('--proto') + 1] = '=http'
sys.exit(subprocess.run(['/usr/bin/curl', *args]).returncode)
''')
    curl.chmod(0o755)
    for name, contents in {
        'uname': '#!/bin/sh\nif [ "$1" = -s ]; then echo "${INSTALLER_OS:-Linux}"; else echo "${INSTALLER_ARCH:-x86_64}"; fi\n',
        'df': '#!/bin/sh\necho "Filesystem 1024-blocks Used Available Capacity Mounted"\necho "fixture 67108864 0 ${INSTALLER_FREE_KB:-33554432} 0% /fixture"\n',
    }.items():
        binary = binaries / name
        binary.write_text(contents)
        binary.chmod(0o755)

    class Official(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            if self.path == '/install/release':
                self.wfile.write(json.dumps({'image': IMAGE}).encode())
            else:
                body = Path('deploy/installations/host.py').read_bytes()
                if os.environ.get('INSTALLER_TAMPER_HOST'):
                    body += b'\n# mismatched download\n'
                self.wfile.write(body)

        def log_message(self, *_):
            pass

    class Mailbox(Official):
        def do_POST(self):
            MAIL.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{}')

    if mode == 'prepare':
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 48151), Official)
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
            for device, message in [('/dev/kvm', 'KVM'), ('/dev/net/tun', 'TUN'), ('/dev/fuse', 'FUSE')]:
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
            os.chown(ROOT / 'data/storage-s3.json', 0, 0)
            result = run_installer()
            assert result.returncode == 0, result.stderr
            assert (ROOT / 'data/storage-s3.json').stat().st_uid == 1000, 'Rerun must repair an interrupted storage chown'
            print('Container: KVM/TUN/FUSE diagnostics, automatic private Garage configuration passed')
        finally:
            server.shutdown()
        return

    mailbox = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Mailbox)
    threading.Thread(target=mailbox.serve_forever, daemon=True).start()
    tls_extensions = Path('/fixture/external.ext')
    tls_extensions.write_text(f'basicConstraints=critical,CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,DNS:{R2_HOST}\n')
    for command in [
        ['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
         '-keyout', '/fixture/ca.key', '-out', '/usr/local/share/ca-certificates/leo-fixture.crt',
         '-days', '1', '-subj', '/CN=Leo fixture CA', '-addext', 'basicConstraints=critical,CA:TRUE'],
        ['openssl', 'req', '-new', '-newkey', 'rsa:2048', '-nodes',
         '-keyout', '/fixture/external.key', '-out', '/fixture/external.csr', '-subj', '/CN=localhost'],
        ['openssl', 'x509', '-req', '-in', '/fixture/external.csr',
         '-CA', '/usr/local/share/ca-certificates/leo-fixture.crt', '-CAkey', '/fixture/ca.key',
         '-CAserial', '/fixture/ca.srl', '-CAcreateserial', '-out', '/fixture/external.crt',
         '-days', '1', '-extfile', str(tls_extensions)],
    ]:
        subprocess.run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['update-ca-certificates'], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    with open('/etc/hosts', 'a') as hosts:
        hosts.write(f'\n127.0.0.1 {R2_HOST}\n')
    env = {**os.environ,
           'LEO_OFFICIAL_DATABASE_URL': os.environ['LEO_OFFICIAL_TEST_DATABASE_URL'],
           'LEO_OFFICIAL_ORIGIN': ORIGIN, 'LEO_OFFICIAL_LISTEN': '127.0.0.1:48151',
           'LEO_OFFICIAL_WEB_DIR': '/repo/dist', 'LEO_INSTALLATION_IMAGE': IMAGE,
           'LEO_OFFICIAL_EMAIL_FROM': 'fixture@example.test', 'LEO_OFFICIAL_EMAIL_KEY': 'fixture-only',
           'LEO_OFFICIAL_EMAIL_ENDPOINT': f'http://127.0.0.1:{mailbox.server_port}/emails'}
    os.environ['INSTALLER_VERIFY'] = '1'
    os.environ['AWS_CA_BUNDLE'] = '/etc/ssl/certs/ca-certificates.crt'
    official = subprocess.Popen(['/repo/target/debug/leo-official'], env=env,
                                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    def official_ready():
        if official.poll() is not None:
            raise RuntimeError('Fixture official service exited: ' + official.communicate()[1].decode())
        return request('/api/account/options')[0] is not None

    external = None
    proxy = None
    try:
        wait_for(official_ready)
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
        storage, _ = request(base + '/settings/storage', cookie=cookie)
        assert storage['configured'] and storage['integrated']
        assert 'secretAccessKey' not in storage and 'accessKeyId' not in storage
        print('Container: real claim and relay passed', flush=True)
        # This uses the real SDK and integrated credentials, including write/read/delete.
        checked, _ = request(base + '/settings/storage/check', {}, cookie, csrf)
        assert checked == {'ok': True}
        print('Container: Garage write/read/delete passed', flush=True)
        (ROOT / 'data/storage-s3.json.integrated').write_bytes((ROOT / 'data/storage-s3.json').read_bytes())
        # External providers are replaced at their S3 HTTP seam, behind valid TLS.
        from moto.server import ThreadedMotoServer
        import boto3
        external = ThreadedMotoServer(ip_address='127.0.0.1', port=0, verbose=False)
        external.start()
        host, port = external.get_host_and_port()
        external_client = boto3.client('s3', endpoint_url=f'http://{host}:{port}',
                                      region_name='us-east-1', aws_access_key_id='external-fixture',
                                      aws_secret_access_key='external-fixture-secret')
        external_client.create_bucket(Bucket='leo-external')
        external_client.put_public_access_block(Bucket='leo-external', PublicAccessBlockConfiguration={
            'BlockPublicAcls': True, 'IgnorePublicAcls': True,
            'BlockPublicPolicy': True, 'RestrictPublicBuckets': True})

        class ExternalS3(http.server.BaseHTTPRequestHandler):
            def forward(self):
                r2 = self.headers.get('Host', '').startswith(R2_HOST)
                unsupported = any(query in self.path for query in ('?acl', '?publicAccessBlock', '?object-lock', '?versions'))
                if r2 and (unsupported or self.headers.get('x-amz-server-side-encryption')):
                    self.send_response(501)
                    self.send_header('Content-Type', 'application/xml')
                    self.end_headers()
                    self.wfile.write(b'<Error><Code>NotImplemented</Code></Error>')
                    return
                upstream = http.client.HTTPConnection(host, port)
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                # Moto derives bucket routing from Host; the TLS proxy owns provider hostnames.
                headers = {name: value for name, value in self.headers.items() if name.lower() != 'host'}
                headers['Host'] = f'{host}:{port}'
                upstream.request(self.command, self.path, body=body, headers=headers)
                response = upstream.getresponse()
                data = response.read()
                self.send_response(response.status)
                for name, value in response.getheaders():
                    if name.lower() not in ('transfer-encoding', 'connection', 'server', 'date', 'content-length'):
                        self.send_header(name, value)
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(data)
                upstream.close()

            do_GET = do_PUT = do_DELETE = do_POST = do_HEAD = forward

            def log_message(self, *_):
                pass

        proxy = http.server.ThreadingHTTPServer(('127.0.0.1', 0), ExternalS3)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain('/fixture/external.crt', '/fixture/external.key')
        proxy.socket = context.wrap_socket(proxy.socket, server_side=True)
        threading.Thread(target=proxy.serve_forever, daemon=True).start()
        endpoint = f'https://localhost:{proxy.server_port}'
        saved, _ = request(base + '/settings/storage', {
            'bucket': 'leo-external', 'endpoint': endpoint, 'region': 'us-east-1',
            'accessKeyId': 'external-fixture', 'secretAccessKey': 'external-fixture-secret',
        }, cookie, csrf, method='PUT')
        assert saved['endpoint'] == endpoint and not saved['integrated']
        assert 'secretAccessKey' not in saved and 'accessKeyId' not in saved
        checked, _ = request(base + '/settings/storage/check', {}, cookie, csrf)
        assert checked == {'ok': True}
        # Public relayed settings plus the existing disk publication seam exercise
        # an old disk against real Garage after changing the default to HTTPS S3.
        storage_test = Path('/fixture/storage-continuity')
        assert storage_test.is_file() and os.access(storage_test, os.X_OK), 'Mount the executable identified by Cargo'
        subprocess.run([str(storage_test), '--ignored', '--exact',
                        'existing_disks_keep_publishing_reading_and_purging_on_their_original_storage',
                        '--nocapture'], check=True, env={
                            **os.environ,
                            'LEO_INSTALLER_TEST_STORAGE_CONFIG': str(ROOT / 'data/storage-s3.json.integrated'),
                            'LEO_INSTALLER_TEST_EXTERNAL_ENDPOINT': endpoint,
                        })
        print('Container: retained disk publication, remote reads and purge on original Garage passed', flush=True)
        r2_endpoint = f'https://{R2_HOST}:{proxy.server_port}'
        r2_settings = {
            'bucket': 'leo-external', 'endpoint': r2_endpoint, 'region': 'auto',
            'accessKeyId': 'external-fixture', 'secretAccessKey': 'external-fixture-secret',
        }
        try:
            request(base + '/settings/storage', r2_settings, cookie, csrf, method='PUT')
            raise AssertionError('R2 must require confirmation of provider-side privacy and retention')
        except AssertionError as error:
            assert 'HTTP 400' in str(error) and 'Confirm' in str(error), str(error)
        r2_settings['privateBucketConfirmed'] = True
        saved, _ = request(base + '/settings/storage', r2_settings, cookie, csrf, method='PUT')
        assert saved['endpoint'] == r2_endpoint
        checked, _ = request(base + '/settings/storage/check', {}, cookie, csrf)
        assert checked == {'ok': True}
        endpoint = r2_endpoint
        identity = (ROOT / 'data/installation-relay/identity.json').read_bytes()
        credentials = (ROOT / 'garage.env').read_bytes()
        # Simulate an interruption between the atomic S3 write and its chown.
        os.chown(ROOT / 'data/storage-s3.json', 0, 0)
        result = run_installer(claim['code'])
        assert result.returncode == 0, result.stderr
        assert identity == (ROOT / 'data/installation-relay/identity.json').read_bytes()
        assert credentials == (ROOT / 'garage.env').read_bytes()
        assert not (ROOT / 'claim.env').exists()
        assert (ROOT / 'data/storage-s3.json').stat().st_mode & 0o777 == 0o600
        assert (ROOT / 'data/storage-s3.json').stat().st_uid == 1000
        session, _ = request('/api/account/session', cookie=cookie)
        assert len(session['installations']) == 1
        wait_for(lambda: request(base + '/settings/storage', cookie=cookie)[0]['endpoint'] == endpoint)
        print('Container: real claim, relay, Garage write/read/delete, external S3/R2 settings and idempotent rerun passed')
    finally:
        pid = ROOT / 'manager.pid'
        if pid.exists():
            os.kill(int(pid.read_text()), signal.SIGTERM)
        official.terminate()
        official.wait(timeout=15)
        mailbox.shutdown()
        if proxy:
            proxy.shutdown()
            proxy.server_close()
        if external:
            external.stop()


if __name__ == '__main__':
    main(sys.argv[1])
