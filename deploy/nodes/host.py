#!/usr/bin/env python3
"""Root-owned systemd supervisor. Containers never receive the host Docker socket."""
import getpass
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(os.environ.get('LEO_NODE_ROOT', '/var/lib/leo-node'))
NAME = 'leo-execution-node'
STOP = False
STOP_AT = None
IMAGE = re.compile(r'^[a-z0-9][a-z0-9./:_-]*@sha256:[a-f0-9]{64}$')


def atomic(path, value):
    temp = path.with_suffix('.tmp')
    with open(temp, 'w', encoding='utf-8', opener=lambda p, f: os.open(p, f, 0o600)) as out:
        json.dump(value, out)
        out.flush()
        os.fsync(out.fileno())
    os.replace(temp, path)
    fd = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def command(args, timeout=120, data=None, interruptible=True):
    with subprocess.Popen(args, stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) as process:
        if data is not None:
            process.stdin.write(data)
            process.stdin.close()
        deadline = time.monotonic() + timeout
        while process.poll() is None:
            if time.monotonic() >= deadline or STOP and interruptible:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                raise RuntimeError('Node command interrupted or timed out')
            time.sleep(0.1)
        if process.returncode:
            raise RuntimeError('Node command failed; previous image and data are retained')


def origin(value):
    parsed = urllib.parse.urlsplit(value)
    if not (parsed.scheme == 'https' or parsed.scheme == 'http' and parsed.hostname in ('localhost', '127.0.0.1', '::1')):
        raise ValueError('Use an HTTPS master origin')
    if parsed.username or parsed.password or parsed.path not in ('', '/') or parsed.query or parsed.fragment:
        raise ValueError('Invalid master origin')
    return value.rstrip('/')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RuntimeError('Master redirects are not accepted')


def request(master, route, body=None, authenticated=True):
    headers = {'Content-Type': 'application/json'}
    if authenticated:
        identity = json.loads((ROOT / 'data/node/identity.json').read_text())
        headers['Authorization'] = 'Bearer ' + identity['token']
    req = urllib.request.Request(origin(master) + route, headers=headers,
                                 data=None if body is None else json.dumps(body).encode())
    with urllib.request.build_opener(NoRedirect).open(req, timeout=5) as response:
        data = response.read(1024 * 1024 + 1)
        if len(data) > 1024 * 1024:
            raise RuntimeError('Master response too large')
        return json.loads(data)


def target(master):
    value = request(master, '/internal/nodes/release', authenticated=False)
    if value.get('protocol') != 2 or not IMAGE.fullmatch(value.get('image', '')):
        raise RuntimeError('Master did not approve a compatible immutable node image')
    return value


def container():
    inspected = subprocess.run(['docker', 'inspect', NAME], capture_output=True, text=True, timeout=10, check=False)
    if inspected.returncode:
        return None
    value = json.loads(inspected.stdout)[0]
    identity = json.loads((ROOT / 'data/node/identity.json').read_text())
    if value['Config'].get('Labels', {}).get('dev.leo.node.owner') != identity['nodeId']:
        raise RuntimeError('Container name belongs to another installation')
    return value


def remove():
    if container() is not None:
        subprocess.run(['docker', 'rm', '-f', NAME], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=False)


def refresh_supervisor():
    current = Path(__file__).resolve()
    pending = current.with_suffix('.next.py')
    command(['docker', 'cp', NAME + ':/opt/leo-node/host.py', str(pending)], timeout=10)
    source = pending.read_bytes()
    if len(source) > 1024 * 1024:
        raise RuntimeError('Invalid supervisor package')
    compile(source, str(current), 'exec')
    if source == current.read_bytes():
        pending.unlink()
        return
    os.chmod(pending, 0o755)
    with open(pending, 'rb') as file:
        os.fsync(file.fileno())
    os.replace(pending, current)
    os.execv(sys.executable, [sys.executable, str(current), 'run'])


def launch(image):
    if not IMAGE.fullmatch(image):
        raise ValueError('Invalid immutable image')
    remove()
    command(['docker', 'run', '-d', '--name', NAME, '--init', '--user', '0:0', '--read-only',
             '--restart=unless-stopped', '--label', 'dev.leo.node.owner=' + json.loads((ROOT / 'data/node/identity.json').read_text())['nodeId'], '--cap-drop=ALL', *['--cap-add=' + cap for cap in ('SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE')],
             '--security-opt=apparmor:unconfined', '--security-opt=seccomp:unconfined',
             '--device=/dev/kvm', '--device=/dev/net/tun', '--sysctl=net.ipv4.ip_forward=1',
             '--sysctl=net.ipv6.conf.all.disable_ipv6=1', '--tmpfs=/run', '--tmpfs=/tmp',
             '-v', f'{ROOT}/data:/data', '-v', f'{ROOT}/state:/runner-state',
             '-e', 'DATA_DIR=/data', '-e', 'RUNNER_STATE_DIR=/runner-state',
             '-e', 'RUNNER_BIND=127.0.0.1:4311', '-e', 'RUNNER_URL=http://127.0.0.1:4311',
             '-e', 'LEO_NODE_IMAGE=' + image, '--entrypoint=/usr/local/bin/leo', image,
             'node-daemon', '/data/node'])
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline and not STOP:
        try:
            command(['docker', 'exec', NAME, '/usr/local/bin/node', '-e',
                     "fetch('http://127.0.0.1:4311/health').then(r=>r.json()).then(v=>{if(v.nodeProtocol!==2||v.pool.ready+v.pool.occupied<1)process.exit(1)}).catch(()=>process.exit(1))"], timeout=5)
            return
        except RuntimeError:
            state = container()
            if state is None or not state['State']['Running']:
                raise RuntimeError('New node container exited before becoming healthy')
            time.sleep(1)
    raise RuntimeError('New node controller failed its health check')


def drain(master):
    configured = json.loads((ROOT / 'config.json').read_text()).get('shutdownTimeoutSeconds', 300)
    budget = max(10, min(300, configured) - 55)
    deadline = min(time.monotonic() + budget, STOP_AT + budget if STOP_AT is not None else float('inf'))
    try:
        request(master, '/internal/nodes/maintenance', {'action': 'drain'})
        while time.monotonic() < min(deadline, STOP_AT + budget if STOP_AT is not None else float('inf')):
            value = request(master, '/internal/nodes/maintenance', {'action': 'status'})
            if value.get('maintenance') == 'ready-to-update':
                return
            time.sleep(1)
    except (OSError, ValueError, RuntimeError):
        # The master will fence by lease expiry. Local VM disks remain on this node.
        pass


def complete(master, image, error=None):
    request(master, '/internal/nodes/maintenance', {'action': 'complete', 'image': image, 'error': error})


def update(config, release):
    image = release['image']
    command(['docker', 'pull', image], timeout=1200)
    if STOP:
        return
    previous = config['image']
    drain(config['master'])
    if STOP:
        return
    try:
        launch(image)
    except (OSError, ValueError, RuntimeError):
        launch(previous)
        config.update(failedImage=image, pendingCompletion=True, updateError='Update health check failed; previous image restored')
        atomic(ROOT / 'config.json', config)
        reconcile(config)
        return
    config.update(image=image, previousImage=previous, failedImage=None, updateError=None, pendingCompletion=True, pendingSupervisor=True)
    atomic(ROOT / 'config.json', config)
    reconcile(config)


def cache_runtimes(master):
    value = request(master, '/internal/nodes/maintenance', {'action': 'runtimes'})
    for runtime in value.get('runtimes', []):
        identifier, image = runtime.get('runtimeId', ''), runtime.get('image', '')
        if not re.fullmatch(r'[a-zA-Z0-9-]{1,100}', identifier) or not IMAGE.fullmatch(image):
            raise RuntimeError('Invalid approved runtime package')
        cache = ROOT / 'state/images' / identifier
        if (cache / 'root.ext4').is_file() and (cache / 'vmlinux').is_file():
            continue
        (ROOT / 'state/images').mkdir(parents=True, exist_ok=True, mode=0o700)
        command(['docker', 'pull', image], timeout=1200)
        # Extract from the approved digest, with no network or host Docker access.
        # Publish atomically; repair missing files without replacing an existing image.
        script = 'test "$APP_RUNTIME_ID" = "$LEO_EXPECTED_RUNTIME"; staging="/cache/$APP_RUNTIME_ID.partial"; mkdir -p "$staging"; zstd -d -f /opt/leo-vm/root.ext4.zst -o "$staging/root.ext4"; cp /opt/leo-vm/vmlinux "$staging/vmlinux"; chmod 444 "$staging/root.ext4" "$staging/vmlinux"; sync; if test ! -d "/cache/$APP_RUNTIME_ID"; then mv "$staging" "/cache/$APP_RUNTIME_ID"; else for file in root.ext4 vmlinux; do test -s "/cache/$APP_RUNTIME_ID/$file" || mv "$staging/$file" "/cache/$APP_RUNTIME_ID/$file"; done; rm -rf "$staging"; fi; sync'
        command(['docker', 'run', '--rm', '--network=none', '--read-only', '--cap-drop=ALL', '--user=0:0',
                 '-v', str(ROOT / 'state/images') + ':/cache', '-e', 'LEO_EXPECTED_RUNTIME=' + identifier,
                 '--entrypoint=/bin/sh', image, '-ec', script], timeout=1200)


def reconcile(config):
    # Persist acknowledgements independently of the image selection: a lost HTTP
    # response must not trigger another restart or strand the node in maintenance.
    if config.get('pendingCompletion'):
        complete(config['master'], config['image'], config.get('updateError'))
        config['pendingCompletion'] = False
        atomic(ROOT / 'config.json', config)
    if config.get('pendingSupervisor'):
        refresh_supervisor()
        config['pendingSupervisor'] = False
        atomic(ROOT / 'config.json', config)


def install(master):
    master = origin(master)
    if (ROOT / 'data/node/identity.json').exists():
        raise RuntimeError('A node identity already exists; use the existing installation')
    release = target(master)
    command(['docker', 'pull', release['image']], timeout=1200)
    code = getpass.getpass('Single-use enrollment code: ').encode()
    command(['docker', 'run', '--rm', '-i', '--user=0:0', '--device=/dev/kvm',
             '-v', f'{ROOT}/data:/data', '--entrypoint=/usr/local/bin/leo', release['image'],
             'node-enroll', master, '/data/node'], data=code)
    atomic(ROOT / 'config.json', {'master': master, 'image': release['image']})


def run():
    config = json.loads((ROOT / 'config.json').read_text())
    # Reconcile an interrupted update against the last committed image selection.
    existing = container()
    if existing is None or not existing['State']['Running'] or existing['Config']['Image'] != config['image']:
        drain(config['master'])
        launch(config['image'])
    config['pendingCompletion'] = True
    atomic(ROOT / 'config.json', config)
    try:
        while not STOP:
            try:
                reconcile(config)
                release = target(config['master'])
                shutdown_timeout = release.get('shutdownTimeoutSeconds', 300)
                if config.get('shutdownTimeoutSeconds') != shutdown_timeout:
                    config['shutdownTimeoutSeconds'] = shutdown_timeout
                    atomic(ROOT / 'config.json', config)
                if release['image'] not in (config['image'], config.get('failedImage')):
                    update(config, release)
                if not STOP:
                    cache_runtimes(config['master'])
            except (OSError, ValueError, RuntimeError):
                print('Node update check failed; retaining current image and data', flush=True)
            for _ in range(1 if config.get('pendingCompletion') or config.get('pendingSupervisor') else 30):
                if STOP:
                    break
                time.sleep(1)
    finally:
        drain(config['master'])
        try:
            if container() is not None:
                command(['docker', 'stop', '--time=15', NAME], timeout=20, interruptible=False)
        except (OSError, RuntimeError):
            remove()


def stopping(_signal, _frame):
    global STOP, STOP_AT
    if STOP_AT is None:
        STOP_AT = time.monotonic()
    STOP = True


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, stopping)
    signal.signal(signal.SIGINT, stopping)
    try:
        if len(sys.argv) == 3 and sys.argv[1] == 'install':
            install(sys.argv[2])
        elif sys.argv[1:] == ['run']:
            run()
        else:
            raise ValueError('Usage: host.py install MASTER | run')
    except (OSError, ValueError, RuntimeError):
        print('Node operation failed. Its identity, disks and previous image are retained.', file=sys.stderr)
        sys.exit(1)
