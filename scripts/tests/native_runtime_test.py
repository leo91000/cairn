#!/usr/bin/env python3
"""Disposable real systemd/ublk/ext4/local-disk qualification; never a personal host.

Invoked through scripts/test-native-runtime.mjs with isolated fixture credentials.
The loopback bridge exists only in this fixture; production remains Unix-only.
"""
import base64
import http.client
import http.server
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import resource
import select
import shutil
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


def running_local_storage(api, run):
    def resumed():
        status = api('/disks/' + run + '/storage-status')
        return status if status.get('paused') is False else None
    return eventually(resumed, timeout=25)


def completed_local_disk(api, manager, run):
    def consolidated():
        status = api('/disks/' + run + '/storage-status')
        return status['dirtyBytes'] == 0 and status.get('published', {}).get('local') is True and status.get('completedGeneration', 0) > 0
    eventually(consolidated, timeout=120)
    result = manager('consolidate', run=run)
    status = result['status']
    assert status['dirtyBytes'] == 0
    assert status['published']['local'] is True
    assert status['completedGeneration'] > 0
    return result


SPEC = importlib.util.spec_from_file_location('native_host', Path(__file__).resolve().parents[2] / 'deploy/nodes/native-host.py')
host = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(host)
SECRET = 'synthetic-generated-secret-never-persist'


def command(*args, **options):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=60, **options)


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


def main():
    if os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') != '1' or os.geteuid() != 0:
        raise RuntimeError('Requires root on an explicitly designated disposable host.')
    binary = Path(sys.argv[1]).resolve(strict=True)
    qualifier = Path(sys.argv[2]).resolve(strict=True)
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
                                  native='Unprivileged systemd workload, journal-backed ext4 on ublk',
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
    with tempfile.TemporaryDirectory(prefix='cairn-native-smoke-', dir='/var/tmp') as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        host.require_docker_host()
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
            directory.mkdir(parents=True, mode=0o700)
        tools.chmod(0o755)
        (tools / 'bin').chmod(0o755)
        probe = tools / 'bin/native-docker-probe'
        probe_source = Path(__file__).with_name('native_docker_probe.c')
        command('cc', '-static', '-O2', '-Wall', '-Wextra', '-Werror', str(probe_source), '-o', str(probe))
        metrics['dockerProbe'] = dict(compiler=command('cc', '--version').stdout.splitlines()[0], flags='-static -O2 -Wall -Wextra -Werror', sourceSha256=hashlib.sha256(probe_source.read_bytes()).hexdigest())
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
                    if self.path.endswith('/direct-reader') and self.command == 'POST':
                        # This response owns the publication reader. Forward its
                        # headers and heartbeats while bounded PUT RPCs proceed.
                        self.send_response(response.status)
                        for key, value in response.getheaders():
                            if key.lower() not in ('content-length', 'transfer-encoding', 'connection'):
                                self.send_header(key, value)
                        self.send_header('Connection', 'close')
                        self.end_headers()
                        while chunk := response.read1(16384):
                            self.wfile.write(chunk)
                            self.wfile.flush()
                        return
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

        contexts = {}
        started_runs = set()
        vm_runs = set()
        retained_disk_mib = {}
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
            if run in vm_runs:
                workspace = json.loads((mounted / 'view.json').read_text())['workspace'] if (mounted / 'view.json').exists() else str(mounted / 'data/upper' / str(data / 'runs' / run / 'workspace').lstrip('/'))
                path = Path('/proc') / pid / 'root' / workspace.lstrip('/')
            else:
                path = Path('/proc') / pid / 'root' / str(state).lstrip('/') / 'native' / run / 'data/workspaces'
            return path / base_projects[run] if run in base_projects else path

        def start_controller():
            nonlocal controller_started
            environment = dict(DATA_DIR=str(data), RUNNER_STATE_DIR=str(state), RUNNER_BIND='127.0.0.1:4311',
                CAIRN_NATIVE_TOOLS=str(tools), CAIRN_NATIVE_USER=user, CAIRN_NODE_BACKEND='native',
                CAIRN_NATIVE_DOCKER_TOOLS=str(docker_tools),
                APP_RUNTIME_ID='synthetic', CAIRN_NATIVE_SUPERVISOR_UNIT=supervisor)
            command('systemd-run', '--quiet', '--collect', '--service-type=exec', '--unit=' + supervisor,
                '--property=LimitNOFILE=1024:524288',
                *['--setenv=' + key + '=' + value for key, value in environment.items()], str(binary), 'native-controller')
            eventually(lambda: api('/health', method='GET').get('backend') == 'native')
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
            ready = eventually(lambda: api('/native-bases/lookup', request)['base'])
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

        def start(run, long=True, lease=90000, expected='changed', measure=None, docker_script='', sandbox='workspace-write', idle=False):
            seed(run)
            channel = data / 'runs' / run / 'home/cairn-mcp.sock'
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
            if not administrator:
                script += 'test "$(CAIRN_MCP_RUN_TOKEN=fixture-plugin-' + run + ' cairn plugin fixture identity)" = "' + run + '"\n'
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
            # Docker grows this conversation's retained disk. A later turn
            # without Docker (including administrator mode) must not shrink it.
            disk_mib = max(retained_disk_mib.get(run, 256), 2048 if docker_script else 256)
            retained_disk_mib[run] = disk_mib
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
            if run in base_archives:
                plan.update(nativeProjectArchives=base_archives[run], nativeProjectBase=base_keys[run])
                os.link(state / 'native-bases' / base_keys[run] / 'pin', data / 'runs' / run / ('native-base-' + attempt + '.pin'))
            image = data / 'runs' / run / 'chat-input/attachments/native-image/image.png'
            image.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            image.write_text('synthetic image bytes')
            image.chmod(0o600)
            (data / 'runner-plans' / (attempt + '.json')).write_text(json.dumps(plan))
            manager('checkpoint', run=run, attempt=attempt, vm=run in vm_runs)
            api('/runs/' + attempt + '/lease', {'remainingMs': lease})
            progress('attempt-checkpointed')
            startup = time.perf_counter()
            progress('run-admission-before')
            api('/runs/' + attempt)
            progress('run-admission-returned')
            def execution_ready():
                if (state / (attempt + '.exit')).exists():
                    return True
                # A preceding short execution can retain its readiness file.
                return (managed(run) / 'ready').read_text() == attempt

            eventually(execution_ready, timeout=600 if docker_script else 60)
            startup_ms = (time.perf_counter() - startup) * 1000
            progress('attempt-ready')
            if (state / (attempt + '.exit')).exists() and read_controller_result(state / (attempt + '.exit'))['StatusCode'] != 0:
                for record in (state / (attempt + '.log')).read_text().splitlines():
                    event = json.loads(record)
                    if event.get('data'):
                        # Only the synthetic workload's errors; plans/grants never logged.
                        print(base64.b64decode(event['data']).decode(errors='replace').replace(SECRET, '[fixture-secret]'), file=sys.stderr)
                diagnostics = ['Synthetic workload exit code: ' + (state / (attempt + '.exit')).read_text().strip()]
                for unit in (supervisor, 'cairn-native-' + attempt + '.service'):
                    for args in [('systemctl', 'show', unit, '--property=Result,ExecMainStatus,ExecMainCode,ActiveState'),
                                 ('journalctl', '--no-pager', '--output=cat', '--lines=60', '--unit=' + unit)]:
                        result = subprocess.run(args, capture_output=True, text=True, timeout=30)
                        text = result.stdout + result.stderr
                        for secret in (SECRET, token, contexts[run]['grant']):
                            text = text.replace(secret, '[fixture-secret]')
                        diagnostics.append(unit + ':\n' + text)
                diagnostic_text = '\n'.join(diagnostics)
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
                pid = command('systemctl', 'show', supervisor, '--property=MainPID', '--value').stdout.strip()
                host_namespace = Path('/proc/1/ns/mnt').stat().st_ino
                private_backends = []
                for task in (Path('/proc') / pid / 'task').iterdir():
                    try:
                        if (task / 'ns/mnt').stat().st_ino != host_namespace:
                            private_backends.append(task)
                    except FileNotFoundError:
                        pass  # An unrelated transient runtime worker may have exited.
                assert private_backends, 'ublk backend did not isolate its mount namespace.'
                for task in private_backends:
                    view = task / 'root' / str(state).lstrip('/') / 'native' / run
                    assert (view / 'attempt').read_text() == attempt
                    assert not (view / 'data/workspaces/ready').exists(), 'Backend namespace contains its consumer filesystem.'
                    for line in (task / 'mountinfo').read_text().splitlines():
                        mount, _, filesystem = line.partition(' - ')
                        path = mount.split()[4]
                        assert not (path.startswith(str(state / 'native') + '/') and filesystem.startswith('ext4 ')), \
                            'Backend namespace pins another conversation filesystem.'
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
            assert not (state / (attempt + '.active')).exists()
            active = command('systemctl', 'show', 'cairn-native-' + attempt + '.service', '--property=ActiveState', '--value').stdout.strip()
            assert active not in ('active', 'activating'), 'Descendant escaped cgroup fencing.'
            assert not (state / 'native' / run / 'data/workspaces/seed').exists(), 'Stopped conversation filesystem is still mounted.'
            return completed_local_disk(api, manager, run)

        command('useradd', '--system', '--no-create-home', '--shell', '/usr/sbin/nologin', user)
        try:
            run = create()
            other = create()
            start_controller()
            progress('fixture-controller-ready')
            first = start(run, lease=45000, measure='nativeRetained')
            fenced(run, first)
            # A terminal ext4 flush failure must retain a crash prefix and finish
            # non-lazy unmount/device retirement after fencing, rather than loop
            # forever before the unmount. Only this disposable filesystem changes.
            readonly_run = create()
            readonly_attempt = start(readonly_run)
            readonly_mount = state / 'native' / readonly_run / 'data'
            durable = managed(readonly_run) / 'readonly-durable-marker'
            with open(durable, 'wb') as marker:
                marker.write(b'kept across read-only close')
                marker.flush()
                os.fsync(marker.fileno())
            # Attribute the disposable mount before using ext4's privileged
            # error-injection interface. Never target the host/root filesystem.
            device = readonly_mount.stat().st_dev
            attachment = state / 'native-block' / readonly_run / 'root/disk.blk'
            assert stat.S_ISBLK(attachment.stat().st_mode)
            assert attachment.stat().st_rdev == device
            sys_device = (Path('/sys/dev/block') / f'{os.major(device)}:{os.minor(device)}').resolve(strict=True)
            assert sys_device.name.startswith('ublkb')
            command('mount', '-o', 'remount,errors=remount-ro', str(readonly_mount))
            # The resumed workload also checks its earlier Claude/session files.
            # Flush those before the fault; a crash prefix does not promise to
            # preserve unrelated dirty kernel pages merely because marker fsync
            # succeeded. The descendant may continue appending after this flush.
            command('sync', '-f', str(readonly_mount))
            (Path('/sys/fs/ext4') / sys_device.name / 'trigger_fs_error').write_text('cairn-disposable-flush-failure\n')
            api('/runs/' + readonly_attempt, method='DELETE')
            readonly_snapshot = fenced(readonly_run, readonly_attempt, expect_flush_failure=True)
            assert not (state / 'native-block' / readonly_run / 'root/disk.blk').exists()
            assert readonly_snapshot['status']['published']['local'] is True
            # Resume the same local journal and confirm the acknowledged bytes.
            readonly_resume = start(readonly_run)
            assert durable.read_bytes() == b'kept across read-only close'
            api('/runs/' + readonly_resume, method='DELETE')
            fenced(readonly_run, readonly_resume)
            # A task allowance exhausted by managed tmp must not strand syncfs
            # after the workload is fenced and capacity monitoring has stopped.
            quota_run = create()
            quota_attempt = start(quota_run, idle=True)
            quota_mount = state / 'native' / quota_run / 'data'
            quota_durable = managed(quota_run) / 'quota-durable-marker'
            quota_durable.write_bytes(b'acknowledged before task pressure')
            dirty = managed(quota_run) / 'quota-dirty-pages'
            dirty.write_bytes(bytes(64 * 1024))
            command('sync', '-f', str(quota_mount))
            quota_policy = dict(contexts[quota_run]['policy'], reserveMiB=16 * 1024 * 1024)
            quota_tmp = state / 'native' / quota_run / 'tmp/quota-pressure'
            try:
                storage_policy(quota_policy)
                eventually(lambda: api('/disks/' + quota_run + '/storage-status')['paused'] is True, timeout=25)
                # Overwrite allocated pages without fsync while node admission
                # is blocked; kernel writeback cannot reach its journal yet.
                with dirty.open('r+b', buffering=0) as stream:
                    assert stream.write(bytes([7]) * (64 * 1024)) == 64 * 1024
                with quota_tmp.open('wb') as stream:
                    stream.truncate(256 * 1024 * 1024)
                stop_reason = state / (quota_attempt + '.stop-reason.json')
                eventually(lambda: json.loads(stop_reason.read_text()).get('reason') == 'conversation_disk_limit', timeout=25)
                eventually(lambda: workload_stopped(quota_attempt), timeout=25)
            finally:
                storage_policy(contexts[quota_run]['policy'])
            # Retain tmp pressure through completion: no external cleanup or
            # larger grant can rescue the stopped monitor's zero backing limit.
            api('/runs/' + quota_attempt, method='DELETE', timeout=25)
            fenced(quota_run, quota_attempt, timeout=25, expect_flush_failure=True)
            quota_tmp.unlink()
            quota_resume = start(quota_run)
            assert quota_durable.read_bytes() == b'acknowledged before task pressure'
            api('/runs/' + quota_resume, method='DELETE')
            fenced(quota_run, quota_resume)
            metrics['nativeTaskQuotaClose'] = dict(taskAllowanceMiB=256, crashPrefixRetained=True,
                                                  acknowledgedMarkerRecovered=True, ownershipReleased=True)
            progress('native-task-headroom-close-complete')
            assert not (state / 'native-block' / run / 'root/disk.blk').exists(), 'Retired device inode was retained.'
            stopped_health = retained_health(api, run)
            assert stopped_health['usage']['diskBytes'] > 0
            assert stopped_health['diskGrowth']['retainedBytes'][run] > 0, 'Stopped journal lost its measured admission credit.'
            consolidated = manager('consolidate', run=run)
            metrics['localConsolidatedBytes'] = consolidated['allocatedBytes']
            metrics['referenceRawBytes'] = len(asset) + 128 * 4096
            status = api('/disks/' + run + '/storage-status')
            assert status['dirtyBytes'] == 0
            receipt = dict(attempt=first)
            try:
                api('/disks/' + run + '/evict', receipt)
            except urllib.error.HTTPError as error:
                assert error.code == 409
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
            # A stale private inode with a different rdev must not block the next
            # allocation. This creates only an unopened test inode, not a device.
            os.mknod(state / 'native-block' / run / 'root/disk.blk', stat.S_IFBLK | 0o600, os.makedev(7, 65535))
            command('systemctl', 'stop', supervisor)
            start_controller()
            resumed = start(run, measure='nativeLocalRestart')
            assert metrics['nativeLocalRestart']['readTriggeredRemoteRequests'] == 0, 'Local resume requested remote blocks.'
            # Real journal -> live ublk -> online ext4 growth. The workload
            # process and namespace stay alive; no whole-disk materialization.
            growth_health = api('/health', method='GET')
            assert growth_health.get('diskGrowth', {}).get('protocol') == 1, 'Qualification host lacks live ublk resize.'
            unit = 'cairn-native-' + resumed + '.service'
            workload_pid = command('systemctl', 'show', unit, '--property=MainPID', '--value').stdout.strip()
            before_capacity = os.statvfs(managed(run)).f_blocks * os.statvfs(managed(run)).f_frsize
            # Negotiate first, then generate real combined workspace/tmp
            # pressure without filling the workspace itself. Sparse tmp is
            # intentionally charged by logical length, as in production.
            api('/disk-growth', dict(protocol=1, grants=[]))
            space = os.statvfs(managed(run))
            used = (space.f_blocks - space.f_bfree) * space.f_frsize
            pressure_file = state / 'native' / run / 'tmp/growth-pressure'
            with pressure_file.open('wb') as stream:
                stream.truncate(max(0, 256 * 1024 * 1024 * 83 // 100 - used))
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
            api('/disk-growth', dict(protocol=1, grants=[dict(attemptId=resumed, allowanceMiB=384, reason='granted')]))
            eventually(lambda: os.statvfs(managed(run)).f_blocks * os.statvfs(managed(run)).f_frsize > before_capacity)
            metrics['onlineGrowth'] = dict(fromMiB=256, toMiB=384, wallMs=(time.monotonic()-growth_start)*1000,
                                          remoteRequests=manager('metrics')['remoteRequests']-before_reads)
            assert command('systemctl', 'show', unit, '--property=MainPID', '--value').stdout.strip() == workload_pid
            pressure_file.unlink()
            # Allocating more than the entire old disk exercises the ublk I/O
            # bounds after resizing, including its newly appended journal tail.
            tail = managed(run) / 'growth-tail'
            with tail.open('wb') as stream:
                os.posix_fallocate(stream.fileno(), 0, 260 * 1024 * 1024)
                stream.seek(260 * 1024 * 1024 - 8)
                stream.write(b'new-tail'); stream.flush(); os.fsync(stream.fileno())
            with tail.open('rb') as stream:
                stream.seek(-8, os.SEEK_END)
                assert stream.read() == b'new-tail'
            tail.unlink()
            api('/disk-growth', dict(protocol=1, grants=[dict(attemptId=resumed, allowanceMiB=256, reason='granted')]))
            assert os.statvfs(managed(run)).f_blocks * os.statvfs(managed(run)).f_frsize > before_capacity, 'Grant replay shrank filesystem.'
            # The issued 384 MiB allowance also governs retained journal bytes,
            # not just ext4 usage. Repeated durable overwrites must consolidate
            # below the old 1 GiB threshold without stopping the native workload.
            # Exercise local consolidation, not the obsolete S3 snapshot API.
            before_local = (api('/disks/' + run + '/storage-status').get('published') or {}).get('generation', 0)
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
            def advanced_local_generation():
                current = api('/disks/' + run + '/storage-status').get('published') or {}
                return current.get('local') is True and current.get('generation', 0) > before_local
            eventually(advanced_local_generation)
            assert command('systemctl', 'show', unit, '--property=MainPID', '--value').stdout.strip() == workload_pid, 'Journal history exhausted the native task allowance.'
            metrics['nativeOverwriteConsolidation'] = dict(json.loads(overwrites.stdout), allowanceMiB=384,
                                                          remoteRequests=manager('metrics')['remoteRequests']-before_reads)
            assert metrics['nativeOverwriteConsolidation']['remoteRequests'] == 0
            (managed(run) / 'overwrite-pressure').unlink()
            # No local environment can be evicted, including a current writer.
            try:
                api('/disks/' + run + '/evict', dict(receipt, attempt=resumed))
            except urllib.error.HTTPError as error:
                assert error.code == 409
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
            assert api('/disks/' + run + '/storage-status')['dirtyBytes'] == 0
            try:
                api('/disks/' + run + '/evict', receipt)
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Eviction removed local journal writes.')
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
            # Close the earlier disk while a later backend remains runnable.
            # CLONE_NEWNS alone copies the earlier mount into that backend,
            # causing DEL_DEV to block and retain every physical slot.
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
            manager('consolidate', run=run)
            if os.environ.get('CAIRN_NATIVE_DOCKER_QUALIFY') == '1':
                import socket
                api('/node-budget', dict(limits=dict(diskMiB=8192, cpu=2, memoryMiB=2048), slots=2))
                docker_run = create()
                docker_other = create()

                def docker_script(phase):
                    workload = Path(__file__).with_name('native_docker_workload.py').read_text()
                    return '/usr/bin/python3 - ' + phase + " <<'NATIVE_DOCKER'\n" + workload + '\nNATIVE_DOCKER\n'

                def docker_result(current):
                    path = state / 'native' / current / 'data/home/.docker-test-result.json'
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
                with socket.socket() as probe:
                    assert probe.connect_ex(('127.0.0.1', 57431)) != 0, 'Docker published on the host network.'
                progress('docker-concurrent')
                parallel = start(docker_other, lease=300000, docker_script=docker_script('other'))
                owner_a = json.loads((state / 'native' / docker_run / 'data/.cairn-identity.json').read_text())
                owner_b = json.loads((state / 'native' / docker_other / 'data/.cairn-identity.json').read_text())
                assert 500000000 <= owner_a['root_uid'] < 501048576
                assert owner_a['root_uid'] == owner_a['root_gid']
                assert owner_a['root_uid'] != owner_b['root_uid']
                assert abs(owner_a['subuid'] - owner_b['subuid']) >= 65536
                api('/runs/' + parallel, method='DELETE')
                fenced(docker_other, parallel)
                # Preserve real Docker state through the upstream administrator
                # execution contract and back to isolated protected execution.
                progress('docker-administrator-transition')
                target = dict(mode='administrator', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                fenced(docker_run, fresh)
                administrator_docker = start(docker_run, lease=300000)
                target = dict(mode='protected', generation=str(uuid.uuid4()))
                assert api('/native-mode', target)['status'] == 'applied'
                current_mode.update(target)
                fenced(docker_run, administrator_docker)
                fresh = start(docker_run, lease=300000, docker_script=docker_script('resume'))
                metrics['dockerAfterAdministrator'] = docker_result(docker_run)
                # Seed the durable state left after an interrupted ownership
                # batch. A hard link keeps the old empty target inode alive
                # across unmount, publication and restart without pinning mounts.
                # Runtime credentials themselves must never enter this disk.
                docker_data = state / 'native' / docker_run / 'data'
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
                progress('docker-consolidate-resume')
                manager('consolidate', run=docker_run)
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
                manager('consolidate', run=docker_run)
            # Consolidation verifies generated secrets never entered local base blocks.
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
            metrics['retainedRemoteObjects'] = len(remaining)
            progress('purge-completed')
            metrics['harnessCpuSeconds'] = resource.getrusage(resource.RUSAGE_SELF).ru_utime
            metrics['host']['loadAverageEnd'] = os.getloadavg()
            metrics['host']['cpuTicksEnd'] = Path('/proc/stat').read_text().splitlines()[0]
            evidence = Path(os.environ.get('NATIVE_STORAGE_EVIDENCE_DIR', str(root / 'evidence')))
            evidence.mkdir(parents=True, exist_ok=True)
            (evidence / 'native-storage.json').write_text(json.dumps(metrics, indent=2) + '\n')
            print('PASS: real ublk/ext4, unprivileged runner-entry, read-only runtime secrets/skills, '
                  'local consolidation, environment eviction refusal, unrelated conversation retained, same-node resume without remote reads, '
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
