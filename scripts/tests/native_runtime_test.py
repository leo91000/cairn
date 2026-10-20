#!/usr/bin/env python3
"""Disposable real systemd/ublk/ext4/S3 qualification; never a personal host.

Invoked through node_s3_test.py so credentials, bucket and network are synthetic.
The loopback bridge exists only in this fixture; production remains Unix-only.
"""
import base64
import boto3
import http.client
import http.server
import importlib.util
import json
import os
from pathlib import Path
import platform
import random
import resource
import select
import stat
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import uuid

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
    if not os.environ.get('CAIRN_NODE_TEST_S3_ENDPOINT', '').startswith('http://127.0.0.1:'):
        raise RuntimeError('Run via the isolated loopback S3 launcher.')
    binary = Path(sys.argv[1]).resolve(strict=True)
    qualifier = Path(sys.argv[2]).resolve(strict=True)
    identifier = str(uuid.uuid4())
    user = 'cairn-smoke-' + identifier[:8]
    supervisor = 'cairn-native-smoke-' + identifier + '.service'
    token = 'synthetic-controller-credential'
    attempts, runs = [], []
    metrics = dict(host=dict(kernel=platform.release(), machine=platform.machine(), cpuCount=os.cpu_count(),
                            cpu=Path('/proc/cpuinfo').read_text().split('model name')[1].splitlines()[0],
                            memory=Path('/proc/meminfo').read_text().splitlines()[0]),
                   conditions='Disposable hosted Linux VM; loopback Moto S3, synthetic credentials; no WAN latency. '
                              'Cold file reads use advisory POSIX_FADV_DONTNEED; post-eviction controller restart clears userspace cache.',
                   workload='Seeded random42 8MiB asset +128 fsynced 4KiB text writes, per-process wall/CPU milliseconds.',
                   resources=dict(nativeCpu=1, nativeMemoryMiB=512, nativeDiskMiB=256,
                                  reference='Host root process without workload cgroup limits',
                                  native='Unprivileged systemd workload, journal-backed ext4 on ublk',
                                  build='Cargo development profile, debug info disabled'))
    with tempfile.TemporaryDirectory(prefix='cairn-native-smoke-', dir='/var/tmp') as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        (root / 'disposable-fixture').touch()
        data, state = root / 'data', root / 'state'
        tools = root / 'versions/synthetic/tools'
        for directory in (data, state, tools / 'bin', data / 'runner-plans'):
            directory.mkdir(parents=True, mode=0o700)
        tools.chmod(0o755)
        (tools / 'bin').chmod(0o755)
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
        metrics['filesystem'] = command('stat', '-f', '-c', '%T', str(root)).stdout.strip()

        def api(path, body=None, method='POST', raw=False):
            connection = host.UnixControllerConnection(state / 'controller.sock', timeout=120)
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

        contexts = {}
        started_runs = set()

        def seed(run, legacy=False):
            stage = data / 'runs' / run
            for directory in (stage / 'workspace', stage / 'home/.codex', stage / 'home/.claude', stage / 'home/.agents/skills/frozen', stage / 'output'):
                directory.mkdir(parents=True, mode=0o700, exist_ok=True)
            (stage / 'workspace/seed').write_text('initial project')
            (stage / 'workspace/asset').write_bytes(asset)
            (stage / 'home/.codex/auth.json').write_text(SECRET)
            (stage / 'home/.agents/skills/frozen/SKILL.md').write_text('frozen fixture skill')
            if legacy:
                old = state / 'native' / run
                for directory in (old / 'data/workspaces', old / 'data/home/.cache/models', old / 'data/home/.codex/sessions', old / 'data/home/.claude/plugins/user', old / 'data/output'):
                    directory.mkdir(parents=True, mode=0o700)
                (old / 'data/workspaces/seed').write_text('initial project')
                (old / 'data/workspaces/asset').write_bytes(asset)
                (old / 'data/home/.cache/models/keep').write_text('model cache')
                (old / 'data/home/.codex/sessions/keep').write_text('session state')
                (old / 'data/home/.claude/plugins/user/tool').write_text('legacy installed plugin')
                (old / 'data/home/.claude/CLAUDE.md').write_text('legacy user instructions')
                (old / 'initialized').write_text('1')
            return stage

        def create(legacy=False):
            run = str(uuid.uuid4())
            runs.append(run)
            contexts[run] = manager('create', run=run)
            seed(run, legacy)
            return run

        def managed(run):
            pid = command('systemctl', 'show', supervisor, '--property=MainPID', '--value').stdout.strip()
            return Path('/proc') / pid / 'root' / str(state).lstrip('/') / 'native' / run / 'data/workspaces'

        def start_controller():
            environment = dict(DATA_DIR=str(data), RUNNER_STATE_DIR=str(state), RUNNER_BIND='127.0.0.1:4311',
                CAIRN_NATIVE_TOOLS=str(tools), CAIRN_NATIVE_USER=user, CAIRN_NODE_BACKEND='native',
                APP_RUNTIME_ID='synthetic', CAIRN_NATIVE_SUPERVISOR_UNIT=supervisor)
            command('systemd-run', '--quiet', '--collect', '--service-type=exec', '--unit=' + supervisor,
                *['--setenv=' + key + '=' + value for key, value in environment.items()], str(binary), 'native-controller')
            eventually(lambda: api('/health', method='GET').get('backend') == 'native')
            assert (state / 'controller.sock').stat().st_mode & 0o777 == 0o600
            pid = command('systemctl', 'show', supervisor, '--property=MainPID', '--value').stdout.strip()
            assert Path('/proc/' + pid + '/ns/mnt').stat().st_ino == Path('/proc/1/ns/mnt').stat().st_ino, 'Controller mounts must be visible to the system manager.'
            api('/node-budget', dict(limits=dict(diskMiB=4096, cpu=2, memoryMiB=1024), slots=1))
            api('/storage-policy', contexts[runs[0]]['policy'])

        def start(run, long=True, lease=90000, expected='changed', legacy=False, measure=None):
            seed(run)
            attempt = str(uuid.uuid4())
            attempts.append(attempt)
            workspace = data / 'runs' / run / 'workspace'
            script = '#!/bin/sh\nset -eux\n'
            script += 'test "$(id -u)" -ne 0\ntest ! -e ' + str(private) + '\n'
            script += 'test ! -e /run/docker.sock\nif touch /usr/cairn-should-be-readonly 2>/dev/null; then exit 81; fi\n'
            script += "test \"$(cat /home/node/.codex/auth.json)\" = '" + SECRET + "'\n"
            script += 'if echo bad > /home/node/.codex/auth.json 2>/dev/null; then exit 82; fi\n'
            # Exercise the same atomic credential replacement as the real Claude
            # broker client (unit coverage invokes that Client directly).
            script += "/usr/bin/python3 - <<'CLAUDE'\nimport os,tempfile\nfrom pathlib import Path\np=Path('/home/node/.claude'); auth=Path('/run/cairn-claude-auth')\nfor i in range(2):\n    with tempfile.NamedTemporaryFile(dir=auth,delete=False) as f:\n        f.write(b'" + SECRET + "'); f.flush(); os.fsync(f.fileno())\n    os.replace(f.name,auth/'.credentials.json')\nassert not (p/'.credentials.json').exists()\n(p/'projects').mkdir(exist_ok=True)\ns=p/'projects/session'\nif s.exists(): assert s.read_text()=='durable Claude session'\ns.write_text('durable Claude session')\nCLAUDE\n"
            if run in started_runs:
                script += 'test "$(cat /home/node/.claude/user-state.json)" = "new user state"\n'
            script += 'printf "new user state" > /home/node/.claude/user-state.json\n'
            script += 'test -f /home/node/.agents/skills/frozen/SKILL.md\n'
            script += 'if touch /home/node/.agents/skills/bad 2>/dev/null; then exit 83; fi\n'
            if legacy:
                script += 'test "$(cat /home/node/.cache/models/keep)" = "model cache"\n'
                script += 'test "$(cat /home/node/.claude/plugins/user/tool)" = "legacy installed plugin"\n'
                script += 'test "$(cat /home/node/.claude/CLAUDE.md)" = "legacy user instructions"\n'
                script += 'test "$(cat /home/node/.codex/sessions/keep)" = "session state"\n'
            script += 'if test -e persisted; then test "$(cat persisted)" = "' + expected + '"; fi\n'
            script += 'printf changed > persisted\nprintf temporary-artifact > /tmp/native-artifact\nprintf ready > ready\n'
            if measure:
                script += "while ! test -e measure-go; do sleep 0.05; done\n/usr/bin/python3 - <<'MEASURE' > measure.json\n" + MEASURE + '\nMEASURE\n'
                script += 'printf measured > measure-ready\n'
            if long:
                script += "setsid sh -c 'while :; do printf x >> descendant; sleep 0.1; done' &\nwait\n"
            (tools / 'bin/codex').write_text(script)
            (tools / 'bin/codex').chmod(0o555)
            plan = dict(id=attempt, runId=run, backend='native', runtimeId='synthetic', nodeLeaseRequired=True,
                expires=None, cwd=str(workspace), projectRoot=str(workspace), sandbox='workspace-write',
                resources=dict(cpu=1, memoryMiB=512, diskMiB=256), args=[], prompt='synthetic input', storage=contexts[run],
                imports=[dict(source=str(workspace), target=str(workspace)), dict(source=str(data / 'runs' / run / 'home'), target='/home/node')])
            (data / 'runner-plans' / (attempt + '.json')).write_text(json.dumps(plan))
            manager('checkpoint', run=run, attempt=attempt)
            api('/runs/' + attempt + '/lease', {'remainingMs': lease})
            api('/runs/' + attempt)
            eventually(lambda: (managed(run) / 'ready').exists() or (state / (attempt + '.exit')).exists())
            if (state / (attempt + '.exit')).exists() and int((state / (attempt + '.exit')).read_text()) != 0:
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

        def fenced(run, attempt):
            eventually(lambda: (state / (attempt + '.exit')).exists(), timeout=90)
            assert not (state / (attempt + '.active')).exists()
            active = command('systemctl', 'show', 'cairn-native-' + attempt + '.service', '--property=ActiveState', '--value').stdout.strip()
            assert active not in ('active', 'activating'), 'Descendant escaped cgroup fencing.'
            assert not (state / 'native' / run / 'data/workspaces/seed').exists(), 'Stopped conversation filesystem is still mounted.'
            snapshot = api('/disks/' + run + '/snapshot-completed')
            assert snapshot['manifest']['kind'] == 'native-ext4-v1'
            assert snapshot['manifest']['onDemand'] is True
            return snapshot

        command('useradd', '--system', '--no-create-home', '--shell', '/usr/sbin/nologin', user)
        try:
            run = create(legacy=True)
            other = create()
            start_controller()
            first = start(run, lease=45000, legacy=True, measure='nativeRetained')
            fenced(run, first)
            assert not (state / 'native-block' / run / 'root/disk.blk').exists(), 'Retired device inode was retained.'
            assert (state / 'native' / run / 'legacy-data').exists(), 'Migration source retired before S3 acknowledgement.'
            assert api('/health', method='GET')['usage']['diskBytes'] > 0
            point = manager('publish', run=run)
            s3 = boto3.client('s3', endpoint_url=os.environ['CAIRN_NODE_TEST_S3_ENDPOINT'])
            first_objects = [obj for page in s3.get_paginator('list_objects_v2').paginate(Bucket='cairn-node-test') for obj in page.get('Contents', [])]
            metrics['firstPointEncodedS3Bytes'] = sum(obj['Size'] for obj in first_objects)
            metrics['firstPointEncodedS3Objects'] = len(first_objects)
            metrics['referenceRawBytes'] = len(asset) + 128 * 4096
            status = api('/disks/' + run + '/storage-status')
            assert status['dirtyBytes'] == 0
            receipt = dict(attempt=first, backupId=point['backupId'], generation=point['generation'], grantId=status['grantId'])
            try:
                api('/disks/' + run + '/evict', dict(receipt, generation=point['generation'] + 1))
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Eviction accepted a stale/mismatched S3 receipt.')
            second_run_attempt = start(other)
            api('/runs/' + second_run_attempt, method='DELETE')
            fenced(other, second_run_attempt)
            other_before = api('/disks/' + other + '/storage-status')
            evicted = api('/disks/' + run + '/evict', receipt)
            assert evicted['evictedBytes'] > 0, 'No real clean block cache reclaimed.'
            assert not (state / 'native' / run / 'legacy-data').exists()
            assert api('/disks/' + other + '/storage-status')['localBytes'] == other_before['localBytes']
            metrics['evictedBytes'] = evicted['evictedBytes']
            # A stale private inode with a different rdev must not block the next
            # allocation. This creates only an unopened test inode, not a device.
            os.mknod(state / 'native-block' / run / 'root/disk.blk', stat.S_IFBLK | 0o600, os.makedev(7, 65535))
            command('systemctl', 'stop', supervisor)
            start_controller()
            resumed = start(run, legacy=True, measure='nativeEvicted')
            assert metrics['nativeEvicted']['readTriggeredRemoteRequests'] > 0, 'Payload did not use demand-paged S3 reads.'
            for consistency in ('crash', 'filesystem'):
                live = api('/runs/' + resumed + '/snapshot', dict(consistency=consistency))
                assert live['manifest']['consistency'] == consistency
                assert live['manifest']['kind'] == 'native-ext4-v1'
            # The current writer and an unacknowledged generation cannot be evicted.
            try:
                api('/disks/' + run + '/evict', receipt)
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Eviction removed an active writer cache.')
            # Real cgroup backpressure must stop all descendant CPUs while the
            # controller/backend remain able to publish and acknowledge blocks.
            policy = dict(contexts[run]['policy'], backupSeconds=5, maxDirtySeconds=5)
            api('/storage-policy', policy)
            eventually(lambda: api('/disks/' + run + '/storage-status')['paused'] is True, timeout=25)
            descendant = managed(run) / 'descendant'
            size = descendant.stat().st_size
            time.sleep(0.5)
            assert descendant.stat().st_size == size, 'Storage pause left a descendant running.'
            manager('live-publish', run=run)
            eventually(lambda: api('/disks/' + run + '/storage-status')['paused'] is False)
            eventually(lambda: descendant.stat().st_size > size)
            api('/storage-policy', contexts[run]['policy'])
            (managed(run) / 'persisted').write_text('unpublished mutation')
            api('/runs/' + resumed, method='DELETE')
            fenced(run, resumed)
            try:
                api('/disks/' + run + '/evict', dict(receipt, attempt=resumed))
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Eviction removed unpublished journal writes.')
            manager('restore', run=run, backupId=point['backupId'])
            restored = start(run, legacy=True)
            api('/runs/' + restored, method='DELETE')
            fenced(run, restored)
            # Restore idempotence must be invalidated by later writes, even for
            # exactly the same backup ID.
            manager('restore', run=run, backupId=point['backupId'])
            guardian = start(run)
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
            assert int((state / (final + '.exit')).read_text()) == 0
            assert not list((state / 'ublk-devices').glob('*.json')), 'Owned ublk registrations survived stopped workloads.'
            try:
                api('/runs/' + final)
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Completed attempt restarted.')
            manager('publish', run=run)
            # Publication checks every decoded S3 block for generated secrets.
            s3 = boto3.client('s3', endpoint_url=os.environ['CAIRN_NODE_TEST_S3_ENDPOINT'])
            objects = [obj for page in s3.get_paginator('list_objects_v2').paginate(Bucket='cairn-node-test') for obj in page.get('Contents', [])]
            metrics['encodedS3Bytes'] = sum(obj['Size'] for obj in objects)
            metrics['encodedS3Objects'] = len(objects)
            for current in runs:
                manager('purge', run=current)
            metrics['harnessCpuSeconds'] = resource.getrusage(resource.RUSAGE_SELF).ru_utime
            evidence = Path(os.environ.get('NATIVE_STORAGE_EVIDENCE_DIR', str(root / 'evidence')))
            evidence.mkdir(parents=True, exist_ok=True)
            (evidence / 'native-storage.json').write_text(json.dumps(metrics, indent=2) + '\n')
            print('PASS: real ublk/ext4, unprivileged runner-entry, read-only runtime secrets/skills, legacy cache/session migration, '
                  'S3 publish/ack, conditional clean eviction, unrelated conversation retained, lazy remote resume, repeated restore, '
                  'lease/descendant/guardian/controller fencing, crash recovery, final checkpoint, retry tombstone.')
        finally:
            helper.stdin.close()
            try:
                helper.wait(timeout=10)
            except subprocess.TimeoutExpired:
                helper.kill()
                helper.wait()
            bridge.shutdown()
            for unit in [supervisor] + ['cairn-native-' + attempt + '.service' for attempt in attempts] + ['cairn-native-guardian-' + attempt + '.service' for attempt in attempts]:
                subprocess.run(['systemctl', 'stop', unit], capture_output=True, timeout=30)
            subprocess.run(['userdel', user], check=True, capture_output=True)


if __name__ == '__main__':
    main()
