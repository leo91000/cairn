#!/usr/bin/env python3
"""Real systemd containment/lease qualification on a disposable Linux host.

Uses a synthetic coding engine, root-private temporary data, unique units/account,
and the actual cairn runner-entry with a CPU-only workload. Never run on a personal
GPU workstation.
"""
import base64
import http.client
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.error
import uuid


SPEC = importlib.util.spec_from_file_location('native_host', Path(__file__).resolve().parents[2] / 'deploy/nodes/native-host.py')
host = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(host)


def command(*args, **options):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=40, **options)


def eventually(check, timeout=30):
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


def main():
    if os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') != '1' or os.geteuid() != 0:
        raise RuntimeError('Requires root on an explicitly designated disposable host.')
    # Hosted runners can expose display devices. This test runs only a synthetic
    # shell workload: it never invokes a graphics or compute API.
    binary = Path(sys.argv[1]).resolve(strict=True)
    identifier = str(uuid.uuid4())
    user = 'cairn-smoke-' + identifier[:8]
    supervisor = 'cairn-native-smoke-' + identifier + '.service'
    token = 'synthetic-controller-credential'
    attempts = []
    with tempfile.TemporaryDirectory(prefix='cairn-native-smoke-', dir='/var/tmp') as temporary:
        root = Path(temporary)
        root.chmod(0o700)
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
        origin = 'http://localhost'
        run = str(uuid.uuid4())
        stage = data / 'runs' / run
        workspace = stage / 'workspace'
        for directory in (workspace, stage / 'home', stage / 'output'):
            directory.mkdir(parents=True, mode=0o700)
        (workspace / 'seed').write_text('initial project')
        managed = state / 'native' / run / 'data/workspaces'

        def api(path, body=None, method='POST'):
            connection = host.UnixControllerConnection(state / 'controller.sock', timeout=40)
            try:
                connection.request(method, path, body=json.dumps(body or {}).encode() if method != 'GET' else None,
                    headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
                response = connection.getresponse()
                if response.status >= 400:
                    raise urllib.error.HTTPError(origin + path, response.status, 'Native request rejected', response.headers, None)
                return json.loads(response.read())
            finally:
                connection.close()

        def start_controller():
            environment = dict(DATA_DIR=str(data), RUNNER_STATE_DIR=str(state), RUNNER_BIND='127.0.0.1:4311',
                CAIRN_NATIVE_TOOLS=str(tools), CAIRN_NATIVE_USER=user, CAIRN_NODE_BACKEND='native',
                APP_RUNTIME_ID='synthetic', CAIRN_NATIVE_SUPERVISOR_UNIT=supervisor)
            command('systemd-run', '--quiet', '--collect', '--service-type=exec', '--unit=' + supervisor,
                *['--setenv=' + key + '=' + value for key, value in environment.items()], str(binary), 'native-controller')
            eventually(lambda: api('/health', method='GET').get('backend') == 'native')
            assert (state / 'controller.sock').stat().st_mode & 0o777 == 0o600

        def start(long=True, lease=15000):
            attempt = str(uuid.uuid4())
            attempts.append(attempt)
            script = '#!/bin/sh\nset -eu\n'
            script += 'test "$(id -u)" -ne 0\n'
            script += 'test ! -e ' + str(private) + '\n'
            script += 'test ! -e /run/docker.sock\n'
            script += 'if touch /usr/cairn-should-be-readonly 2>/dev/null; then exit 81; fi\n'
            script += 'printf changed > persisted\nprintf ready > ready\n'
            if long:
                script += "setsid sh -c 'while :; do printf x >> descendant; sleep 0.1; done' &\nwait\n"
            (tools / 'bin/codex').write_text(script)
            (tools / 'bin/codex').chmod(0o555)
            plan = dict(id=attempt, runId=run, backend='native', runtimeId='synthetic', nodeLeaseRequired=True,
                expires=None, cwd=str(workspace), projectRoot=str(workspace), sandbox='workspace-write',
                resources=dict(cpu=1, memoryMiB=512, diskMiB=1024), args=[], prompt='synthetic input',
                imports=[dict(source=str(workspace), target=str(workspace)), dict(source=str(stage / 'home'), target='/home/node')])
            (data / 'runner-plans' / (attempt + '.json')).write_text(json.dumps(plan))
            api('/runs/' + attempt + '/lease', {'remainingMs': lease})
            api('/runs/' + attempt)
            eventually(lambda: (managed / 'ready').exists() or (state / (attempt + '.exit')).exists())
            if not (managed / 'ready').exists():
                records = (state / (attempt + '.log')).read_text().splitlines()
                for record in records:
                    event = json.loads(record)
                    if event.get('data'):
                        print(base64.b64decode(event['data']).decode(errors='replace'), file=sys.stderr)
                raise AssertionError('Synthetic workload failed before reaching readiness.')
            return attempt

        def fenced(attempt):
            eventually(lambda: (state / (attempt + '.exit')).exists(), timeout=40)
            path = managed / 'descendant'
            size = path.stat().st_size if path.exists() else 0
            time.sleep(0.5)
            assert not path.exists() or path.stat().st_size == size, 'Descendant escaped cgroup fencing.'
            assert not (state / (attempt + '.active')).exists()
            assert json.loads((state / 'native' / run / 'completed.json').read_text())['manifest']['kind'] == 'native-files-v1'
            (managed / 'ready').unlink(missing_ok=True)

        command('useradd', '--system', '--no-create-home', '--shell', '/usr/sbin/nologin', user)
        try:
            start_controller()
            first = start(lease=5000)
            fenced(first)
            assert (managed / 'persisted').read_text() == 'changed'
            # Repeated attempts preserve the existing workspace instead of reseeding it.
            (workspace / 'persisted').write_text('replacement seed must be ignored')
            second = start()
            command('systemctl', 'kill', '--signal=SIGKILL', '--kill-who=main', 'cairn-native-guardian-' + second + '.service')
            fenced(second)
            third = start()
            command('systemctl', 'kill', '--signal=SIGKILL', '--kill-who=main', supervisor)
            eventually(lambda: not command('systemctl', 'show', 'cairn-native-' + third + '.service', '--property=ActiveState', '--value').stdout.strip() in ('active', 'activating'))
            start_controller()
            eventually(lambda: (state / (third + '.exit')).exists())
            assert not (state / (third + '.active')).exists()
            (managed / 'ready').unlink(missing_ok=True)
            final = start(long=False)
            fenced(final)
            assert int((state / (final + '.exit')).read_text()) == 0
            assert (managed / 'persisted').read_text() == 'changed'
            try:
                api('/runs/' + final)
            except urllib.error.HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Completed attempt restarted.')
            print('PASS: unprivileged runner-entry, private filesystem, readonly system, lease expiry, descendant fencing, guardian crash, controller crash/recovery, persisted workspace, stopped checkpoint, retry tombstone.')
        finally:
            for unit in [supervisor] + ['cairn-native-' + attempt + '.service' for attempt in attempts] + ['cairn-native-guardian-' + attempt + '.service' for attempt in attempts]:
                subprocess.run(['systemctl', 'stop', unit], capture_output=True, timeout=30)
            subprocess.run(['userdel', user], check=True, capture_output=True)


if __name__ == '__main__':
    main()
