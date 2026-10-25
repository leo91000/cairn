"""Synthetic Docker workload executed inside the disposable native enclave."""
import json
import http.client
import os
import pwd
import shutil
import shlex
import socket
from pathlib import Path
import subprocess
import sys
import time
import threading
import urllib.request
import urllib.error

ALPINE = 'alpine:3.22.2'
POSTGRES = 'postgres:17-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24'
REDIS = 'redis:8.2.2-alpine'
PORT = 57431


def command(*args, success=True):
    result = subprocess.run(args, capture_output=True, text=True, timeout=120)
    if success and result.returncode:
        raise AssertionError('Command failed: ' + ' '.join(args) + '\n' + result.stderr)
    if not success:
        assert result.returncode != 0, 'Restricted Docker operation succeeded: ' + ' '.join(args)
    return result.stdout.strip()


def http_command(body, port=80):
    # Alpine's minimal BusyBox omits httpd; nc is included in the base image.
    # Keep the fixture self-contained and avoid package downloads at startup.
    response = f'HTTP/1.1 200 OK\r\nContent-Length: {len(body.encode())}\r\nConnection: close\r\n\r\n{body}'
    return ['sh', '-c', f"while :; do printf '%s' {shlex.quote(response)} | nc -l -p {port}; done"]


def wait_http(expected):
    deadline = time.monotonic() + 20
    while True:
        try:
            assert urllib.request.urlopen('http://127.0.0.1:' + str(PORT), timeout=2).read().strip() == expected
            return
        except (OSError, urllib.error.URLError):
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.1)


def wait_compose_services(compose_path):
    # Detached startup does not imply application readiness. In particular,
    # Postgres can already accept connections while resumed Redis is starting.
    deadline = time.monotonic() + 45
    probes = [('db', ['pg_isready']), ('redis', ['redis-cli', 'ping'])]
    pending = dict(probes)
    diagnostics = {}
    while pending:
        for service, probe in list(pending.items()):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError('Compose services did not become ready: ' + repr(diagnostics))
            try:
                ready = subprocess.run(['docker', 'compose', '-p', 'native', '-f', str(compose_path),
                                        'exec', '-T', service, *probe],
                                       capture_output=True, text=True, timeout=min(10, remaining))
            except subprocess.TimeoutExpired:
                diagnostics[service] = 'readiness command timed out'
                continue
            expected = 'accepting connections' in ready.stdout if service == 'db' else ready.stdout.strip() == 'PONG'
            if ready.returncode == 0 and expected:
                del pending[service]
                diagnostics.pop(service, None)
            else:
                diagnostics[service] = (ready.stdout + ready.stderr)[-4096:]
        if pending:
            time.sleep(min(0.2, max(0, deadline - time.monotonic())))


def bind_race(image):
    root = Path.cwd() / '.docker-race'
    root.mkdir(exist_ok=True)
    allowed = root / 'allowed'
    allowed.mkdir(exist_ok=True)
    (allowed / 'daemon.json').write_text('allowed')
    source = root / 'source'
    if source.is_symlink():
        source.unlink()
    if not source.exists():
        allowed.rename(source)
    stop = threading.Event()

    def swap():
        while not stop.is_set():
            source.rename(allowed)
            source.symlink_to('/run/cairn-docker', target_is_directory=True)
            time.sleep(0.001)
            source.unlink()
            allowed.rename(source)
            time.sleep(0.001)

    thread = threading.Thread(target=swap)
    thread.start()
    try:
        for _ in range(12):
            result = subprocess.run(['docker', 'run', '--rm', '-v', str(source) + ':/probe:ro', image, 'cat', '/probe/daemon.json'], capture_output=True, text=True, timeout=30)
            assert result.returncode != 0 or result.stdout.strip() == 'allowed', 'Bind race exposed private runtime metadata.'
    finally:
        stop.set()
        thread.join()


def raw_rejected(path, body):
    class UnixConnection(http.client.HTTPConnection):
        def connect(self):
            self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.sock.settimeout(self.timeout)
            self.sock.connect(os.environ['DOCKER_HOST'].removeprefix('unix://'))

    connection = UnixConnection('localhost', timeout=30)
    try:
        connection.request('POST', path, body.encode(), {'Content-Type': 'application/json'})
        response = connection.getresponse()
        message = response.read(65536).decode(errors='replace')
        assert 400 <= response.status < 500, f'Raw API control bypass: {path}: {body}: {response.status}: {message}'
    finally:
        connection.close()


def raw_api_rejections():
    options = '{"device":"/run/cairn-docker","type":"none","o":"bind"}'
    for field in ('driverOpts', 'DRIVEROPTS', 'DriverOptſ'):
        raw_rejected('/volumes/create', '{"Name":"native-case-escape","Driver":"local","' + field + '":' + options + '}')
    raw_rejected('/volumes/create', '{"Name":"native-case-escape","DriverOpts":' + options + ',"DriverOpts":{}}')
    assert 'native-case-escape' not in command('docker', 'volume', 'ls', '-q').splitlines()
    raw_rejected('/containers/create', json.dumps(dict(Image=ALPINE, hostConfig=dict(Binds=['/:/host']))))
    raw_rejected('/containers/create', '{"Image":"' + ALPINE + '","HostConfig":{"Privileged":true},"HostConfig":{}}')
    container = command('docker', 'run', '-d', ALPINE, 'sleep', '300')
    try:
        raw_rejected('/containers/' + container + '/exec', '{"Cmd":["true"],"privileged":true}')
    finally:
        command('docker', 'rm', '-f', container)


def main():
    phase = sys.argv[1]
    metrics = dict(phase=phase, pulls=[], timings={}, workloadCgroupCpuMs={}, loadAverage={},
                   cpuScope='Whole conversation cgroup: client, daemon, helpers and any running services; aggregate CPU, not exclusive per-process attribution.')
    membership = next(line.removeprefix('0::') for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    cpu_file = Path('/sys/fs/cgroup') / membership.lstrip('/') / 'cpu.stat'

    def charged_cpu_ms():
        counters = dict(line.split() for line in cpu_file.read_text().splitlines())
        return int(counters['usage_usec']) / 1000

    def timed(name, *args):
        cpu_before = charged_cpu_ms()
        load_before = os.getloadavg()
        start = time.perf_counter()
        result = command(*args)
        metrics['timings'][name] = (time.perf_counter() - start) * 1000
        metrics['workloadCgroupCpuMs'][name] = charged_cpu_ms() - cpu_before
        metrics['loadAverage'][name] = dict(before=load_before, after=os.getloadavg())
        return result

    status = dict(line.split(':', 1) for line in Path('/proc/self/status').read_text().splitlines() if ':' in line)
    assert status['NoNewPrivs'].strip() == '1'
    assert int(status['CapBnd'].strip(), 16) == 0
    assert os.getuid() == 1000
    assert len(status['NSpid'].split()) == 1, 'procfs exposes a parent PID namespace.'
    mounts = [line.split() for line in Path('/proc/self/mountinfo').read_text().splitlines()]
    for protected in ('/proc/sys', '/proc/sysrq-trigger', '/sys', '/sys/fs/cgroup'):
        matches = [fields for fields in mounts if fields[4] == protected]
        assert matches and 'ro' in matches[-1][5].split(','), protected
    assert not os.access('/proc/sys/net/ipv4/ip_forward', os.W_OK), 'Engine can change enclave network sysctls.'
    assert not Path('/run/docker.sock').exists()
    assert not Path('/var/run/docker.sock').exists()
    try:
        list(Path('/var/lib/cairn-docker').iterdir())
    except PermissionError:
        pass
    else:
        raise AssertionError('Engine could read raw daemon metadata.')
    assert not Path('/run/cairn-docker/daemon.started').exists(), 'Daemon started before its first Docker command.'
    assert pwd.getpwuid(1000).pw_name == 'node'
    socket.getaddrinfo('registry-1.docker.io', 443)
    try:
        urllib.request.urlopen('https://registry-1.docker.io/v2/', timeout=20)
    except urllib.error.HTTPError as error:
        assert error.code == 401
    command('docker', '--version')
    command('docker', 'buildx', 'version')
    command('docker', 'compose', 'version')
    command('docker', 'context', 'ls')
    command('docker', 'run', '--help')
    assert not Path('/run/cairn-docker/daemon.started').exists(), 'Client-only command launched the daemon.'
    timed('firstDaemonMs', 'docker', 'info')
    info = json.loads(command('docker', 'info', '--format', '{{json .}}'))
    assert 'name=rootless' in info['SecurityOptions'], info['SecurityOptions']
    metrics['cgroupDriver'] = info['CgroupDriver']
    timed('warmDaemonMs', 'docker', 'info')
    probe = Path.home() / '.native-docker-probe'
    shutil.copyfile('/opt/cairn-native-tools/bin/native-docker-probe', probe)
    probe.chmod(0o755)

    def volume_probe(mode):
        return command('docker', 'run', '--rm', '-v', str(probe) + ':/probe:ro', '-v', 'persistent:/v', ALPINE, '/probe', mode, '/v')
    forbidden_commands = [
        ('docker', 'run', '--privileged', ALPINE, 'true'),
        ('docker', 'run', '--cap-add=SYS_ADMIN', ALPINE, 'true'),
        ('docker', 'run', '--pid=host', ALPINE, 'true'),
        ('docker', 'run', '-v', '/:/host', ALPINE, 'true'),
        ('docker', 'run', '-v', '/run/cairn-docker:/host', ALPINE, 'true'),
        ('docker', 'volume', 'create', '--opt', 'type=none', '--opt', 'o=bind', '--opt', 'device=/', 'escape'),
    ]
    for forbidden in forbidden_commands if phase != 'other' else []:
        # Pull the fixture before create-time policy checks, keeping pull costs
        # separate from daemon startup and rejection measurements below.
        if phase == 'fresh' and not metrics['pulls']:
            for image in (ALPINE, POSTGRES, REDIS):
                cpu_before = charged_cpu_ms()
                load_before = os.getloadavg()
                elapsed = time.perf_counter()
                command('docker', 'pull', image)
                pull_ms = (time.perf_counter() - elapsed) * 1000
                pull_cpu_ms = charged_cpu_ms() - cpu_before
                metrics['pulls'].append(dict(image=image, ms=pull_ms, workloadCgroupCpuMs=pull_cpu_ms,
                    loadAverage=dict(before=load_before, after=os.getloadavg()),
                    digest=command('docker', 'image', 'inspect', '--format', '{{json .RepoDigests}}', image)))
        command(*forbidden, success=False)
    if phase != 'other':
        # A recursive home bind cannot unmask frozen credentials or skills,
        # even for container namespace root with ordinary Docker capabilities.
        command('docker', 'run', '--rm', '-v', '/home/node:/engine-home', ALPINE, 'sh', '-c',
                'test -r /engine-home/.codex/auth.json && '
                '! (printf forbidden >/engine-home/.codex/auth.json) && '
                '! touch /engine-home/.agents/skills/not-allowed')
    if phase == 'oom':
        command('docker', 'run', '-d', '--name', 'node-ceiling', '-v', str(probe) + ':/probe:ro', '-v', 'persistent:/v', ALPINE, '/probe', 'oom', '/v')
    elif phase == 'readonly':
        for name in ('create', 'json', 'prune'):
            command('docker', 'start', name, success=False)
            raw_rejected('/containers/' + name + '/start', '{}')
        command('docker', 'run', '--rm', '-v', str(Path.cwd()) + ':/workspace', ALPINE, 'true', success=False)
        command('docker', 'run', '--rm', '-v', str(Path.cwd()) + ':/workspace:ro', ALPINE,
                'sh', '-c', 'test -f /workspace/persisted && ! touch /workspace/forbidden')
    elif phase == 'other':
        assert not command('docker', 'volume', 'ls', '-q'), 'Other conversation can see volumes.'
        assert not command('docker', 'image', 'ls', '-q'), 'Other conversation can see images.'
        command('docker', 'pull', ALPINE)
        command('docker', 'run', '-d', '--name', 'other-http', '-p', str(PORT) + ':80', ALPINE,
                *http_command('other'))
        wait_http(b'other')
    else:
        context = Path('.docker-fixture')
        context.mkdir(exist_ok=True)
        dockerfile = context / 'Dockerfile'
        dockerfile.write_text('FROM ' + ALPINE + '\nRUN head -c 64 /dev/urandom > /cache-token\n')
        timed('buildMs', 'docker', 'buildx', 'build', '--load', '--progress=plain', '-t', 'cairn-cache', str(context))
        command('docker', 'tag', 'cairn-cache', 'cairn.local/fixture/cache:dev')
        command('docker', 'image', 'inspect', 'cairn.local/fixture/cache:dev')
        cached = command('docker', 'run', '--rm', 'cairn-cache', 'sha256sum', '/cache-token')
        if phase == 'fresh':
            (Path.home() / '.docker-cache-token').write_text(cached)
            command('docker', 'volume', 'create', 'persistent')
            command('docker', 'run', '--rm', '-v', 'persistent:/v', ALPINE, 'sh', '-c',
                    'echo durable >/v/marker; chown 999:999 /v/marker; chmod 600 /v/marker')
        else:
            assert cached == (Path.home() / '.docker-cache-token').read_text(), 'Build cache was lost after resume.'
        if phase == 'fresh':
            volume_probe('metadata-create')
        assert volume_probe('metadata-check') == 'metadata-preserved'
        metrics['volumeIo'] = json.loads(volume_probe('io'))
        if phase == 'fresh':
            bind_race(ALPINE)
            raw_api_rejections()
            # These are valid container names, not collection routes. Persist
            # writable binds and prove the later read-only turn refuses both
            # CLI and raw API activation instead of skipping saved controls.
            for name in ('create', 'json', 'prune'):
                command('docker', 'create', '--name', name, '-v', str(Path.cwd()) + ':/workspace', ALPINE, 'true')
        assert command('docker', 'run', '--rm', '-v', 'persistent:/v', ALPINE, 'cat', '/v/marker') == 'durable'
        assert command('docker', 'run', '--rm', '-v', 'persistent:/v', ALPINE, 'stat', '-c', '%u:%g:%a', '/v/marker') == '999:999:600'
        compose = dict(services=dict(
            db=dict(image=POSTGRES, environment=dict(POSTGRES_PASSWORD='fixture-only'), volumes=['database:/var/lib/postgresql/data']),
            redis=dict(image=REDIS),
            app=dict(image=ALPINE, ports=[str(PORT) + ':80'], command=http_command('conversation'))),
            volumes=dict(database={}))
        compose_path = context / 'compose.json'
        compose_path.write_text(json.dumps(compose))
        command('docker', 'compose', '-p', 'native', '-f', str(compose_path), 'up', '-d')
        wait_compose_services(compose_path)
        assert command('docker', 'compose', '-p', 'native', '-f', str(compose_path), 'exec', '-T', 'redis', 'redis-cli', 'ping') == 'PONG'
        wait_http(b'conversation')
        # Same agent-visible bind paths reach the daemon; no host path rewriting.
        assert command('docker', 'run', '--rm', '-v', str(Path.cwd()) + ':/workspace', ALPINE, 'cat', '/workspace/persisted') == 'changed'
    (Path.home() / '.docker-test-result.json').write_text(json.dumps(metrics))


if __name__ == '__main__':
    main()
