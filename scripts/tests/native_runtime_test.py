#!/usr/bin/env python3
"""Disposable real systemd/directory-quota/local-disk qualification; never a personal host.

Invoked through scripts/ci/test-native-runtime.mjs with isolated fixture credentials.
The loopback bridge exists only in this fixture; production remains Unix-only.
"""
import base64
from contextlib import contextmanager
import http.client
import http.server
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import random
import resource
import select
import shutil
import shlex
import stat
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
import zipfile


def controller_result(value):
    """Normalize a controller wait result using the runner client's contract."""
    if type(value) is int:
        code = value
        finalization_failed = False
    elif isinstance(value, dict):
        code = value.get('StatusCode')
        finalization_failed = value.get('FinalizationFailed') is True and code == 70
    else:
        raise ValueError('Invalid controller result.')
    if type(code) is not int or not 0 <= code <= 255:
        raise ValueError('Invalid controller exit status.')
    return {'StatusCode': code, 'FinalizationFailed': finalization_failed}


def read_controller_result(path):
    try:
        value = json.loads(path.read_text())
    except json.JSONDecodeError as error:
        raise ValueError('Invalid controller exit record.') from error
    return controller_result(value)


def match_controller_result(path, wait_result):
    recorded = read_controller_result(path)
    assert controller_result(wait_result) == recorded, 'Wait result differs from the saved controller result.'
    return recorded


def require_finalization_failure(path, wait_result):
    proof = match_controller_result(path, wait_result)
    assert proof == {'StatusCode': 70, 'FinalizationFailed': True}, 'Missing controller flush-failure evidence.'
    return proof


def owned_namespace_pid(group, namespace_pid, proc=Path('/proc')):
    """Resolve a workload PID only inside its attempt's host cgroup."""
    assert group.startswith('/') and group != '/', 'Missing attempt cgroup.'
    matches = []
    for task in proc.iterdir():
        if not task.name.isdecimal():
            continue
        try:
            membership = next((line[3:] for line in (task / 'cgroup').read_text().splitlines()
                               if line.startswith('0::')), None)
            if membership != group and not (membership or '').startswith(group + '/'):
                continue
            namespaces = next((line.split()[1:] for line in (task / 'status').read_text().splitlines()
                               if line.startswith('NSpid:')), [])
            if namespaces and int(namespaces[-1]) == namespace_pid:
                matches.append(int(task.name))
        except (FileNotFoundError, ProcessLookupError):
            continue
    assert len(matches) == 1, f'Expected one owned process for namespace PID {namespace_pid}; found {matches} in {group}'
    return matches[0]


def retained_workspace(state, run, project=None):
    workspace = state / 'native-data' / run / 'data/workspaces'
    return workspace / project if project else workspace


def running_local_storage(api, run):
    def resumed():
        status = api('/disks/' + run + '/storage-status')
        return status if status.get('paused') is False else None
    return eventually(resumed, timeout=25)


def completed_local_disk(api, manager, run):
    status = api('/disks/' + run + '/storage-status')
    assert status['storage'] == 'local-directory-v1'
    assert status['availability'] == 'available'
    assert not status['paused']
    result = manager('inspect', run=run)
    assert result['storage'] == 'local-directory-v1'
    assert result['allocatedBytes'] > 0
    return result


@contextmanager
def native_fixture(filesystem='ext4'):
    # A single quota filesystem for ordinary environment directories. Control
    # receipts live on the host filesystem and remain writable at inode ENOSPC.
    root = Path(tempfile.mkdtemp(prefix='cairn-native-smoke-', dir='/var/tmp'))
    root.chmod(0o700)
    pool = root / 'state/native-data'
    pool.mkdir(parents=True, mode=0o700)
    image = root / f'native-data.{filesystem}'
    mounted = False
    try:
        with image.open('wb') as stream:
            stream.truncate(12 * 1024**3)
        if filesystem == 'xfs':
            command('mkfs.xfs', '-q', '-f', '-m', 'crc=1,reflink=1', '-i', 'projid32bit=1', str(image))
        else:
            command('mkfs.ext4', '-q', '-F', '-O', 'project,quota', '-E', 'quotatype=prjquota', str(image))
        command('mount', '-o', 'loop,prjquota', str(image), str(pool))
        mounted = True
        yield root
    finally:
        # Never recursively erase a fixture with an uncertain live consumer.
        # Failure retains the owned backing image and an explicit non-green result.
        if mounted:
            command('umount', str(pool))
        mounts = Path('/proc/self/mountinfo').read_text()
        assert not any(line.split()[4].startswith(str(root) + '/') for line in mounts.splitlines()), 'Fixture still owns mounts; retained for diagnosis.'
        shutil.rmtree(root)


SPEC = importlib.util.spec_from_file_location('native_host', Path(__file__).resolve().parents[2] / 'deploy/nodes/native-host.py')
host = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(host)
SECRET = 'synthetic-generated-secret-never-persist'


class FixtureDiskReservations:
    """Remember the manager's accepted allowances across turns and restarts."""

    def __init__(self):
        self.retained = {}

    def reserve(self, run, minimum_mib):
        allowance = max(self.retained.get(run, 0), minimum_mib)
        self.retained[run] = allowance
        return allowance

    def grow(self, api, run, attempt, allowance_mib):
        api('/disk-growth', dict(protocol=1, grants=[dict(attemptId=attempt, allowanceMiB=allowance_mib, reason='granted')]))
        self.reserve(run, allowance_mib)


def redact_fixture_diagnostics(text, token, context):
    for secret in (SECRET, token, context.get('grant')):
        if secret:
            text = text.replace(secret, '[fixture-secret]')
    return text


def command(*args, **options):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=60, **options)


def qualify_administrator_network(binary, docker_tools):
    arguments = [sys.executable, str(Path(__file__).with_name('native_administrator_network.py')),
                 str(binary), '--docker-tools', str(docker_tools)]
    try:
        result = subprocess.run(arguments, capture_output=True, text=True, timeout=150)
        output = (result.stdout or '') + (result.stderr or '')
        passed = any(line.startswith('PASS cgroup ancestry,') for line in result.stdout.splitlines())
        if result.returncode or not passed:
            raise AssertionError('Administrator network qualification failed '
                                 f'(exit {result.returncode}, completion receipt={passed}):\n' + output[-16384:])
    except subprocess.TimeoutExpired as error:
        def decoded(value):
            return value.decode('utf-8', errors='replace') if isinstance(value, bytes) else (value or '')
        output = decoded(error.stdout) + decoded(error.stderr)
        raise AssertionError('Administrator network qualification exceeded its outer deadline:\n'
                             + output[-16384:]) from error


def eventually(check, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            result = check()
            if result:
                return result
        except (OSError, ValueError, urllib.error.URLError, http.client.HTTPException):
            pass
        time.sleep(0.1)
    raise AssertionError('Timed out waiting for native runtime evidence.')


def retained_health(api, run):
    def sample():
        health = api('/health', method='GET')
        if health.get('status') != 'ok':
            return None
        growth = health.get('diskGrowth') or {}
        retained = growth.get('retainedBytes', {})
        return health if retained.get(run, 0) > 0 else None

    # Health is a cached observation. Busy storage deliberately grants no
    # retained credit; wait for an idle sample and retain that exact snapshot.
    return eventually(sample)


# Identical workload for the direct host reference and the actual sandbox engine.
# Each run performs fsynced writes and reads the deterministic seeded asset.
MEASURE = '''import json,os,time
from pathlib import Path
root=Path.cwd()
start=time.perf_counter(); cpu=time.process_time()
for i in range(128):
    with open(root/('text-%03d'%i),'wb') as f:
        f.write((b'cairn native storage fixture\\n'*160)[:4096]); f.flush(); os.fsync(f.fileno())
write={'wallMs':(time.perf_counter()-start)*1000,'cpuMs':(time.process_time()-cpu)*1000}
reads=[]
for cold in [True,False]:
    with open(root/'asset','rb') as f:
        if cold: os.posix_fadvise(f.fileno(),0,0,os.POSIX_FADV_DONTNEED)
        start=time.perf_counter(); cpu=time.process_time(); data=f.read()
    assert len(data)==8*1024*1024
    reads.append({'wallMs':(time.perf_counter()-start)*1000,'cpuMs':(time.process_time()-cpu)*1000})
print(json.dumps({'write':write,'readColdHint':reads[0],'readWarm':reads[1]}))
'''


class OfflineFixtureMaster(http.server.BaseHTTPRequestHandler):
    """Offer synthetic, scoped commands over the installed node's real relay."""
    def log_message(self, *_):
        pass

    def respond(self, status, body=b'', headers=None):
        self.send_response(status)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def json_response(self, payload):
        self.respond(200, json.dumps(payload).encode(), {'Content-Type': 'application/json'})

    def do_POST(self):
        length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(length)
        if self.path == '/internal/nodes/heartbeat':
            self.server.heartbeat_seen.set()
        elif self.path == '/internal/nodes/poll':
            try:
                return self.json_response(self.server.commands.get(timeout=0.5))
            except queue.Empty:
                return self.json_response(None)
        elif self.path == '/internal/nodes/reply':
            frame = json.loads(body)
            reply = self.server.replies[frame['id']]
            if 'status' in frame:
                reply['status'] = frame['status']
            if frame.get('done'):
                reply['done'].set()
            return self.json_response({})
        elif self.path.startswith('/internal/node-workspace/'):
            from native_connector_benchmark import SKILL_HIT, SKILL_KEY

            attempt, operation = self.path.removeprefix('/internal/node-workspace/').split('/')
            fixture = self.server.preparations.get(attempt)
            if fixture and operation == 'plan':
                return self.json_response(fixture['descriptor'])
            if fixture and operation == 'lease':
                return self.json_response({'remainingMs': fixture['lease']})
            selection = json.loads(body) if fixture and operation == 'seed' else {}
            if selection.get('kind') == 'initial':
                offered = selection.get('nativeSkillCache')
                omit_workspace = selection.get('workspace') is False and bool(fixture['descriptor'].get('projectArchives'))
                offer = self.server.seed_offer(attempt, omit_workspace) if isinstance(offered, list) else {}
                key = offer.get('key')
                hit = bool(key and key in offered)
                payload = fixture['streams'][(omit_workspace, hit)]
                headers = {}
                if key:
                    headers[SKILL_KEY] = key
                if offer.get('allocation') is not None:
                    headers['x-cairn-seed-allocation'] = str(offer['allocation'])
                if hit:
                    headers[SKILL_HIT] = '1'
                fixture['observed'] = dict(cacheSupported=isinstance(offered, list), cacheHit=hit,
                                           workspaceOmitted=omit_workspace, encodedBytes=len(payload),
                                           managerOfferMs=offer.get('elapsedMs'))
                return self.respond(200, payload, headers)
        self.respond(503)


def main():
    if os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') != '1' or os.geteuid() != 0:
        raise RuntimeError('Requires root on an explicitly designated disposable host.')
    binary = Path(sys.argv[1]).resolve(strict=True)
    qualifier = Path(sys.argv[2]).resolve(strict=True)
    legacy_binary = Path(sys.argv[3]).resolve(strict=True)
    xfs_smoke = os.environ.get('CAIRN_NATIVE_XFS_SMOKE') == '1'
    connector_reference = os.environ.get('CAIRN_NATIVE_CONNECTOR_REFERENCE') == '1'
    active_binary = legacy_binary
    legacy_phase = True
    identifier = str(uuid.uuid4())
    user = 'cairn-smoke-' + identifier[:8]
    supervisor = 'cairn-native-smoke-' + identifier + '.service'
    token = 'synthetic-controller-credential'
    attempts, runs, reserved_users = [], [], []
    mcp_servers = {}
    metrics = dict(host=dict(kernel=platform.release(), machine=platform.machine(), cpuCount=os.cpu_count(),
                            cpu=Path('/proc/cpuinfo').read_text().split('model name')[1].splitlines()[0],
                            memory=Path('/proc/meminfo').read_text().splitlines()[0],
                            loadAverageStart=os.getloadavg(), cpuTicksStart=Path('/proc/stat').read_text().splitlines()[0]),
                   conditions='Disposable hosted Linux VM; local disks and synthetic credentials; no WAN latency. '
                              'Cold file reads use advisory POSIX_FADV_DONTNEED; controller restart clears userspace cache.',
                   workload='Seeded random42 8MiB asset +128 fsynced 4KiB text writes, per-process wall/CPU milliseconds.',
                   resources=dict(nativeCpu=1, nativeMemoryMiB=512, nativeDiskMiB=256,
                                  reference='Host root process without workload cgroup limits',
                                  native='Unprivileged systemd workload, retained directories on dedicated ext4 project quotas; per-attempt IO 64MiB/s',
                                  build='Cargo development profile, debug info disabled'))

    def progress(phase):
        # A hosted runner may terminate before artifact upload. Keep bounded
        # phase/resource evidence in its logs without printing plans or grants.
        memory = dict(line.split(':', 1) for line in Path('/proc/meminfo').read_text().splitlines())
        sample = dict(phase=phase, monotonicSeconds=time.monotonic(),
                      availableMemory=memory['MemAvailable'].strip(), loadAverage=os.getloadavg(),
                      fixtureMaxRssKiB=resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
                      freeDiskBytes=shutil.disk_usage('/var/tmp').free)
        membership = next((line.removeprefix('0::') for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::')), None)
        if membership is not None:
            group = Path('/sys/fs/cgroup') / membership.lstrip('/')
            for field in ('memory.current', 'memory.max', 'memory.events'):
                path = group / field
                if path.is_file():
                    sample[field] = path.read_text().strip()
        metrics.setdefault('progress', []).append(sample)
        print('Native qualification: ' + json.dumps(sample), flush=True)

    progress('start')
    host.require_docker_host()
    with native_fixture('xfs' if xfs_smoke else 'ext4') as root:
        root.chmod(0o700)
        previous_install = host.INSTALL
        host.INSTALL = root / 'managed-tools'
        docker_tools = host.prepare_docker_tools()
        host.configure_docker_apparmor(docker_tools)
        # Exercise the distribution's file-capability alternative as well as
        # validating its ordinary installed setuid helpers above. Copies live
        # only on this explicitly disposable host, in the fixture's managed bin.
        for name, capability, bit in [('newuidmap', 'cap_setuid=ep', 1 << 7), ('newgidmap', 'cap_setgid=ep', 1 << 6)]:
            source = Path(shutil.which(name))
            target = docker_tools / 'bin' / name
            shutil.copyfile(source, target)
            target.chmod(0o755)
            command('setcap', capability, str(target))
            assert host.trusted_mapping_helper(target, bit)
        metrics['mappingHelpers'] = 'Root-owned distribution copies with only CAP_SETUID/CAP_SETGID effective, no setuid bit; host originals unchanged.'
        host.INSTALL = previous_install
        progress('host-tool-setup-complete')
        (root / 'disposable-fixture').touch()
        data, state = root / 'data', root / 'state'
        tools = root / 'versions/synthetic/tools'
        for directory in (data, state, tools / 'bin', data / 'runner-plans'):
            directory.mkdir(parents=True, mode=0o700, exist_ok=True)
        tools.chmod(0o755)
        (tools / 'bin').chmod(0o755)
        probe = tools / 'bin/native-docker-probe'
        probe_source = Path(__file__).with_name('native_docker_probe.c')
        command('cc', '-static', '-O2', '-Wall', '-Wextra', '-Werror', str(probe_source), '-o', str(probe))
        metrics['dockerProbe'] = dict(compiler=command('cc', '--version').stdout.splitlines()[0], flags='-static -O2 -Wall -Wextra -Werror', sourceSha256=hashlib.sha256(probe_source.read_bytes()).hexdigest())
        if os.environ.get('CAIRN_NATIVE_DOCKER_QUALIFY') == '1':
            progress('administrator-network-qualification')
            qualify_administrator_network(binary, docker_tools)
            metrics['administratorNetworkQualified'] = True

        (data / 'runner-secret').write_text(token)
        (tools / 'manifest.json').write_text('{}')
        (tools.parent / 'release.json').write_text(json.dumps({'runtimeId': 'synthetic', 'os': 'linux', 'arch': 'x86_64'}))
        private = root / 'owner-private'
        private.write_text('synthetic owner data')
        asset = random.Random(42).randbytes(8 * 1024 * 1024)
        reference = root / 'reference'
        reference.mkdir()
        (reference / 'asset').write_bytes(asset)
        metrics['reference'] = json.loads(command(sys.executable, '-c', MEASURE, cwd=reference).stdout)
        metrics['dockerVolumeHostReference'] = json.loads(command(str(probe), 'io', str(reference)).stdout)
        metrics['filesystem'] = command('stat', '-f', '-c', '%T', str(root)).stdout.strip()

        def api(path, body=None, method='POST', raw=False, timeout=120):
            connection = host.UnixControllerConnection(state / 'controller.sock', timeout=timeout)
            try:
                connection.request(method, path, body=json.dumps(body or {}).encode() if method != 'GET' else None,
                    headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
                response = connection.getresponse()
                payload = response.read()
                if response.status >= 400:
                    raise urllib.error.HTTPError('http://fixture' + path, response.status, 'Native request rejected', response.headers, None)
                return payload if raw else json.loads(payload)
            finally:
                connection.close()

        fixture_master = http.server.ThreadingHTTPServer(('127.0.0.1', 0), OfflineFixtureMaster)
        fixture_master.daemon_threads = True
        fixture_master.heartbeat_seen = threading.Event()
        fixture_master.commands = queue.Queue()
        fixture_master.replies = {}
        fixture_master.preparations = {}
        threading.Thread(target=fixture_master.serve_forever, daemon=True).start()
        node_identity = root / 'node-identity'
        node_identity.mkdir(mode=0o700)
        (node_identity / 'identity.json').write_text(json.dumps(dict(
            master='http://127.0.0.1:' + str(fixture_master.server_port),
            nodeId=str(uuid.uuid4()), token='synthetic-offline-node-token')))

        policy_generation = 0

        def storage_policy(policy):
            nonlocal policy_generation
            policy_generation += 1
            return api('/storage-policy', dict(generation=policy_generation, policy=policy))

        class Bridge(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def forward(self):
                connection = host.UnixControllerConnection(state / 'controller.sock', timeout=300)
                try:
                    length = int(self.headers.get('Content-Length', 0))
                    body = self.rfile.read(length) if length else None
                    connection.request(self.command, self.path, body=body, headers=dict(self.headers))
                    response = connection.getresponse()
                    payload = response.read()
                    self.send_response(response.status)
                    for key, value in response.getheaders():
                        if key.lower() not in ('content-length', 'transfer-encoding', 'connection'):
                            self.send_header(key, value)
                    self.send_header('Content-Length', str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except (OSError, http.client.HTTPException):
                    self.send_error(503)
                finally:
                    connection.close()
            do_GET = do_POST = do_DELETE = forward

        bridge = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Bridge)
        bridge.daemon_threads = True
        threading.Thread(target=bridge.serve_forever, daemon=True).start()
        helper = subprocess.Popen([str(qualifier), str(root), 'http://127.0.0.1:' + str(bridge.server_port)],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)

        def manager(op, **values):
            helper.stdin.write(json.dumps(dict(op=op, **values)) + '\n')
            helper.stdin.flush()
            if not select.select([helper.stdout], [], [], 300)[0]:
                raise AssertionError('Manager fixture timed out: ' + op)
            response = json.loads(helper.stdout.readline())
            assert response['ok'], 'Manager fixture failed: ' + op + ': ' + response.get('error', '')
            return response['value']

        fixture_master.seed_offer = lambda attempt, skip: manager('connector-seed-offer', attempt=attempt, skipWorkspace=skip)

        contexts = {}
        started_runs = set()
        vm_runs = set()
        disk_reservations = FixtureDiskReservations()
        controller_started = False
        current_mode = dict(mode="protected", generation=None)
        host_marker = Path("/etc/cairn-native-qualification-" + identifier)
        base_projects, base_archives, base_keys = {}, {}, {}

        def seed(run):
            stage = data / 'runs' / run
            for directory in (stage / 'workspace', stage / 'home/.codex', stage / 'home/.claude', stage / 'home/.agents/skills/frozen', stage / 'output'):
                directory.mkdir(parents=True, mode=0o700, exist_ok=True)
            if run in base_projects:
                (stage / 'workspace' / base_projects[run]).mkdir(exist_ok=True)
            else:
                (stage / 'workspace/seed').write_text('initial project')
                (stage / 'workspace/asset').write_bytes(asset)
            local_bin = stage / 'home/.local/bin'
            local_bin.mkdir(parents=True, exist_ok=True, mode=0o755)
            for name in ('ip', 'iptables', 'ip6tables', 'newuidmap', 'newgidmap'):
                shadow = local_bin / name
                shadow.write_text('#!/bin/sh\nprintf hijacked >/tmp/native-untrusted-helper\nexit 93\n')
                shadow.chmod(0o755)
            (stage / 'home/.codex/auth.json').write_text(SECRET)
            (stage / 'home/.agents/skills/frozen/SKILL.md').write_text('frozen fixture skill')
            return stage

        def create():
            run = str(uuid.uuid4())
            runs.append(run)
            progress('fixture-create-before')
            contexts[run] = manager('create', run=run)
            seed(run)
            progress('fixture-create-complete')
            return run

        def managed(run):
            pid = command('systemctl', 'show', supervisor, '--property=MainPID', '--value').stdout.strip()
            mounted = state / 'native' / run
            view = json.loads((mounted / 'view.json').read_text())
            path = Path('/proc') / pid / 'root' / view['workspace'].lstrip('/')
            return path / base_projects[run] if run in base_projects else path

        def start_controller():
            nonlocal controller_started
            environment = dict(DATA_DIR=str(data), RUNNER_STATE_DIR=str(state), RUNNER_BIND='127.0.0.1:4311',
                CAIRN_NATIVE_TOOLS=str(tools), CAIRN_NATIVE_USER=user, CAIRN_NODE_BACKEND='native',
                CAIRN_NATIVE_DOCKER_TOOLS=str(docker_tools),
                APP_RUNTIME_ID='synthetic', CAIRN_NATIVE_SUPERVISOR_UNIT=supervisor)
            if xfs_smoke or connector_reference:
                environment['RUNNER_URL'] = 'http://localhost/'
            # Keep the immutable reference fixture unchanged. The candidate uses
            # the real installed entry point, including its pre-Tokio IO setup.
            candidate = active_binary == binary
            entrypoint = ['node-daemon', str(node_identity)] if candidate else ['native-controller']
            if candidate:
                fixture_master.heartbeat_seen.clear()
            command('systemd-run', '--quiet', '--collect', '--service-type=exec', '--unit=' + supervisor,
                '--property=LimitNOFILE=1024:524288',
                *['--setenv=' + key + '=' + value for key, value in environment.items()], str(active_binary), *entrypoint)
            try:
                eventually(lambda: api('/health', method='GET').get('backend') == 'native')
                if candidate:
                    eventually(fixture_master.heartbeat_seen.is_set)
                    metrics['candidateEntryPoint'] = 'node-daemon'
                    metrics['offlineMasterHeartbeatObserved'] = True
            except Exception:
                diagnostics = []
                for args in [('systemctl', 'show', supervisor,
                              '--property=ActiveState,SubState,Result,ExecMainStatus'),
                             ('journalctl', '--no-pager', '--output=cat', '--lines=80', '--unit=' + supervisor)]:
                    result = subprocess.run(args, capture_output=True, text=True, timeout=10)
                    diagnostics.append(result.stdout + result.stderr)
                diagnostic_text = '\n'.join(diagnostics)
                for context in contexts.values():
                    diagnostic_text = redact_fixture_diagnostics(diagnostic_text, token, context)
                print(diagnostic_text, file=sys.stderr)
                evidence = Path(os.environ.get('NATIVE_STORAGE_EVIDENCE_DIR', str(root / 'evidence')))
                evidence.mkdir(parents=True, exist_ok=True)
                (evidence / 'controller-start-failure.txt').write_text(diagnostic_text)
                raise
            assert (state / 'controller.sock').stat().st_mode & 0o777 == 0o600
            pid = command('systemctl', 'show', supervisor, '--property=MainPID', '--value').stdout.strip()
            limits = Path('/proc/' + pid + '/limits').read_text().splitlines()
            open_files = next(line.split() for line in limits if line.startswith('Max open files'))
            assert int(open_files[3]) >= 65536, 'Controller inherited the unsafe systemd open-file limit.'
            assert Path('/proc/' + pid + '/ns/mnt').stat().st_ino == Path('/proc/1/ns/mnt').stat().st_ino, 'Controller mounts must be visible to the system manager.'
            # Restarts recover the configured budget, including capacity already
            # granted to retained Docker disks. Never shrink it to fixture defaults.
            if not controller_started:
                api('/node-budget', dict(limits=dict(diskMiB=4096, cpu=2, memoryMiB=1024), slots=1))
                controller_started = True
            storage_policy(contexts[runs[0]]['policy'])

        def prepare_base(run):
            project = str(uuid.uuid4())
            cache = data / 'project-archives'
            cache.mkdir(exist_ok=True)
            archive = cache / 'fixture.zip'
            with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=1) as output:
                output.writestr('seed', 'initial project')
                output.writestr('asset', asset)
            checksum = manager('archive-hash', path=str(archive))['hash']
            descriptor = dict(projectId=project, revision='a'*40, key='b'*64, hash=checksum,
                              bytes=archive.stat().st_size, logicalBytes=len(asset)+len('initial project'))
            cached = cache / (checksum + '.zip')
            archive.rename(cached)
            request = dict(archives=[descriptor], diskMiB=256)
            api('/native-bases/warm', request)
            try:
                ready = eventually(lambda: api('/native-bases/lookup', request)['base'])
            except AssertionError:
                # Keep the original failure while preserving this owned cache's
                # publication state and redacted supervisor errors for diagnosis.
                facts = []
                bases = state / 'native-bases'
                if bases.is_dir():
                    for entry in list(bases.iterdir())[:20]:
                        facts.append(dict(key=entry.name, ready=(entry / 'ready.json').is_file(),
                                          digest=(entry / 'contents.digest').is_file(), pin=(entry / 'pin').is_file()))
                print(json.dumps(dict(nativeBasePublication=facts)), file=sys.stderr)
                try:
                    diagnostic = subprocess.run(['journalctl', '--no-pager', '--output=cat', '--lines=60',
                                                 '--unit=' + supervisor], capture_output=True, text=True, timeout=10)
                    text = (diagnostic.stdout + diagnostic.stderr)[-16384:]
                    print(redact_fixture_diagnostics(text, token, contexts[run]), file=sys.stderr)
                except (OSError, subprocess.TimeoutExpired) as error:
                    print('Native base diagnostics unavailable: ' + type(error).__name__, file=sys.stderr)
                raise
            base_projects[run], base_archives[run], base_keys[run] = project, [descriptor], ready['key']
            stage = seed(run)
            for name in ('seed', 'asset'):
                (stage / 'workspace' / name).unlink(missing_ok=True)
            # A prepared conversation must not require the original ZIP or an
            # expanded transfer tree after warming has completed.
            cached.unlink()
            metrics['preparedBaseMetadataBytes'] = ready['metadataBytes']

        class PluginChannel(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                assert self.path == '/plugin-command'
                assert self.headers.get('Authorization') == 'Bearer fixture-plugin-' + self.server.run_id
                body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
                assert body['plugin'] == 'fixture' and body['command'] == 'identity'
                response = json.dumps(dict(output='text', value=self.server.run_id)).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(response)))
                self.send_header('Connection', 'close')
                self.end_headers()
                self.wfile.write(response)

        def relay(path, method='POST'):
            identifier = str(uuid.uuid4())
            reply = dict(done=threading.Event())
            fixture_master.replies[identifier] = reply
            fixture_master.commands.put(dict(id=identifier, method=method, path=path, body='e30='))
            assert reply['done'].wait(120), 'Node relay did not complete ' + path
            del fixture_master.replies[identifier]
            assert 200 <= reply['status'] < 300, f'Node relay rejected {path}: {reply["status"]}'

        def start(run, long=True, lease=90000, expected='changed', measure=None, docker_script='', sandbox='workspace-write', idle=False, environment=False, connector_sample=None):
            seed(run)
            channel = data / 'runs' / run / 'home/cairn-mcp.sock'
            if connector_sample is None:
                if run in mcp_servers:
                    mcp_servers[run].shutdown()
                    mcp_servers[run].server_close()
                channel.unlink(missing_ok=True)
                server = socketserver.ThreadingUnixStreamServer(str(channel), PluginChannel)
                server.daemon_threads = True
                server.run_id = run
                channel.chmod(0o600)
                threading.Thread(target=server.serve_forever, daemon=True).start()
                mcp_servers[run] = server
            attempt = str(uuid.uuid4())
            attempts.append(attempt)
            workspace = data / 'runs' / run / 'workspace'
            script = '#!/bin/sh\nset -eux\n'
            script += 'test "$(id -u)" -ne 0\n'
            administrator = current_mode['mode'] == 'administrator'
            script += "/usr/bin/python3 - <<'CAPS'\nfrom pathlib import Path\ns=dict(line.split(':',1) for line in Path('/proc/self/status').read_text().splitlines() if ':' in line)\n"
            script += 'for k in ("CapEff", "CapAmb"): assert bool(int(s[k].strip(),16) & 4) == ' + str(administrator) + '\nCAPS\n'
            if administrator:
                script += 'test "$(sudo -n id -u)" = 0\n'
                # Admin's read/search capability preserves private logical
                # workspace ancestors without chmod or path/session remapping.
                script += 'test "$(cat ' + str(private) + ')" = "synthetic owner data"\n'
                script += 'test "$(sudo -n cat ' + str(private) + ')" = "synthetic owner data"\n'
                script += 'sudo -n sh -c "printf administrative-change > ' + str(host_marker) + '"\n'
            else:
                script += 'test ! -e ' + str(private) + '\n'
                script += 'if sudo -n true 2>/dev/null; then exit 80; fi\n'
                script += 'test ! -e /run/docker.sock\nif touch /usr/cairn-should-be-readonly 2>/dev/null; then exit 81; fi\n'
            if not administrator and connector_sample is None:
                script += 'test "$(CAIRN_MCP_RUN_TOKEN=fixture-plugin-' + run + ' cairn plugin fixture identity)" = "' + run + '"\n'
            if connector_sample is None:
                script += "test \"$(cat /home/node/.codex/auth.json)\" = '" + SECRET + "'\n"
                script += 'if echo bad > /home/node/.codex/auth.json 2>/dev/null; then exit 82; fi\n'
            # Exercise the same atomic credential replacement as the real Claude
            # broker client (unit coverage invokes that Client directly).
            script += "/usr/bin/python3 - <<'CLAUDE'\nimport os,tempfile\nfrom pathlib import Path\np=Path('/home/node/.claude'); auth=Path('/run/cairn-claude-auth')\nfor i in range(2):\n    with tempfile.NamedTemporaryFile(dir=auth,delete=False) as f:\n        f.write(b'" + SECRET + "'); f.flush(); os.fsync(f.fileno())\n    os.replace(f.name,auth/'.credentials.json')\nassert not (p/'.credentials.json').exists()\n(p/'projects').mkdir(exist_ok=True)\ns=p/'projects/session'\nif s.exists(): assert s.read_text()=='durable Claude session'\ns.write_text('durable Claude session')\nCLAUDE\n"
            if run in started_runs:
                script += 'test "$(cat /home/node/.claude/user-state.json)" = "new user state"\n'
            script += 'printf "new user state" > /home/node/.claude/user-state.json\n'
            script += 'test "$(cat /run/cairn-chat/attachments/native-image/image.png)" = "synthetic image bytes"\n'
            script += 'test -f /home/node/.agents/skills/frozen/SKILL.md\n'
            script += 'if touch /home/node/.agents/skills/bad 2>/dev/null; then exit 83; fi\n'
            script += 'if test -e persisted; then test "$(cat persisted)" = "' + expected + '"; fi\n'
            if sandbox != 'read-only':
                script += 'printf changed > persisted\n'
            script += 'test ! -e /tmp/native-untrusted-helper\n'
            script += 'printf temporary-artifact > /tmp/native-artifact\n'
            script += docker_script
            if sandbox != 'read-only':
                script += f'printf %s {attempt} > ready\n'
            if measure:
                script += "while ! test -e measure-go; do sleep 0.05; done\n/usr/bin/python3 - <<'MEASURE' > measure.json\n" + MEASURE + '\nMEASURE\n'
                script += 'printf measured > measure-ready\n'
            if long and idle:
                script += 'sleep infinity\n'
            elif long:
                # Read the actual read-only bind as the unprivileged workload,
                # including concurrent steering updates after a disk warning.
                script += "/usr/bin/python3 - <<'INBOX' &\nimport json,os,time\nfrom pathlib import Path\nseen={}\nwhile True:\n    for name in ('messages.json','disk-notice.json'):\n        try: value=json.loads((Path('/run/cairn-chat')/name).read_text())\n        except FileNotFoundError: continue\n        except PermissionError:\n            Path('inbox-read-error').write_text(name); raise\n        if name=='disk-notice.json': marker,content='disk-notice-seen',value['id']\n        elif value: marker,content='steering-seen',value[0]['text']\n        else: continue\n        if seen.get(marker)==content: continue\n        with open(marker,'w') as output:\n            output.write(content); output.flush(); os.fsync(output.fileno())\n        seen[marker]=content\n    time.sleep(0.02)\nINBOX\n"
                script += ("sudo -n " if administrator else "") + "setsid sh -c 'while :; do printf x >> descendant; sleep 0.1; done' &\nwait\n"
            workload = 'native-workload-' + attempt
            (tools / 'bin' / workload).write_text(script)
            (tools / 'bin' / workload).chmod(0o555)
            (tools / 'bin/codex').write_text(Path(__file__).with_name('native_chat_fixture.py').read_text())
            (tools / 'bin/codex').chmod(0o555)
            # Online grants and Docker both grow the retained disk. Later turns
            # and controller restarts must reserve its full accepted allowance.
            disk_mib = disk_reservations.reserve(run, 2048 if docker_script else 256)
            plan = dict(id=attempt, runId=run, backend='native', runtimeId='synthetic', nodeLeaseRequired=True, nativeMode=current_mode.copy(),
                expires=None, cwd=str(workspace / base_projects[run] if run in base_projects else workspace), projectRoot=str(workspace), sandbox=sandbox,
                resources=dict(cpu=1, memoryMiB=1024 if docker_script else 512, diskMiB=disk_mib), storage=contexts[run],
                chat=dict(provider='codex', runId=run, attemptId=attempt,
                    execution=dict(messageId=attempt, text='synthetic input', recovery=False),
                    cwd=str(workspace / base_projects[run] if run in base_projects else workspace),
                    inputDirectory='/run/cairn-chat', output=str(data / 'runs' / run / 'output/result.md'),
                    model='fixture', reasoning='low', sandbox=sandbox, writableRoots=[str(workspace)],
                    args=['--fixture-workload', workload]),
                imports=[dict(source=str(workspace), target=str(workspace)), dict(source=str(data / 'runs' / run / 'home'), target='/home/node')])
            if environment:
                plan.update(environmentProtocol=1, turnId=str(uuid.uuid4()), expires=int(time.time()*1000)+300000)
            if run in base_archives and connector_sample is None:
                plan.update(nativeProjectArchives=base_archives[run], nativeProjectBase=base_keys[run])
                os.link(state / 'native-bases' / base_keys[run] / 'pin', data / 'runs' / run / ('native-base-' + attempt + '.pin'))
            image = data / 'runs' / run / 'chat-input/attachments/native-image/image.png'
            image.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            image.write_text('synthetic image bytes')
            image.chmod(0o600)
            if connector_sample is None:
                (data / 'runner-plans' / (attempt + '.json')).write_text(json.dumps(plan))
                manager('checkpoint', run=run, attempt=attempt, vm=run in vm_runs)
                api('/runs/' + attempt + '/lease', {'remainingMs': lease})
                progress('attempt-checkpointed')
            else:
                from native_connector_benchmark import seed_frames, skill_bundle

                manager('checkpoint', run=run, attempt=attempt, vm=run in vm_runs)
                source = root / ('connector-seed-' + attempt)
                (data / 'runs' / run).rename(source)
                skill_bundle(source, connector_sample)
                descriptor = dict(dataRoot=str(data), plan=plan)
                if run in base_archives:
                    descriptor['projectArchives'] = base_archives[run]
                streams = {}
                for omit_workspace in ((False, True) if run in base_archives else (False,)):
                    for omit_skills in (False, True):
                        frames = seed_frames(source, omit_skills=omit_skills, omit_workspace=omit_workspace)
                        if omit_workspace:
                            assert not any(Path(json.loads(line).get('path', '')).is_relative_to('workspace')
                                           for line in frames.splitlines()), 'Prepared seed includes workspace entries.'
                        streams[(omit_workspace, omit_skills)] = frames
                fixture_master.preparations[attempt] = dict(
                    descriptor=descriptor, lease=lease, streams=streams,
                )
            startup = time.perf_counter()
            progress('run-admission-before')
            if connector_sample is None:
                api('/runs/' + attempt)
            else:
                relay('/prepare/' + attempt)
                prepared_at = time.perf_counter()
                relay('/runs/' + attempt)
            progress('run-admission-returned')
            def execution_ready():
                if (state / (attempt + '.exit')).exists():
                    return True
                # A preceding short execution can retain its readiness file.
                return (managed(run) / 'ready').read_text() == attempt

            eventually(execution_ready, timeout=600 if docker_script else 60)
            startup_ms = (time.perf_counter() - startup) * 1000
            if connector_sample is not None:
                shutil.rmtree(source)
                sample = fixture_master.preparations.pop(attempt)['observed']
                report = api('/runs/' + attempt + '/bootstrap', method='GET')['bootstrap']
                sample.update(connectorMs=(prepared_at - startup) * 1000, readinessMs=startup_ms,
                              controllerUsageScans=report.get('usageScans'))
                metrics.setdefault('connectorStartup', []).append(sample)
            progress('attempt-ready')
            if (state / (attempt + '.exit')).exists() and read_controller_result(state / (attempt + '.exit'))['StatusCode'] != 0:
                for record in (state / (attempt + '.log')).read_text().splitlines():
                    event = json.loads(record)
                    if event.get('data'):
                        # Only the synthetic workload's errors; plans/grants never logged.
                        text = base64.b64decode(event['data']).decode(errors='replace')
                        print(redact_fixture_diagnostics(text, token, contexts[run]), file=sys.stderr)
                diagnostics = ['Synthetic workload exit code: ' + (state / (attempt + '.exit')).read_text().strip()]
                for unit in (supervisor, 'cairn-native-' + attempt + '.service'):
                    for args in [('systemctl', 'show', unit, '--property=Result,ExecMainStatus,ExecMainCode,ActiveState'),
                                 ('journalctl', '--no-pager', '--output=cat', '--lines=60', '--unit=' + unit)]:
                        result = subprocess.run(args, capture_output=True, text=True, timeout=30)
                        text = result.stdout + result.stderr
                        diagnostics.append(unit + ':\n' + text)
                diagnostic_text = redact_fixture_diagnostics('\n'.join(diagnostics), token, contexts[run])
                print(diagnostic_text, file=sys.stderr)
                evidence = Path(os.environ.get('NATIVE_STORAGE_EVIDENCE_DIR', str(root / 'evidence')))
                evidence.mkdir(parents=True, exist_ok=True)
                (evidence / 'failure-diagnostics.txt').write_text(diagnostic_text)
                raise AssertionError('Synthetic workload failed before readiness.')
            startup_cpu_ms = None
            if long:
                namespace = command('systemctl', 'show', 'cairn-native-' + attempt + '.service', '--property=ControlGroup', '--value').stdout.strip()
                assert namespace.startswith('/'), 'Workload cgroup unavailable for startup CPU sampling.'
                cpu_stat = dict(line.split() for line in (Path('/sys/fs/cgroup') / namespace.lstrip('/') / 'cpu.stat').read_text().splitlines())
                startup_cpu_ms = int(cpu_stat['usage_usec']) / 1000
            metrics.setdefault('conversationStartupMs', []).append(dict(ms=startup_ms,
                docker=bool(docker_script), nativeMode=current_mode['mode'], resume=run in started_runs, workloadCgroupCpuMs=startup_cpu_ms,
                cpuScope='Whole fresh workload cgroup at readiness; short executions may already be collected.',
                loadAverage=os.getloadavg(), readinessIncludesDockerWorkload=bool(docker_script)))
            started_runs.add(run)
            if long:
                if active_binary == binary:
                    assert not list((state / 'ublk-devices').glob('*.json')), 'Fresh directories created a ublk device.'
                    namespace = command('systemctl', 'show', 'cairn-native-' + attempt + '.service', '--property=ControlGroup', '--value').stdout.strip()
                    io = (Path('/sys/fs/cgroup') / namespace.lstrip('/') / 'io.max').read_text()
                    device = (state / 'native-data').stat().st_dev
                    line = next(value for value in io.splitlines() if value.startswith(f'{os.major(device)}:{os.minor(device)} '))
                    assert 'rbps=67108864' in line and 'wbps=67108864' in line, 'Missing native IO rate caps.'
                assert api('/runs/' + attempt + '/artifact', dict(runId=run, path='/tmp/native-artifact'), raw=True) == b'temporary-artifact'
            if measure:
                before = manager('metrics')['remoteRequests']
                (managed(run) / 'measure-go').touch()
                eventually(lambda: (managed(run) / 'measure-ready').exists())
                metrics[measure] = json.loads((managed(run) / 'measure.json').read_text())
                metrics[measure]['readTriggeredRemoteRequests'] = manager('metrics')['remoteRequests'] - before
                for name in ('measure-go', 'measure-ready', 'measure.json'):
                    (managed(run) / name).unlink()
            if long:
                (managed(run) / 'ready').unlink()
            return attempt

        def qualify_environment(*, administrator=False):
            if api('/health', method='GET').get('environmentProtocol') != 1:
                return
            run = create()
            administrator_workload = """
if test -e administrator-first-turn; then
    test "$(docker inspect --format '{{.State.Running}}' cairn-background)" = true
else
    test ! -e /run/cairn-docker/network.ready
    touch administrator-first-turn
fi
"""
            attempt = start(run, long=False, lease=300000, environment=True,
                docker_script=administrator_workload if administrator else '')
            plan = json.loads((data / 'runner-plans' / (attempt + '.json')).read_text())
            def receipt(turn):
                value = api('/runs/' + attempt + '/turn/' + turn, method='GET', timeout=25)
                return value if value.get('turnId') == turn else None
            first = eventually(lambda: receipt(plan['turnId']))
            assert first['code'] == 0
            control = '/runs/' + attempt + '/environment'
            assert api(control, dict(operation='activity', requestId=str(uuid.uuid4())))['jobs'] is False
            admitted = api(control, dict(operation='start_job', requestId='measurement', name='measurement',
                command='echo $$ > job-pid; while ! test -e finish-job; do sleep 0.1; done; printf job-finished'))
            job = admitted['jobId']
            eventually(lambda: (managed(run) / 'job-pid').exists())
            namespace_pid = int((managed(run) / 'job-pid').read_text())
            group = command('systemctl', 'show', 'cairn-native-' + attempt + '.service', '--property=ControlGroup', '--value').stdout.strip()
            pid = owned_namespace_pid(group, namespace_pid)
            assert not (state / (attempt + '.exit')).exists()
            assert api(control, dict(operation='activity', requestId=str(uuid.uuid4())))['jobs'] is True
            docker_job = None
            if os.environ.get('CAIRN_NATIVE_DOCKER_QUALIFY') == '1':
                docker_job = api(control, dict(operation='start_job', requestId='docker', name='container',
                    command='docker run -d --network=none --name=cairn-background alpine:3.22.2 sleep 300 && echo started > container-ready'))['jobId']
                eventually(lambda: (managed(run) / 'container-ready').exists(), timeout=180)
                api('/runs/' + attempt + '/lease', {'remainingMs': 300000})
                assert api(control, dict(operation='activity', requestId=str(uuid.uuid4())))['containers'] is True

                def docker_finished():
                    result = api(control, dict(operation='job_status', requestId=str(uuid.uuid4()), jobId=docker_job))
                    return result['outcome']['receipt'] if result['outcome']['state'] == 'finished' else None

                assert eventually(docker_finished)['exitCode'] == 0
            plan['turnId'] = str(uuid.uuid4())
            accepted = api(control, dict(operation='resume_turn', requestId='second', turnId=plan['turnId'], plan=plan))
            assert accepted['event'] == 'accepted'
            second = eventually(lambda: receipt(plan['turnId']))
            assert second['code'] == 0, f"Second provider turn exited with {second['code']}."
            assert second['generation'] == first['generation'], 'Provider turn changed the environment generation.'
            assert owned_namespace_pid(group, namespace_pid) == pid, 'Background job changed between provider turns.'
            if docker_job:
                assert api(control, dict(operation='activity', requestId=str(uuid.uuid4())))['containers'] is True
            (managed(run) / 'finish-job').touch()
            def completed():
                result = api(control, dict(operation='job_status', requestId=str(uuid.uuid4()), jobId=job))
                return result if result['outcome']['state'] == 'finished' else None
            api('/runs/' + attempt + '/lease', {'remainingMs': 300000})
            outcome = eventually(completed)['outcome']['receipt']
            assert outcome['exitCode'] == 0 and outcome['logTail'] == 'job-finished'
            assert not Path('/proc', str(pid)).exists()
            api(control, dict(operation='close', requestId='close'))
            api('/runs/' + attempt, method='DELETE')
            fenced(run, attempt)
            for suffix in ('.e', '.r'):
                assert not (state / 'roots' / (attempt + suffix)).exists()
            metric = 'managedAdministratorBackgroundJobs' if administrator else 'managedBackgroundJobs'
            metrics[metric] = dict(twoProviderTurns=True, sameEnvironment=True,
                jobCgroup=True, completionReceipt=True, socketCleanup=True, containerPreserved=bool(docker_job))

        def qualify_administrator_docker_turns():
            run = create()
            workload = """
if ! test -e administrator-docker-pid; then
    test ! -e /run/cairn-docker/network.ready
    mkfifo /tmp/cairn-docker-supervisor.log /tmp/cairn-dockerd.log /tmp/cairn-slirp.log
    docker info >/dev/null
    sudo -n cat /run/cairn-docker/dockerd.pid > administrator-docker-pid
else
    docker info >/dev/null
    test "$(sudo -n cat /run/cairn-docker/dockerd.pid)" = "$(cat administrator-docker-pid)"
fi
"""
            attempt = start(run, long=False, lease=300000, docker_script=workload, environment=True)
            plan = json.loads((data / 'runner-plans' / (attempt + '.json')).read_text())
            control = '/runs/' + attempt + '/environment'
            def receipt():
                turn = plan['turnId']
                value = api('/runs/' + attempt + '/turn/' + turn, method='GET', timeout=25)
                return value if value.get('turnId') == turn else None

            first = eventually(receipt)
            assert first['code'] == 0
            daemon = int((managed(run) / 'administrator-docker-pid').read_text())
            group = command('systemctl', 'show', 'cairn-native-' + attempt + '.service',
                '--property=ControlGroup', '--value').stdout.strip()
            assert Path('/proc', str(daemon), 'cgroup').read_text().startswith('0::' + group + '/')
            plan['turnId'] = str(uuid.uuid4())
            accepted = api(control, dict(operation='resume_turn', requestId='administrator-docker-second',
                turnId=plan['turnId'], plan=plan))
            assert accepted['event'] == 'accepted'
            second = eventually(receipt)
            assert second['code'] == 0 and second['generation'] == first['generation']
            api(control, dict(operation='close', requestId='administrator-docker-close'))
            api('/runs/' + attempt, method='DELETE')
            fenced(run, attempt)
            metrics['administratorDockerTurns'] = True


        def qualify_exclusive(owner, peer, peer_run):
            # Older reference runtimes deliberately skip the new capability.
            if not api('/health', method='GET').get('exclusiveReservations'):
                return
            descendant = managed(peer_run) / 'descendant'
            epoch = api('/exclusive', method='GET')['highestEpoch'] + 1
            reservation = dict(id=str(uuid.uuid4()), epoch=epoch, ownerAttemptId=owner,
                               memberAttemptIds=[], expiresAt=int(time.time() * 1000) + 5000,
                               remainingMs=5000, minimumAvailableMemoryMiB=1, reason='Native runtime qualification')
            acquired = api('/exclusive/acquire', reservation)
            assert acquired['reservation']['state'] == 'acquired'
            assert peer in acquired['reservation']['pausedAttemptIds']
            size = descendant.stat().st_size
            time.sleep(0.3)
            assert descendant.stat().st_size == size, 'Exclusive pause left a descendant running.'
            assert api('/runs/' + peer + '/administrative-pause', method='GET')['administrativePausedMs'] >= 250
            # The controller checks its gate before plan loading or slot admission.
            try:
                api('/runs/' + str(uuid.uuid4()))
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Exclusive reservation admitted an unrelated attempt.')
            released = api('/exclusive/release', dict(id=reservation['id'], epoch=epoch))
            assert released['reservation'] is None
            eventually(lambda: descendant.stat().st_size > size)
            # Expiration uses the same physical release path without a manager.
            reservation.update(id=str(uuid.uuid4()), epoch=epoch + 1,
                               expiresAt=int(time.time() * 1000) + 1000, remainingMs=1000)
            api('/exclusive/acquire', reservation)
            eventually(lambda: api('/exclusive', method='GET')['reservation'] is None, timeout=10)
            size = descendant.stat().st_size
            eventually(lambda: descendant.stat().st_size > size)
            reservation.update(id=str(uuid.uuid4()), epoch=epoch + 2,
                               expiresAt=int(time.time() * 1000) + 5000, remainingMs=5000,
                               minimumAvailableMemoryMiB=1_000_000)
            try:
                api('/exclusive/acquire', reservation)
            except urllib.error.HTTPError as error:
                assert error.code == 503
            else:
                raise AssertionError('Native reservation promised unavailable memory.')
            assert api('/exclusive', method='GET')['reservation'] is None
            size = descendant.stat().st_size
            eventually(lambda: descendant.stat().st_size > size)
            metrics.setdefault('exclusiveReservations', []).append(dict(mode=current_mode['mode'],
                descendantFrozen=True, admissionBlocked=True, released=True, expired=True))

        def workload_stopped(attempt):
            result = subprocess.run(['systemctl', 'show', 'cairn-native-' + attempt + '.service',
                '--property=ActiveState,MainPID,ControlGroup'], capture_output=True, text=True, timeout=5)
            properties = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
            if properties.get('ActiveState') not in ('inactive', 'failed') or properties.get('MainPID') != '0':
                return False
            group = properties.get('ControlGroup', '')
            if not group:
                return True
            events = Path('/sys/fs/cgroup') / group.lstrip('/') / 'cgroup.events'
            return not events.exists() or 'populated 1' not in events.read_text().splitlines()

        def fenced(run, attempt, timeout=90, expect_flush_failure=False):
            eventually(lambda: (state / (attempt + '.exit')).exists(), timeout=timeout)
            # Completion writes the exit marker before retiring admission. Use
            # the runner wait protocol to observe the entire ordered release.
            result = api('/runs/' + attempt + '/wait')
            exit_file = state / (attempt + '.exit')
            if expect_flush_failure:
                metrics['nativeFlushFailure'] = require_finalization_failure(exit_file, result)
            else:
                match_controller_result(exit_file, result)
            if (state / (attempt + '.active')).exists():
                journal = subprocess.run(['journalctl', '--no-pager', '--output=cat', '--lines=120', '--unit=' + supervisor],
                                         capture_output=True, text=True, timeout=10)
                diagnostics = journal.stdout + journal.stderr
                for context in contexts.values():
                    diagnostics = redact_fixture_diagnostics(diagnostics, token, context)
                evidence = Path(os.environ.get('NATIVE_STORAGE_EVIDENCE_DIR', str(root / 'evidence')))
                evidence.mkdir(parents=True, exist_ok=True)
                (evidence / 'incomplete-retirement.txt').write_text(diagnostics[-65536:])
                raise AssertionError('Native active marker survived wait: ' + exit_file.read_text())
            active = command('systemctl', 'show', 'cairn-native-' + attempt + '.service', '--property=ActiveState', '--value').stdout.strip()
            assert active not in ('active', 'activating'), 'Descendant escaped cgroup fencing.'
            if legacy_phase:
                assert not (state / 'native' / run / 'data/workspaces/seed').exists(), 'Legacy conversation is still mounted.'
                return None
            assert (state / 'native-data' / run / 'data/workspaces').is_dir(), 'Run stop erased retained work.'
            if not expect_flush_failure:
                assert not (state / 'native-data' / run / 'tmp').exists(), 'Run stop retained managed temporary files.'
            return completed_local_disk(api, manager, run)

        command('useradd', '--system', '--no-create-home', '--shell', '/usr/sbin/nologin', user)
        try:
            run = create()
            if xfs_smoke or connector_reference:
                active_binary = binary
                legacy_phase = False
                start_controller()
                observed = api('/health', method='GET')
                assert observed['backend'] == 'native'

                for index in range(3):
                    for warm in (False, True):
                        measured = create()
                        attempt = start(measured, connector_sample=index)
                        sample = metrics['connectorStartup'][-1]
                        assert sample['cacheSupported'] or not xfs_smoke, 'Candidate connector did not propose skill caching.'
                        assert sample['cacheHit'] == (warm and sample['cacheSupported']), 'Connector chose an unexpected seed variant.'
                        sample['warmRequest'] = warm
                        sample['preparedBase'] = False
                        sample['claimedSpare'] = False
                        relay('/runs/' + attempt, method='DELETE')
                        fenced(measured, attempt)
                        manager('purge', run=measured)
                        api('/disks/' + measured + '/delete', {'permanentlyDeleted': True})

                prepared_run = create()
                prepare_base(prepared_run)
                prepared_key = base_keys[prepared_run]

                def prepared_inode():
                    for receipt in (state / 'native-base-spares').glob('*'):
                        spare = json.loads(receipt.read_text())
                        if spare['key'] != prepared_key:
                            continue
                        identifier = spare['run']
                        marker = state / 'native' / identifier / 'spare-ready'
                        if marker.is_file() and marker.read_text() == prepared_key:
                            return (state / 'native-data' / identifier).stat().st_ino
                    return None

                spare_inode = eventually(prepared_inode) if xfs_smoke else None
                prepared_attempt = start(prepared_run, connector_sample=2)
                acquired = state / 'native-data' / prepared_run
                claimed = spare_inode is not None and acquired.stat().st_ino == spare_inode
                sample = metrics['connectorStartup'][-1]
                assert sample['workspaceOmitted'], 'Prepared start did not omit the workspace from its initial seed.'
                if xfs_smoke:
                    assert claimed, 'The connector-driven start did not claim the prepared inode.'
                    assert sample['cacheHit'], 'The prepared start did not reuse its cached skills.'
                sample.update(warmRequest=True, preparedBase=True, claimedSpare=claimed)
                relay('/runs/' + prepared_attempt, method='DELETE')
                fenced(prepared_run, prepared_attempt)
                manager('purge', run=prepared_run)
                api('/disks/' + prepared_run + '/delete', {'permanentlyDeleted': True})

                with binary.open('rb') as source:
                    binary_sha = hashlib.file_digest(source, 'sha256').hexdigest()
                report = dict(filesystem='xfs' if xfs_smoke else 'ext4', runtimeSha256=binary_sha,
                              scans='Controller process-wide scans from admission to submission. Connector scans excluded.',
                              seed='Prebuilt response frames. The real Rust skill digest and allocation estimate run before responding and are reported separately.',
                              samples=metrics['connectorStartup'])
                evidence = Path(os.environ.get('NATIVE_STORAGE_EVIDENCE_DIR', str(root / 'evidence')))
                evidence.mkdir(parents=True, exist_ok=True)
                (evidence / 'connector-startup.json').write_text(json.dumps(report, indent=2) + '\n')
                print(json.dumps(dict(connectorStartup=report)))
                if connector_reference:
                    return

                from native_docker_workload import ALPINE

                volume = 'cairn-xfs-fixture'

                def write_volume(value, owner):
                    return (
                        f'docker pull {ALPINE}\n'
                        f'docker volume create {volume}\n'
                        f"docker run --rm -v {volume}:/v {ALPINE} sh -ec "
                        f"'printf %s {value} >/v/marker; chown {owner}:{owner} /v/marker; chmod 600 /v/marker'\n"
                    )

                gentle_stop = (
                    f'docker run -d --name xfs-stop-fixture --stop-signal USR1 -v {volume}:/v {ALPINE} sh -ec '
                    "'trap \"echo orderly >/v/shutdown; exit 0\" USR1; "
                    'touch /v/stop-ready; while :; do sleep 1 & wait "$!"; done\'\n'
                    'for tick in $(seq 1 80); do\n'
                    '  if docker exec xfs-stop-fixture test -e /v/stop-ready; then break; fi\n'
                    '  sleep 0.1\n'
                    'done\n'
                    'docker exec xfs-stop-fixture test -e /v/stop-ready\n'
                )
                attempt = start(run, lease=300000, docker_script=write_volume('protected', 999) + gentle_stop)
                disk = state / 'native-data' / run / 'data'
                storage_owner = json.loads((disk / '.cairn-identity.json').read_text())
                assert storage_owner['user'] == 'namespace-storage-v1'
                assert (disk / 'workspaces/persisted').stat().st_uid == storage_owner['host_engine'][0]
                assert (disk / 'docker').stat().st_uid == storage_owner['root_uid'] == 0
                mapped = state / 'roots' / (attempt + '.idmap')
                assert mapped.is_mount(), 'The stop command lost its mapped bind source.'
                api('/runs/' + attempt, method='DELETE')
                fenced(run, attempt)
                assert (disk / 'docker' / 'volumes' / volume / '_data' / 'shutdown').read_text().strip() == 'orderly', 'Docker did not stop the running container gracefully.'
                assert not mapped.exists(), 'Fencing retained the mapped bind source.'

                target = dict(mode='administrator', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                raw_owner = storage_owner['host_engine'][0]
                administrator = start(run, lease=300000, docker_script=write_volume('administrator', raw_owner))
                raw_volume = disk / 'docker-administrator' / 'volumes' / volume / '_data' / 'marker'
                assert raw_volume.read_text() == 'administrator'
                assert raw_volume.stat().st_uid == raw_owner
                api('/runs/' + administrator, method='DELETE')
                fenced(run, administrator)

                target = dict(mode='protected', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                check = f'docker run --rm --user 999:999 -v {volume}:/v {ALPINE} sh -ec \'test "$(cat /v/marker)" = protected\'\n'
                resumed = start(run, lease=300000, docker_script=check)
                assert raw_volume.read_text() == 'administrator'
                assert raw_volume.stat().st_uid == raw_owner
                api('/runs/' + resumed, method='DELETE')
                fenced(run, resumed)
                manager('purge', run=run)
                api('/disks/' + run + '/delete', {'permanentlyDeleted': True})
                assert not disk.exists()
                print('PASS: XFS launch, protected and administrator Docker volumes, retained owners, resume and quota retirement.')
                return
            other = create()
            start_controller()
            progress('legacy-controller-ready')
            legacy_attempt = start(run, long=False)
            fenced(run, legacy_attempt)
            legacy_path = state / 'disks' / run / 'lazy'
            legacy_required = legacy_path.is_dir()
            if not legacy_required:
                assert api('/disks/' + run + '/storage-status')['storage'] == 'local-directory-v1', 'Unknown reference storage format.'
            command('systemctl', 'stop', supervisor)
            legacy_temporary = state / 'native' / run / 'tmp'
            if legacy_required:
                legacy_temporary.mkdir(parents=True, exist_ok=True)
                (legacy_temporary / 'obsolete-before-upgrade').write_bytes(b'old managed temporary data')
                legacy_path.rename(legacy_path.parent / 'resize-source')
                (legacy_path.parent / 'bootstrap.pending').write_bytes(b'resizing')
                (legacy_path.parent / 'data.ext4').write_bytes(b'partly resized temporary image')
            active_binary = binary
            legacy_phase = False
            start_controller()
            progress('candidate-controller-ready')
            qualify_environment()
            first = start(run, lease=45000, measure='nativeRetained')
            fenced(run, first)
            assert not legacy_path.exists(), 'Verified migration retained its old journal.'
            assert not (legacy_path.parent / 'resize-source').exists(), 'Resize recovery retained its old source.'
            assert not legacy_temporary.exists(), 'Migration retained obsolete managed temporary files.'
            metrics['legacyMigration'] = dict(legacyConversionExercised=legacy_required, referenceBinarySha256=hashlib.sha256(legacy_binary.read_bytes()).hexdigest(),
                candidateBinarySha256=hashlib.sha256(binary.read_bytes()).hexdigest(), retainedWorkspace=True,
                retainedClaudeSession=True, legacyResourcesRemoved=True)
            # A terminal ext4 flush failure must retain a crash prefix and finish
            # non-lazy unmount/device retirement after fencing, rather than loop
            # forever before the unmount. Only this disposable filesystem changes.
            readonly_run = create()
            readonly_attempt = start(readonly_run)
            readonly_mount = state / 'native-data'
            durable_relative = Path(readonly_run) / 'data/workspaces/readonly-durable-marker'
            durable = readonly_mount / durable_relative
            with durable.open('wb') as marker:
                marker.write(b'kept across read-only close'); marker.flush(); os.fsync(marker.fileno())
            device = readonly_mount.stat().st_dev
            sys_device = (Path('/sys/dev/block') / f'{os.major(device)}:{os.minor(device)}').resolve(strict=True)
            assert sys_device.name.startswith('loop'), 'Fault injection requires the owned fixture loop device.'
            backing = command('losetup', '--noheadings', '--output', 'BACK-FILE', '/dev/' + sys_device.name).stdout.strip()
            assert Path(backing).resolve() == root / 'native-data.ext4', 'Fault backing is not this fixture.'
            command('mount', '-o', 'remount,errors=remount-ro', str(readonly_mount))
            command('sync', '-f', str(readonly_mount))
            (Path('/sys/fs/ext4') / sys_device.name / 'trigger_fs_error').write_text('cairn-disposable-flush-failure\n')
            api('/runs/' + readonly_attempt, method='DELETE')
            # The whole pool is read-only. Inspect only after closing and repair;
            # finalization failure is authenticated independently of the work log.
            fenced(readonly_run, readonly_attempt, expect_flush_failure=True)
            command('systemctl', 'stop', supervisor)
            command('umount', str(readonly_mount))
            repaired = subprocess.run(['e2fsck', '-f', '-y', str(root / 'native-data.ext4')], capture_output=True, text=True, timeout=120)
            assert repaired.returncode in (0, 1), 'Owned pool filesystem repair failed.'
            command('mount', '-o', 'loop,prjquota', str(root / 'native-data.ext4'), str(readonly_mount))
            start_controller()
            readonly_resume = start(readonly_run)
            assert durable.read_bytes() == b'kept across read-only close'
            api('/runs/' + readonly_resume, method='DELETE')
            fenced(readonly_run, readonly_resume)
            # Real project EDQUOT applies to allocated bytes, including /tmp.
            # Sparse logical lengths do not consume that allowance. Control
            # records remain writable and stop removes temporary pressure.
            quota_run = create()
            quota_attempt = start(quota_run, idle=True)
            quota_data = state / 'native-data' / quota_run
            quota_durable = quota_data / 'data/workspaces/quota-durable-marker'
            with quota_durable.open('wb') as stream:
                stream.write(b'acknowledged before task pressure'); stream.flush(); os.fsync(stream.fileno())
            quota_tmp = quota_data / 'tmp/quota-pressure'
            command('setpriv', '--bounding-set=-sys_resource', sys.executable, '-c', "import errno,os,sys\nwith open(sys.argv[1], 'wb', buffering=0) as stream:\n    while True:\n        try:\n            stream.write(bytes(1024 * 1024))\n        except OSError as error:\n            assert error.errno in (errno.EDQUOT, errno.ENOSPC), error\n            break\n", str(quota_tmp))
            control_probe = state / 'native' / quota_run / 'fixture-full-control'
            control_probe.write_text('writable at EDQUOT')
            control_probe.unlink()
            api('/runs/' + quota_attempt, method='DELETE', timeout=25)
            fenced(quota_run, quota_attempt, timeout=25)
            assert not quota_tmp.exists()
            quota_resume = start(quota_run)
            assert quota_durable.read_bytes() == b'acknowledged before task pressure'
            api('/runs/' + quota_resume, method='DELETE')
            fenced(quota_run, quota_resume)
            metrics['nativeTaskQuotaClose'] = dict(taskAllowanceMiB=256, quotaEnforced=True,
                acknowledgedMarkerRecovered=True, controlWritable=True, temporaryPressureRemoved=True, ownershipReleased=True)
            progress('native-task-quota-close-complete')
            assert not (state / 'native-block' / run / 'root/disk.blk').exists(), 'Retired device inode was retained.'
            stopped_health = retained_health(api, run)
            assert stopped_health['usage']['diskBytes'] > 0
            assert stopped_health['diskGrowth']['retainedBytes'][run] > 0, 'Stopped directory lost its measured admission credit.'
            retained = manager('inspect', run=run)
            metrics['localDirectoryAllocatedBytes'] = retained['allocatedBytes']
            metrics['referenceRawBytes'] = len(asset) + 128 * 4096
            status = api('/disks/' + run + '/storage-status')
            assert status['storage'] == 'local-directory-v1'
            receipt = dict(attempt=first)
            try:
                api('/disks/' + run + '/evict', receipt)
            except urllib.error.HTTPError as error:
                assert error.code in (404, 405, 409)
            else:
                raise AssertionError('Local environment data was evicted.')
            prepare_base(other)
            second_run_attempt = start(other)
            assert not (data / 'runs' / other / ('native-base-' + second_run_attempt + '.pin')).exists(), 'Completed clone retained its preparation pin.'
            api('/runs/' + second_run_attempt, method='DELETE')
            fenced(other, second_run_attempt)
            other_before = api('/disks/' + other + '/storage-status')
            assert api('/disks/' + other + '/storage-status')['localBytes'] == other_before['localBytes']
            metrics['environmentEvictionRefused'] = True
            command('systemctl', 'stop', supervisor)
            start_controller()
            resumed = start(run, measure='nativeLocalRestart')
            assert metrics['nativeLocalRestart']['readTriggeredRemoteRequests'] == 0, 'Local resume requested remote blocks.'
            # Raise the live project hard limit without replacing the retained
            # directory, process or namespace.
            growth_health = api('/health', method='GET')
            assert growth_health.get('diskGrowth', {}).get('protocol') == 1, 'Qualification host lacks live quota growth.'
            unit = 'cairn-native-' + resumed + '.service'
            workload_pid = command('systemctl', 'show', unit, '--property=MainPID', '--value').stdout.strip()
            allowance = state / 'disks' / run / 'directory-limit'
            before_capacity = int(allowance.read_text())
            api('/disk-growth', dict(protocol=1, grants=[]))
            used = api('/disks/' + run + '/storage-status')['localBytes']
            pressure_file = state / 'native-data' / run / 'tmp/growth-pressure'
            with pressure_file.open('wb') as stream:
                os.posix_fallocate(stream.fileno(), 0, max(0, 256 * 1024 * 1024 * 83 // 100 - used))
            eventually(lambda: (managed(run) / 'disk-notice-seen').exists(), timeout=20)
            assert not (managed(run) / 'inbox-read-error').exists()
            inbox = data / 'runs' / run / 'chat-input'
            steering = inbox / '.fixture-steering'
            steering.write_text(json.dumps([dict(id='synthetic-follow-up', text='Continue safely')]))
            steering.chmod(0o444)
            steering.replace(inbox / 'messages.json')
            eventually(lambda: (managed(run) / 'steering-seen').read_text() == 'Continue safely', timeout=20)
            assert not (managed(run) / 'inbox-read-error').exists()
            before_reads = manager('metrics')['remoteRequests']
            growth_start = time.monotonic()
            disk_reservations.grow(api, run, resumed, 384)
            eventually(lambda: int(allowance.read_text()) > before_capacity)
            metrics['onlineGrowth'] = dict(fromMiB=256, toMiB=384, wallMs=(time.monotonic()-growth_start)*1000,
                                          remoteRequests=manager('metrics')['remoteRequests']-before_reads)
            assert command('systemctl', 'show', unit, '--property=MainPID', '--value').stdout.strip() == workload_pid
            pressure_file.unlink()
            # Allocate beyond the old quota while keeping the same directory.
            tail = managed(run) / 'growth-tail'
            with tail.open('wb') as stream:
                os.posix_fallocate(stream.fileno(), 0, 260 * 1024 * 1024)
                stream.seek(260 * 1024 * 1024 - 8)
                stream.write(b'new-tail'); stream.flush(); os.fsync(stream.fileno())
            with tail.open('rb') as stream:
                stream.seek(-8, os.SEEK_END)
                assert stream.read() == b'new-tail'
            tail.unlink()
            disk_reservations.grow(api, run, resumed, 256)
            assert int(allowance.read_text()) > before_capacity, 'Grant replay shrank the quota.'
            # Durable overwrites retain only the latest filesystem contents.
            # Confirm allocated bytes stay bounded without stopping the workload.
            before_local = api('/disks/' + run + '/storage-status')['localBytes']
            before_reads = manager('metrics')['remoteRequests']
            overwrites = command(sys.executable, '-c', '''
import json, os, sys, time
path = sys.argv[1]
started, cpu = time.monotonic(), time.process_time()
with open(path, 'w+b', buffering=0) as stream:
    for round in range(6):
        data = bytes([round + 1]) * (4 * 1024 * 1024)
        for offset in range(0, 64 * 1024 * 1024, len(data)):
            assert os.pwrite(stream.fileno(), data, offset) == len(data)
        os.fsync(stream.fileno())
    assert os.pread(stream.fileno(), 4096, 64 * 1024 * 1024 - 4096) == bytes([6]) * 4096
print(json.dumps(dict(writtenBytes=6 * 64 * 1024 * 1024, retainedFileBytes=os.stat(path).st_size,
                     wallMs=(time.monotonic()-started)*1000, cpuMs=(time.process_time()-cpu)*1000)))
''', str(managed(run) / 'overwrite-pressure'))
            after_local = api('/disks/' + run + '/storage-status')['localBytes']
            assert after_local - before_local <= 66 * 1024 * 1024, 'Overwrites accumulated historical storage.'
            assert command('systemctl', 'show', unit, '--property=MainPID', '--value').stdout.strip() == workload_pid, 'Repeated overwrites stopped the native task.'
            metrics['nativeOverwriteBounded'] = dict(json.loads(overwrites.stdout), allowanceMiB=384,
                                                          remoteRequests=manager('metrics')['remoteRequests']-before_reads)
            assert metrics['nativeOverwriteBounded']['remoteRequests'] == 0
            (managed(run) / 'overwrite-pressure').unlink()
            assert api('/disks/' + run + '/storage-status')['localBytes'] > 0
            # No local environment can be evicted, including a current writer.
            try:
                api('/disks/' + run + '/evict', dict(receipt, attempt=resumed))
            except urllib.error.HTTPError as error:
                assert error.code in (404, 405, 409)
            else:
                raise AssertionError('Eviction removed an active writer cache.')
            # Local work has no backup-lag state or remote dependency.
            status = running_local_storage(api, run)
            assert 'backupDegraded' not in status and 'backupUrgent' not in status
            assert status['paused'] is False
            descendant = managed(run) / 'descendant'
            size = descendant.stat().st_size
            eventually(lambda: descendant.stat().st_size > size)
            # Real disk-reserve pressure must still stop every descendant CPU.
            # The maximum supported reserve exceeds this disposable host's free
            # space without allocating a pressure file or exhausting its disk.
            pressure_policy = dict(contexts[run]['policy'], reserveMiB=16 * 1024 * 1024)
            storage_policy(pressure_policy)
            eventually(lambda: api('/disks/' + run + '/storage-status')['waitingFor'] == 'disk-space', timeout=25)
            eventually(lambda: api('/disks/' + run + '/storage-status')['paused'] is True)
            size = descendant.stat().st_size
            time.sleep(0.5)
            assert descendant.stat().st_size == size, 'Storage pause left a descendant running.'
            storage_policy(contexts[run]['policy'])
            eventually(lambda: api('/disks/' + run + '/storage-status')['paused'] is False)
            eventually(lambda: descendant.stat().st_size > size)
            (managed(run) / 'persisted').write_text('local mutation')
            # Stopping a protected workload must thaw its owned unit before
            # systemd can deliver termination signals. No manual thaw in this test.
            storage_policy(pressure_policy)
            eventually(lambda: api('/disks/' + run + '/storage-status')['waitingFor'] == 'disk-space', timeout=25)
            eventually(lambda: api('/disks/' + run + '/storage-status')['paused'] is True, timeout=25)
            stop_finished = threading.Event()
            stop_errors = []
            def stop_protected_workload():
                try:
                    api('/runs/' + resumed, method='DELETE')
                except Exception as error:
                    stop_errors.append(error)
                finally:
                    stop_finished.set()
            threading.Thread(target=stop_protected_workload, daemon=True).start()
            # DELETE must thaw and fence every workload process while reserve
            # pressure remains active. Its durable checkpoint may still need I/O.
            eventually(lambda: workload_stopped(resumed), timeout=25)
            # Release only this fixture's artificial reserve after CPU fencing,
            # so pending ext4 writes can flush before ownership is released.
            storage_policy(contexts[run]['policy'])
            assert stop_finished.wait(120), 'Protected stop did not finish after reserve pressure was released.'
            if stop_errors:
                raise stop_errors[0]
            fenced(run, resumed)
            assert api('/disks/' + run + '/storage-status')['storage'] == 'local-directory-v1'
            try:
                api('/disks/' + run + '/evict', receipt)
            except urllib.error.HTTPError as error:
                assert error.code in (404, 405, 409)
            else:
                raise AssertionError('Eviction removed retained work.')
            progress('storage-growth-pressure-complete')
            restored = start(run, expected='local mutation')
            (managed(run) / 'persisted').write_text('second local mutation')
            api('/runs/' + restored, method='DELETE')
            fenced(run, restored)
            guardian = start(run, expected='second local mutation')
            command('systemctl', 'kill', '--signal=SIGKILL', '--kill-who=main', 'cairn-native-guardian-' + guardian + '.service')
            fenced(run, guardian)
            crashed = start(run)
            command('systemctl', 'kill', '--signal=SIGKILL', '--kill-who=main', supervisor)
            eventually(lambda: command('systemctl', 'show', supervisor, '--property=ActiveState', '--value').stdout.strip() not in ('active', 'activating'))
            eventually(lambda: command('systemctl', 'show', supervisor, '--property=LoadState', '--value').stdout.strip() == 'not-found')
            start_controller()
            fenced(run, crashed)
            final = start(run, long=False)
            fenced(run, final)
            assert read_controller_result(state / (final + '.exit'))['StatusCode'] == 0
            progress('local-crash-recovery-complete')
            # Closing one retained directory must not stop another conversation
            # that shares the same filesystem device.
            api('/node-budget', dict(limits=dict(diskMiB=4096, cpu=2, memoryMiB=1024), slots=2))
            earlier = start(run)
            later = start(other)
            qualify_exclusive(earlier, later, other)
            closing = time.monotonic()
            api('/runs/' + earlier, method='DELETE')
            fenced(run, earlier, timeout=15)
            assert api('/health', method='GET')['activeRuns'] == 1
            assert (managed(other) / 'persisted').read_text() == 'changed'
            api('/runs/' + later, method='DELETE')
            fenced(other, later, timeout=15)
            metrics['concurrentCloseMs'] = (time.monotonic() - closing) * 1000
            api('/node-budget', dict(limits=dict(diskMiB=4096, cpu=2, memoryMiB=1024), slots=1))
            assert not list((state / 'ublk-devices').glob('*.json')), 'Owned ublk registrations survived stopped workloads.'
            try:
                api('/runs/' + final)
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Completed attempt restarted.')
            progress('concurrent-retirement-complete')
            # Real host administration and both directions of a multi-conversation
            # transition. This runs only on the disposable CI VM guarded above.
            api('/node-budget', dict(limits=dict(diskMiB=4096, cpu=2, memoryMiB=1024), slots=2))
            first_protected, second_protected = start(run), start(other)
            target = dict(mode='administrator', generation=str(uuid.uuid4()))
            applied = api('/native-mode', target)
            assert applied['status'] == 'applied', applied
            current_mode.update(target)
            fenced(run, first_protected)
            fenced(other, second_protected)
            admin_first, admin_second = start(run), start(other)
            qualify_exclusive(admin_first, admin_second, other)
            assert stat.S_IMODE(root.stat().st_mode) == 0o700, 'Administrator changed private ancestor permissions.'
            assert host_marker.read_text() == 'administrative-change', 'sudo did not administer the actual host.'
            target = dict(mode='protected', generation=str(uuid.uuid4()))
            applied = api('/native-mode', target)
            assert applied['status'] == 'applied', applied
            current_mode.update(target)
            fenced(run, admin_first)
            fenced(other, admin_second)
            assert host_marker.read_text() == 'administrative-change', 'Returning to protected must not claim to restore the host.'
            protected_again = start(run, long=False)
            fenced(run, protected_again)
            assert read_controller_result(state / (protected_again + '.exit'))['StatusCode'] == 0
            assert api('/native-mode', target) == applied, 'Retry of the applied generation must be idempotent.'
            assert api('/health', method='GET')['nativeMode'] == applied
            # A crash after persisting intent must never reopen admission on an
            # unconfirmed protected claim. Resume the durable request explicitly.
            command('systemctl', 'stop', supervisor)
            pending_generation = dict(mode='protected', generation=str(uuid.uuid4()))
            (state / 'native-mode.json').write_text(json.dumps(dict(applied, status='pending', target=pending_generation)))
            start_controller()
            assert api('/health', method='GET')['nativeMode']['status'] == 'pending'
            stale_attempt = str(uuid.uuid4())
            stale_plan = json.loads((data / 'runner-plans' / (protected_again + '.json')).read_text())
            stale_plan['id'] = stale_attempt
            attempts.append(stale_attempt)
            (data / 'runner-plans' / (stale_attempt + '.json')).write_text(json.dumps(stale_plan))
            api('/runs/' + stale_attempt + '/lease', {'remainingMs': 60000})
            try:
                api('/runs/' + stale_attempt)
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Unconfirmed privilege generation admitted an execution.')
            applied = api('/native-mode', pending_generation)
            assert applied['status'] == 'applied', applied
            current_mode.update(pending_generation)
            after_crash = start(run, long=False)
            fenced(run, after_crash)
            assert read_controller_result(state / (after_crash + '.exit'))['StatusCode'] == 0
            metrics['nativeAdministrator'] = dict(realHostSudo=True, ordinaryUser=True,
                concurrentConversations=2, rootDescendantsStopped=True,
                protectedAfterAdministrator=True, filesPreserved=True, hostChangesRetained=True,
                pendingCrashFailsClosed=True, generationRetry=True)
            progress('native-mode-transition-complete')
            if os.environ.get('CAIRN_NATIVE_DOCKER_QUALIFY') == '1':
                target = dict(mode='administrator', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                administrator_runs = [create(), create()]
                administrator_attempts = []
                rootful_source = Path(__file__).with_name('native_rootful_consumer.py').read_text()
                for current in administrator_runs:
                    retained = state / 'native-data' / current / 'data/workspaces'
                    # Keep Unix socket names short in the execution's managed /tmp.
                    runtime = '/tmp/crf-' + current.replace('-', '')[:12]
                    rootful = shlex.join(['sudo', '-n', 'env', 'CAIRN_NATIVE_DISPOSABLE_HOST=1',
                        '/usr/bin/python3', '-', str(docker_tools / 'bin'), str(probe), str(retained), runtime])
                    rootful += " <<'ROOTFUL'\n" + rootful_source + '\nROOTFUL\n'
                    administrator_attempts.append(start(current, lease=300000, docker_script=rootful))
                records = [json.loads((managed(current) / 'administrator-consumer.json').read_text()) for current in administrator_runs]
                assert records[0]['cgroup'] != records[1]['cgroup']
                for index, current in enumerate(administrator_runs):
                    record = records[index]
                    scope = Path(record['cgroup'])
                    before = scope.stat()
                    assert (before.st_dev, before.st_ino) == (record['cgroupDevice'], record['cgroupInode'])
                    assert 'populated 1' in (scope / 'cgroup.events').read_text()
                    api('/runs/' + administrator_attempts[index], method='DELETE')
                    fenced(current, administrator_attempts[index], timeout=25)
                    assert not scope.exists() or 'populated 1' not in (scope / 'cgroup.events').read_text()
                    retained = state / 'native-data' / current / 'data/workspaces'
                    assert (retained / 'administrator-volume/persisted').read_bytes() == b'container work retained'
                    if index == 0:
                        peer = Path(records[1]['cgroup'])
                        assert peer.stat().st_ino == records[1]['cgroupInode']
                        assert 'populated 1' in (peer / 'cgroup.events').read_text(), 'Same-filesystem foreign container was stopped.'
                    retirement_id = str(uuid.uuid4())
                    api('/disks/' + current + '/delete', dict(retirementId=retirement_id, attempts=[administrator_attempts[index]]))
                    assert not (state / 'native-data' / current).exists(), 'Expired administrator Docker data survived.'
                target = dict(mode='protected', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                metrics['administratorDockerConsumers'] = dict(externalSystemdScopes=2, privateDaemonsKilled=True,
                    sourceBoundToEnvironment=True, foreignContainerPreserved=True, stopPreservedVolumes=True, expirationRemovedVolumes=True)
            manager('inspect', run=run)
            # Exercise actual node retirement and its durable receipt repeatedly.
            # Policy scheduling/offline manager retries are covered by the backend
            # lifecycle suite; these cycles qualify their physical node operation.
            samples = []
            peer_bytes = (retained_workspace(state, other, base_projects.get(other)) / 'persisted').read_bytes()
            for cycle in range(5):
                current = create()
                created = start(current, long=False)
                fenced(current, created)
                own = state / 'native-data' / current
                assert (own / 'data/workspaces/persisted').read_text() == 'changed'
                resumed_cycle = start(current, long=False)
                fenced(current, resumed_cycle)
                assert (own / 'data/home/.claude/projects/session').read_text() == 'durable Claude session'
                if cycle == 2:
                    command('systemctl', 'stop', supervisor)
                    start_controller()
                retirement_id = str(uuid.uuid4())
                deletion = dict(retirementId=retirement_id, attempts=[created, resumed_cycle])
                acknowledged = api('/disks/' + current + '/delete', deletion)
                assert acknowledged['deleted'] is True and acknowledged['retirementId'] == retirement_id
                assert not own.exists(), 'Retirement retained its workload directory.'
                assert not (state / 'native' / current / 'quota-owner.json').exists(), 'Retirement retained kernel quota ownership.'
                assert not (state / 'native' / current / 'docker-runtime').exists()
                for attempt in [created, resumed_cycle]:
                    for suffix in ['active', 'run', 'vm.json', 'log', 'terminal', 'cleanup-result']:
                        assert not (state / (attempt + '.' + suffix)).exists(), f'Retired technical attempt retained {suffix}.'
                assert not (state / 'disks' / current / 'lazy').exists()
                assert not (state / 'disks' / current / 'data.ext4').exists()
                repeated = api('/disks/' + current + '/delete', deletion)
                assert repeated['deleted'] is True and repeated['retirementId'] == retirement_id
                # Recreate this conversation's environment, then replay the old
                # retirement after a controller restart. New work must survive.
                started_runs.discard(current)
                seed(current)
                replacement = start(current, long=False)
                fenced(current, replacement)
                command('systemctl', 'stop', supervisor)
                start_controller()
                repeated = api('/disks/' + current + '/delete', deletion)
                assert repeated['deleted'] is True and repeated['retirementId'] == retirement_id
                assert (own / 'data/workspaces/persisted').read_text() == 'changed'
                api('/disks/' + current + '/delete', dict(retirementId=str(uuid.uuid4()), attempts=[replacement]))
                assert not own.exists()
                assert (retained_workspace(state, other, base_projects.get(other)) / 'persisted').read_bytes() == peer_bytes
                samples.append(dict(cycle=cycle, workloadDirectories=0, privateDockerRuntimeDirectories=0,
                    environmentImages=0, activeRuns=api('/health', method='GET')['activeRuns'],
                    cleanupRetried=True, restart=True, replacementPreserved=True))
            metrics['nativeLifecycleCycles'] = samples
            metrics['intentionallyRetained'] = [
                'User history and artifacts in manager storage, governed by existing retention rules.',
                'Controller lock files and immutable disk-retirement receipts, required to reject late deletes after replacement.',
                'One shared native-data filesystem, reusable project counter, prepared base/runtime caches under existing cache budgets.'
            ]
            if os.environ.get('CAIRN_NATIVE_DOCKER_QUALIFY') == '1':
                import socket
                api('/node-budget', dict(limits=dict(diskMiB=8192, cpu=2, memoryMiB=2048), slots=2))
                docker_run = create()
                docker_other = create()

                def docker_script(phase):
                    workload = Path(__file__).with_name('native_docker_workload.py').read_text()
                    return '/usr/bin/python3 - ' + phase + " <<'NATIVE_DOCKER'\n" + workload + '\nNATIVE_DOCKER\n'

                def docker_result(current):
                    path = state / 'native-data' / current / 'data/home/.docker-test-result.json'
                    return json.loads(path.read_text())

                progress('docker-fresh')
                fresh = start(docker_run, lease=300000, docker_script=docker_script('fresh'))
                metrics['dockerFresh'] = docker_result(docker_run)
                namespace = command('systemctl', 'show', 'cairn-native-' + fresh + '.service', '--property=ControlGroup', '--value').stdout.strip()
                group = Path('/sys/fs/cgroup') / namespace.lstrip('/')
                metrics['dockerCgroup'] = dict(cpuMax=(group / 'cpu.max').read_text().strip(),
                    memoryMax=(group / 'memory.max').read_text().strip(), cpuStat=(group / 'cpu.stat').read_text())
                process_names = []
                for membership in [group / 'cgroup.procs'] + list(group.rglob('cgroup.procs')):
                    for pid in membership.read_text().split():
                        try:
                            process_names.append(Path('/proc', pid, 'comm').read_text().strip())
                        except FileNotFoundError:
                            pass
                assert 'dockerd' in process_names and 'containerd' in process_names, process_names
                metrics['dockerCgroup']['descendantNames'] = sorted(set(process_names))
                assert int((group / 'memory.max').read_text()) == 1024 * 1024 * 1024
                assert (group / 'cpu.max').read_text().split()[0] != 'max'
                with socket.socket() as network_probe:
                    assert network_probe.connect_ex(('127.0.0.1', 57431)) != 0, 'Docker published on the host network.'
                progress('docker-concurrent')
                parallel = start(docker_other, lease=300000, docker_script=docker_script('other'))
                owner_a = json.loads((state / 'native-data' / docker_run / 'data/.cairn-identity.json').read_text())
                owner_b = json.loads((state / 'native-data' / docker_other / 'data/.cairn-identity.json').read_text())
                assert 500000000 <= owner_a['root_uid'] < 501048576
                assert owner_a['root_uid'] == owner_a['root_gid']
                assert owner_a['root_uid'] != owner_b['root_uid']
                assert abs(owner_a['subuid'] - owner_b['subuid']) >= 65536
                api('/runs/' + parallel, method='DELETE')
                fenced(docker_other, parallel)
                # Preserve real Docker state through the upstream administrator
                # execution contract and back to isolated protected execution.
                def host_network_state():
                    import re
                    def rules(binary):
                        lines = command(binary).stdout.splitlines()
                        return [re.sub(r'\[\d+:\d+\]', '[counters]', line) for line in lines if line and not line.startswith('#')]
                    return dict(ipv4=rules('iptables-save'), ipv6=rules('ip6tables-save'),
                                forwarding=Path('/proc/sys/net/ipv4/ip_forward').read_text(),
                                loopback=Path('/proc/sys/net/ipv4/conf/all/route_localnet').read_text())
                host_network = host_network_state()
                progress('docker-administrator-transition')
                target = dict(mode='administrator', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                fenced(docker_run, fresh)
                if api('/health', method='GET').get('environmentProtocol') == 1:
                    progress('administrator-docker-provider-turns')
                    qualify_administrator_docker_turns()
                    progress('administrator-docker-job-turns')
                    qualify_environment(administrator=True)
                administrator_workload = Path(__file__).with_name('native_administrator_docker_workload.py').read_text()
                administrator_script = "/usr/bin/python3 - " + str(owner_a['subuid'] + 999) + ' ' + str(owner_a['subgid'] + 999) + " <<'ADMINISTRATOR_DOCKER'\n" + administrator_workload + '\nADMINISTRATOR_DOCKER\n'
                administrator_docker = start(docker_run, lease=300000, docker_script=administrator_script)
                managed_container = json.loads((managed(docker_run) / 'administrator-docker.json').read_text())
                managed_pid = managed_container['pid']
                membership = Path('/proc', str(managed_pid), 'cgroup').read_text()
                owned_group = command('systemctl', 'show', 'cairn-native-' + administrator_docker + '.service', '--property=ControlGroup', '--value').stdout.strip()
                assert membership.startswith('0::' + owned_group + '/'), membership
                # A second administrator daemon must not reuse the first's
                # default bridge or disturb its published localhost service.
                administrator_parallel = start(docker_other, lease=300000, docker_script=administrator_script)
                assert urllib.request.urlopen('http://127.0.0.1:' + managed_container['port'], timeout=5).read() == b'managed'
                assert host_network_state() == host_network, 'Managed Docker changed host firewall or forwarding policy.'
                parallel_container = json.loads((managed(docker_other) / 'administrator-docker.json').read_text())
                writes = managed(docker_run) / 'docker-writes'
                eventually(lambda: writes.exists() and writes.stat().st_size > 0)
                pressure_policy = dict(contexts[docker_run]['policy'], reserveMiB=16 * 1024 * 1024)
                storage_policy(pressure_policy)
                eventually(lambda: api('/disks/' + docker_run + '/storage-status')['paused'] is True)
                size = writes.stat().st_size
                time.sleep(0.3)
                assert writes.stat().st_size == size, 'Administrator Docker escaped the run freeze.'
                storage_policy(contexts[docker_run]['policy'])
                eventually(lambda: writes.stat().st_size > size)
                administrator_store = state / 'native-data' / docker_run / 'data/docker-administrator'
                for digit in ['d', 'e']:
                    partial = administrator_store / 'containers' / (digit * 64)
                    partial.mkdir(mode=0o700)
                    if digit == 'e':
                        (partial / 'hostconfig.json').write_text('{')
                try:
                    api('/runs/' + administrator_docker, method='DELETE')
                    fenced(docker_run, administrator_docker)
                except Exception:
                    # Preserve the controller's exact holder/close refusal
                    # before the fixture's final cleanup.
                    result = subprocess.run(
                        ['journalctl', '--no-pager', '--output=cat', '--lines=100', '--unit=' + supervisor],
                        capture_output=True, text=True, timeout=10)
                    diagnostic = result.stdout + result.stderr
                    for context in contexts.values():
                        diagnostic = redact_fixture_diagnostics(diagnostic, token, context)
                    evidence = Path(os.environ.get('NATIVE_STORAGE_EVIDENCE_DIR', str(root / 'evidence')))
                    evidence.mkdir(parents=True, exist_ok=True)
                    (evidence / 'administrator-close-failure.txt').write_text(diagnostic[-65536:])
                    print(diagnostic[-65536:], file=sys.stderr)
                    raise
                # Direct directory ownership is qualified above by the two
                # environment-owned rootful scopes. An arbitrary external
                # engine bind is deliberately not evidence to kill that engine.
                # Legacy raw mappings are qualified by the journal-format
                # prerequisite: direct directories expose no private device.
                metrics['legacyRawMappingNotApplicable'] = 'Direct native directories have no private block device.'
                assert not Path('/proc', str(managed_pid)).exists(), 'Administrator Docker survived suspension.'
                # Retire the earlier disk while the later-started daemon stays
                # alive. A recursively private clone would retain its mount.
                assert urllib.request.urlopen('http://127.0.0.1:' + parallel_container['port'], timeout=5).read() == b'managed'
                api('/runs/' + administrator_parallel, method='DELETE')
                fenced(docker_other, administrator_parallel)
                # Reopen the same store: the prior container must remain stopped.
                administrator_docker = start(docker_run, long=False, lease=300000, docker_script=administrator_script)
                fenced(docker_run, administrator_docker)
                # The next reopen checks the stop marker from normal completion.
                administrator_docker = start(docker_run, lease=300000, docker_script=administrator_script)
                metrics['administratorDockerLifecycle'] = dict(cgroupConfined=True, freezeConfirmed=True, stoppedBeforeUnmount=True, noImplicitRestart=True)
                target = dict(mode='protected', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                fenced(docker_run, administrator_docker)
                fresh = start(docker_run, lease=300000, docker_script=docker_script('resume'))
                metrics['dockerAfterAdministrator'] = docker_result(docker_run)
                # A raw rootful volume UID can coincide with protected namespace
                # UID1000. It must stay raw through both directions of translation.
                target = dict(mode='administrator', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                fenced(docker_run, fresh)
                owner_verification = start(docker_run, long=False, lease=300000, docker_script=administrator_script)
                fenced(docker_run, owner_verification)
                target = dict(mode='protected', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                fresh = start(docker_run, lease=300000, docker_script=docker_script('resume'))
                metrics['administratorRawVolumeOwnersPreserved'] = True
                # Seed the durable state left after an interrupted ownership
                # batch. A hard link keeps the old empty target inode alive
                # across unmount, publication and restart without pinning mounts.
                # Runtime credentials themselves must never enter this disk.
                docker_data = state / 'native-data' / docker_run / 'data'
                auth_target = docker_data / 'home/.codex/auth.json'
                assert auth_target.read_bytes() == b''
                pinned_target = docker_data / '.fixture-pinned-auth'
                os.link(auth_target, pinned_target)
                metadata = auth_target.stat()
                recovery_mode = (metadata.st_mode & ~0o7777) | 0o640
                entry = dict(relative=list(b'home/.codex/auth.json'), inode=metadata.st_ino,
                    uid=metadata.st_uid, gid=metadata.st_gid, mode=recovery_mode,
                    symlink=False, attributes=[], needs_apply=True)
                (docker_data / '.cairn-identity.transition.json').write_text(json.dumps(dict(previous=owner_a, target=owner_a)))
                (docker_data / '.cairn-identity.pending.json').write_text(json.dumps([entry]))
                for name in ('.cairn-identity.transition.json', '.cairn-identity.pending.json'):
                    (docker_data / name).chmod(0o600)
                auth_target.chmod(0o600)
                # Model a crash between directory creation and metadata save,
                # alongside an individually corrupt container. Healthy saved
                # binds, cache and volumes must survive local resume and controller restart.
                # Seed while the disk is mounted: fencing below unmounts it.
                containers = docker_data / 'docker/containers'
                owner = containers.stat()
                for digit in ['d', 'e']:
                    incomplete = containers / (digit * 64)
                    incomplete.mkdir(mode=0o700)
                    os.chown(incomplete, owner.st_uid, owner.st_gid)
                    if digit == 'e':
                        host_config = incomplete / 'hostconfig.json'
                        host_config.write_text('{')
                        host_config.chmod(0o600)
                        os.chown(host_config, owner.st_uid, owner.st_gid)
                api('/runs/' + fresh, method='DELETE')
                fenced(docker_run, fresh)
                progress('docker-inspect-resume')
                manager('inspect', run=docker_run)
                resumed_docker = start(docker_run, lease=300000, docker_script=docker_script('resume'))
                assert stat.S_IMODE(pinned_target.stat().st_mode) == 0o640, 'Pending inode metadata was not replayed before home refresh.'
                assert auth_target.stat().st_ino != pinned_target.stat().st_ino
                assert not (docker_data / '.cairn-identity.pending.json').exists()
                assert not (docker_data / '.cairn-identity.transition.json').exists()
                pinned_target.unlink()
                metrics['pendingHomeMetadataRecovered'] = True
                metrics['dockerResume'] = docker_result(docker_run)
                metrics['dockerPartialMetadataRecovered'] = True
                api('/runs/' + resumed_docker, method='DELETE')
                fenced(docker_run, resumed_docker)
                progress('docker-local-reopen')
                restored_docker = start(docker_run, lease=300000, docker_script=docker_script('resume'))
                metrics['dockerLocalReopen'] = docker_result(docker_run)
                progress('docker-controller-fencing')
                command('systemctl', 'kill', '--signal=SIGKILL', '--kill-who=main', supervisor)
                eventually(lambda: command('systemctl', 'show', supervisor, '--property=LoadState', '--value').stdout.strip() == 'not-found')
                start_controller()
                fenced(docker_run, restored_docker)
                api('/node-budget', dict(limits=dict(diskMiB=8192, cpu=2, memoryMiB=2048), slots=2))
                guardian_docker = start(docker_run, lease=300000, docker_script=docker_script('resume'))
                command('systemctl', 'kill', '--signal=SIGKILL', '--kill-who=main', 'cairn-native-guardian-' + guardian_docker + '.service')
                fenced(docker_run, guardian_docker)
                lease_docker = start(docker_run, lease=300000, docker_script=docker_script('resume'))
                api('/runs/' + lease_docker + '/lease', {'remainingMs': 1000})
                fenced(docker_run, lease_docker)
                progress('docker-memory-ceiling')
                oom_docker = start(docker_run, lease=300000, docker_script=docker_script('oom'))
                fenced(docker_run, oom_docker)
                assert read_controller_result(state / (oom_docker + '.exit'))['StatusCode'] != 0, 'Over-budget container survived or exited normally.'
                metrics['dockerMemoryCeilingFenced'] = True
                progress('docker-readonly')
                readonly_docker = start(docker_run, long=False, lease=300000, sandbox='read-only', docker_script=docker_script('readonly'))
                fenced(docker_run, readonly_docker)
                manager('inspect', run=docker_run)
            # Inspection verifies generated secrets never entered retained files.
            assert manager('metrics')['remoteRequests'] == 0, 'A local environment requested remote disk data.'
            progress('purge-start')
            for index, current in enumerate(runs):
                progress('purge-' + str(index + 1) + '-of-' + str(len(runs)))
                manager('purge', run=current)
                api('/disks/' + current + '/delete', {'permanentlyDeleted': True})
                if os.environ.get('CAIRN_NATIVE_DOCKER_QUALIFY') == '1':
                    # Permanent local deletion must also retire private Docker state.
                    docker_runtime_path = state / 'native' / current / 'docker-runtime'
                    assert not docker_runtime_path.exists() and not docker_runtime_path.is_symlink(), 'Permanent deletion retained Docker runtime state.'
            if os.environ.get('CAIRN_NATIVE_DOCKER_QUALIFY') == '1':
                metrics['dockerRuntimeCleanupVerified'] = True
            progress('purge-completed')
            metrics['harnessCpuSeconds'] = resource.getrusage(resource.RUSAGE_SELF).ru_utime
            metrics['host']['loadAverageEnd'] = os.getloadavg()
            metrics['host']['cpuTicksEnd'] = Path('/proc/stat').read_text().splitlines()[0]
            evidence = Path(os.environ.get('NATIVE_STORAGE_EVIDENCE_DIR', str(root / 'evidence')))
            evidence.mkdir(parents=True, exist_ok=True)
            (evidence / 'native-storage.json').write_text(json.dumps(metrics, indent=2) + '\n')
            print('PASS: real directory/project-quota ext4, unprivileged runner-entry, read-only runtime secrets/skills, '
                  'verified legacy conversion, environment eviction refusal, unrelated conversation retained, same-node resume without remote reads, '
                  'lease/descendant/guardian/controller fencing, crash recovery, concurrent disk retirement, final checkpoint, retry tombstone.')
        finally:
            # An impossible test reserve must not keep kernel writeback blocked
            # while fixture cleanup waits for the controller to stop.
            try:
                policy_generation += 1
                api('/storage-policy', dict(generation=policy_generation, policy=contexts[runs[0]]['policy']), timeout=5)
            except Exception:
                pass
            helper.stdin.close()
            try:
                helper.wait(timeout=10)
            except subprocess.TimeoutExpired:
                helper.kill()
                helper.wait()
            bridge.shutdown()
            fixture_master.shutdown()
            fixture_master.server_close()
            for server in mcp_servers.values():
                server.shutdown()
                server.server_close()
            for unit in [supervisor] + ['cairn-native-' + attempt + '.service' for attempt in attempts] + ['cairn-native-guardian-' + attempt + '.service' for attempt in attempts]:
                subprocess.run(['systemctl', 'stop', unit], capture_output=True, timeout=30)
            for current in runs:
                subprocess.run(['userdel', 'cairn-c-' + current.replace('-', '')[:24]], capture_output=True, timeout=30)
            for reserved in reserved_users:
                subprocess.run(['userdel', reserved], check=True, capture_output=True)
            Path('/etc/sudoers.d/cairn-native-' + str(__import__('pwd').getpwnam(user).pw_uid)).unlink(missing_ok=True)
            host_marker.unlink(missing_ok=True)
            subprocess.run(['userdel', user], check=True, capture_output=True)


if __name__ == '__main__':
    main()
