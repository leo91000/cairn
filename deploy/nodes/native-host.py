#!/usr/bin/env python3
"""Root-owned native node installation and versioned, verified updates.

The manager's HTTPS origin authenticates the release manifest. Exact binary and
tool digests authenticate its payloads. Credentials never enter command arguments.
"""
import getpass
import grp
import hashlib
import http.client
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile

ROOT = Path('/var/lib/cairn-native')
INSTALL = Path('/opt/cairn-native')
SERVICE = 'cairn-native-node.service'
CGROUP_ROOT = Path('/sys/fs/cgroup')
STOP = False


class UnixControllerConnection(http.client.HTTPConnection):
    def __init__(self, path, timeout=3):
        super().__init__('localhost', timeout=timeout)
        self.path = str(path)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('Native downloads do not follow redirects.')


HTTP = urllib.request.build_opener(NoRedirect)


def origin(value):
    parsed = urllib.parse.urlsplit(value)
    secure = parsed.scheme == 'https' or (parsed.scheme == 'http' and parsed.hostname in ('localhost', '127.0.0.1', '::1'))
    if not secure or not parsed.hostname or parsed.username or parsed.password or parsed.path not in ('', '/') or parsed.query or parsed.fragment:
        raise ValueError('Use a bare manager HTTPS origin.')
    return value.rstrip('/')


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor, temporary = tempfile.mkstemp(dir=path.parent)
    try:
        with os.fdopen(descriptor, 'w') as output:
            json.dump(value, output)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        Path(temporary).unlink(missing_ok=True)


def request(manager, endpoint, payload=None, token=None, limit=1_000_000):
    if not endpoint.startswith('/internal/nodes/'):
        raise ValueError('Unexpected native endpoint.')
    headers = {'Content-Type': 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    data = json.dumps(payload or {}).encode()
    query = urllib.request.Request(origin(manager) + endpoint, data=data, headers=headers, method='POST')
    response = HTTP.open(query, timeout=30)
    return response


def json_request(manager, endpoint, payload=None, token=None):
    with request(manager, endpoint, payload, token) as response:
        data = response.read(1_000_001)
        if len(data) > 1_000_000:
            raise ValueError('Native metadata exceeds limit.')
        return json.loads(data)


def validate_release(value):
    for field in ('sha256', 'toolsSha256'):
        if not re.fullmatch('[0-9a-f]{64}', value.get(field, '')):
            raise ValueError('Invalid native release digest.')
    if value.get('os') != 'linux' or value.get('arch') != 'x86_64' or not value.get('protocolMin', 99) <= 2 <= value.get('protocolMax', 0):
        raise ValueError('Native release platform/protocol is incompatible.')
    if not isinstance(value.get('dataEpoch'), int) or value['dataEpoch'] < 1:
        raise ValueError('Missing installation data compatibility epoch.')
    if value.get('binaryPath') != '/internal/nodes/native-binary' or value.get('toolsPath') != '/internal/nodes/native-tools':
        raise ValueError('Unexpected native artifact endpoint.')
    for field, maximum in (('size', 256 * 1024 * 1024), ('toolsSize', 512 * 1024 * 1024)):
        if not isinstance(value.get(field), int) or not 0 < value[field] <= maximum:
            raise ValueError('Invalid native artifact size.')
    if not re.fullmatch('[A-Za-z0-9._-]{1,100}', value.get('runtimeId', '')):
        raise ValueError('Invalid native runtime identifier.')
    return value


def download(manager, release, directory, tools=False, code=None, token=None):
    digest = release['toolsSha256' if tools else 'sha256']
    size = release['toolsSize' if tools else 'size']
    endpoint = release['toolsPath' if tools else 'binaryPath']
    target = directory / ('tools.zip' if tools else 'cairn')
    hasher = hashlib.sha256()
    received = 0
    with request(manager, endpoint, {'code': code} if code else {}, token) as response, target.open('xb') as output:
        while True:
            chunk = response.read(65536)
            if not chunk:
                break
            received += len(chunk)
            if received > size:
                raise ValueError('Native download exceeds approved size.')
            hasher.update(chunk)
            output.write(chunk)
        output.flush()
        os.fsync(output.fileno())
    if received != size or hasher.hexdigest() != digest:
        raise ValueError('Native download integrity check failed.')
    target.chmod(0o555 if not tools else 0o400)
    return target


def unpack_tools(archive, directory):
    expected = {'bin/codex', 'bin/claude', 'bin/opencode', 'bin/node', 'bin/gh'}
    destination = directory / 'tools'
    destination.mkdir(mode=0o755)
    destination.chmod(0o755)
    with zipfile.ZipFile(archive) as bundle:
        records = bundle.infolist()
        names = [record.filename for record in records]
        if len(names) != len(set(names)) or set(names) != expected | {'bin/', 'manifest.json'}:
            raise ValueError('Unexpected native tool bundle files.')
        if sum(record.file_size for record in records) > 1024 * 1024 * 1024:
            raise ValueError('Native tools exceed extracted size budget.')
        if bundle.getinfo('manifest.json').file_size > 65536:
            raise ValueError('Native tool manifest exceeds limit.')
        manifest = json.loads(bundle.read('manifest.json'))
        if manifest.get('version') != 1 or manifest.get('os') != 'linux' or manifest.get('arch') != 'x86_64':
            raise ValueError('Unknown native tool manifest.')
        entries = manifest.get('files', [])
        if len(entries) != len(expected) or {entry.get('path') for entry in entries} != expected:
            raise ValueError('Incomplete native tool inventory.')
        (destination / 'bin').mkdir(mode=0o755)
        (destination / 'bin').chmod(0o755)
        for entry in entries:
            record = bundle.getinfo(entry['path'])
            if stat.S_ISLNK(record.external_attr >> 16) or record.is_dir() or record.file_size != entry.get('size'):
                raise ValueError('Native tool is not a regular executable.')
            hasher = hashlib.sha256()
            with bundle.open(record) as source, (destination / entry['path']).open('xb') as output:
                while True:
                    chunk = source.read(65536)
                    if not chunk:
                        break
                    hasher.update(chunk)
                    output.write(chunk)
                output.flush()
                os.fsync(output.fileno())
            if hasher.hexdigest() != entry.get('sha256'):
                raise ValueError('Native tool integrity check failed.')
            (destination / entry['path']).chmod(0o555)
        atomic_json(destination / 'manifest.json', manifest)


def verify_installed(directory, release):
    def verified(path, size, digest):
        if path.is_symlink() or not path.is_file() or path.stat().st_size != size:
            raise ValueError('Installed native file changed.')
        with path.open('rb') as source:
            if hashlib.file_digest(source, 'sha256').hexdigest() != digest:
                raise ValueError('Installed native integrity check failed.')
    verified(directory / 'cairn', release['size'], release['sha256'])
    manifest = json.loads((directory / 'tools/manifest.json').read_text())
    expected = {'bin/codex', 'bin/claude', 'bin/opencode', 'bin/node', 'bin/gh'}
    entries = manifest.get('files', [])
    if len(entries) != len(expected) or {entry.get('path') for entry in entries} != expected:
        raise ValueError('Installed native tool inventory changed.')
    for entry in entries:
        verified(directory / 'tools' / entry['path'], entry['size'], entry['sha256'])


def collect_versions(current, previous=None):
    # Retain the current/previous release and every engine pinned by local work.
    # Unreadable metadata aborts collection rather than discarding a needed runtime.
    pinned = set()
    native = ROOT / 'state/native'
    if native.exists():
        for path in native.glob('*/runtime.json'):
            pinned.add(json.loads(path.read_text())['runtimeId'])
    keep = {current.resolve()}
    if previous:
        keep.add(previous.resolve())
    for directory in (INSTALL / 'versions').iterdir():
        if directory.is_symlink() or not directory.is_dir() or not re.fullmatch('[0-9a-f]{64}-[0-9a-f]{64}', directory.name):
            continue
        release = json.loads((directory / 'release.json').read_text())
        if directory.resolve() not in keep and release['runtimeId'] not in pinned:
            shutil.rmtree(directory)


def prepare(manager, release, code=None, token=None):
    release = validate_release(release)
    versions = INSTALL / 'versions'
    versions.mkdir(mode=0o755, parents=True, exist_ok=True)
    versions.chmod(0o755)
    name = release['sha256'] + '-' + release['toolsSha256']
    destination = versions / name
    if destination.exists():
        if json.loads((destination / 'release.json').read_text()) != release:
            raise ValueError('Installed native release metadata changed.')
        verify_installed(destination, release)
        return destination
    temporary = Path(tempfile.mkdtemp(dir=versions, prefix='.candidate-'))
    try:
        binary = download(manager, release, temporary, code=code, token=token)
        archive = download(manager, release, temporary, tools=True, code=code, token=token)
        unpack_tools(archive, temporary)
        archive.unlink()
        # Version checks catch ABI/library mismatches before stopping an active node.
        subprocess.run([str(binary), '--version'], check=True, timeout=15, stdout=subprocess.DEVNULL)
        atomic_json(temporary / 'release.json', release)
        temporary.chmod(0o755)
        os.rename(temporary, destination)
        return destination
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)


def verify_workload_tools(directory):
    workload = pwd.getpwnam('cairn-workload')
    with tempfile.TemporaryDirectory(prefix='cairn-native-probe-') as temporary:
        home = Path(temporary)
        os.chown(home, workload.pw_uid, workload.pw_gid)
        environment = {'PATH': str(directory / 'tools/bin') + ':/usr/bin:/bin', 'HOME': str(home), 'LANG': 'C.UTF-8'}
        for executable in [directory / 'cairn'] + [directory / 'tools/bin' / name for name in ('codex', 'claude', 'opencode', 'node', 'gh')]:
            subprocess.run([str(executable), '--version'], check=True, timeout=30, cwd=home,
                           user=workload.pw_uid, group=workload.pw_gid, extra_groups=[], env=environment,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def select(directory):
    temporary = INSTALL / '.current-new'
    temporary.unlink(missing_ok=True)
    temporary.symlink_to(directory)
    os.replace(temporary, INSTALL / 'current')
    descriptor = os.open(INSTALL, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def environment(directory):
    release = json.loads((directory / 'release.json').read_text())
    return {'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LANG': 'C.UTF-8', 'DATA_DIR': str(ROOT / 'data'),
            'RUNNER_STATE_DIR': str(ROOT / 'state'), 'RUNNER_URL': 'http://127.0.0.1:4311', 'RUNNER_BIND': '127.0.0.1:4311',
            'CAIRN_NODE_BACKEND': 'native', 'CAIRN_NATIVE_TOOLS': str(directory / 'tools'),
            'CAIRN_NATIVE_SUPERVISOR_UNIT': SERVICE, 'APP_RUNTIME_ID': release['runtimeId']}


def require_storage():
    for binary in ('mkfs.ext4', 'e2fsck', 'resize2fs'):
        if shutil.which(binary) is None:
            raise ValueError('Native ublk storage requires e2fsprogs (' + binary + ').')
    if not Path('/sys/module/ublk_drv').is_dir():
        try:
            subprocess.run(['modprobe', 'ublk_drv'], check=True, capture_output=True, timeout=10)
        except (OSError, subprocess.SubprocessError) as error:
            raise ValueError('Native storage requires a kernel with the ublk_drv module. Install the matching kernel modules before enrolling.') from error
    try:
        control = Path('/dev/ublk-control').stat()
    except OSError as error:
        raise ValueError('Native storage requires /dev/ublk-control.') from error
    if not stat.S_ISCHR(control.st_mode):
        raise ValueError('Native storage control must be a character device.')


def install(manager):
    manager = origin(manager)
    if (ROOT / 'data/node/identity.json').exists():
        raise ValueError('Native node is already enrolled.')
    version = subprocess.run(['systemctl', '--version'], check=True, capture_output=True, text=True).stdout.split()[1]
    if int(version) < 252:
        raise ValueError('Native execution requires systemd 252 or newer.')
    try:
        controllers = set((CGROUP_ROOT / 'cgroup.controllers').read_text().split())
    except OSError as error:
        raise ValueError('Native execution requires cgroup v2 with CPU, memory and PID controllers.') from error
    if not {'cpu', 'memory', 'pids'} <= controllers:
        raise ValueError('Native execution requires cgroup v2 with CPU, memory and PID controllers.')
    require_storage()
    ROOT.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(ROOT, 0o700)
    (ROOT / 'data').mkdir(mode=0o700, exist_ok=True)
    (ROOT / 'state').mkdir(mode=0o700, exist_ok=True)
    code = getpass.getpass('Native node enrollment code: ').strip()
    if not re.fullmatch('[A-Za-z0-9_-]{43}', code):
        raise ValueError('Invalid native enrollment code.')
    release = json_request(manager, '/internal/nodes/native-release', {'code': code})
    directory = prepare(manager, release, code=code)
    try:
        workload = pwd.getpwnam('cairn-workload')
    except KeyError:
        subprocess.run(['useradd', '--system', '--no-create-home', '--shell', '/usr/sbin/nologin', 'cairn-workload'], check=True)
        workload = pwd.getpwnam('cairn-workload')
    if workload.pw_uid == 0:
        raise ValueError('Native workload user must not be root.')
    for group in ('render', 'video'):
        try:
            grp.getgrnam(group)
        except KeyError:
            continue
        subprocess.run(['usermod', '-a', '-G', group, 'cairn-workload'], check=True)
    verify_workload_tools(directory)
    subprocess.run([str(directory / 'cairn'), 'node-enroll', manager, str(ROOT / 'data/node')],
                   input=code.encode(), env=environment(directory), check=True)
    atomic_json(ROOT / 'settings.json', {'manager': manager})
    select(directory)
    Path('/etc/systemd/system/' + SERVICE).write_text('''[Unit]
Description=Cairn native execution node
After=network-online.target
Wants=network-online.target
[Service]
Type=simple
ExecStart=/usr/bin/python3 /opt/cairn-native/host.py run
User=root
UMask=0077
Restart=on-failure
RestartSec=10
TimeoutStopSec=300
KillMode=control-group
[Install]
WantedBy=multi-user.target
''')
    subprocess.run(['systemctl', 'daemon-reload'], check=True)
    subprocess.run(['systemctl', 'enable', '--now', SERVICE], check=True)
    print('Native node enrolled. Enable it in Cairn and explicitly select trusted callers and native agents.')


def health(directory, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and not STOP:
        try:
            connection = UnixControllerConnection(ROOT / 'state/controller.sock')
            try:
                connection.request('GET', '/health')
                response = connection.getresponse()
                value = json.loads(response.read(65536))
            finally:
                connection.close()
            release = json.loads((directory / 'release.json').read_text())
            if response.status == 200 and value.get('status') == 'ok' and value.get('backend') == 'native' and value.get('runtimeId') == release['runtimeId']:
                return True
        except (OSError, ValueError, http.client.HTTPException):
            pass
        time.sleep(0.5)
    return False


def terminate(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=300)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
            # BindsTo and the independent lease guardian fence each owned unit.


def start_node(directory):
    return subprocess.Popen([str(directory / 'cairn'), 'node-daemon', str(ROOT / 'data/node')], env=environment(directory))


def activate(candidate, previous, child):
    terminate(child)
    trial = start_node(candidate)
    if trial.poll() is None and health(candidate):
        select(candidate)
        return trial, candidate, None
    terminate(trial)
    restored = start_node(previous)
    if not health(previous):
        terminate(restored)
        raise RuntimeError('Native update and rollback both failed health checks.')
    return restored, previous, 'Native candidate failed health checks; previous compatible version restored.'


def run():
    require_storage()
    manager = origin(json.loads((ROOT / 'settings.json').read_text())['manager'])
    identity = json.loads((ROOT / 'data/node/identity.json').read_text())
    token = identity['token']
    current = (INSTALL / 'current').resolve(strict=True)
    child = None
    failed = None
    try:
        child = start_node(current)
        if not health(current):
            raise RuntimeError('Native controller failed startup health checks.')
        while not STOP:
            if child.poll() is not None:
                raise RuntimeError('Native node process exited unexpectedly.')
            draining = False
            try:
                release = validate_release(json_request(manager, '/internal/nodes/native-release', token=token))
                installed = json.loads((current / 'release.json').read_text())
                same = release['sha256'] == installed['sha256'] and release['toolsSha256'] == installed['toolsSha256']
                candidate_id = release['sha256'] + '-' + release['toolsSha256']
                if not same and candidate_id != failed:
                    if release['dataEpoch'] != installed['dataEpoch']:
                        raise ValueError('Native update needs a reviewed installation data epoch transition.')
                    candidate = prepare(manager, release, token=token)
                    verify_workload_tools(candidate)
                    if STOP:
                        break
                    json_request(manager, '/internal/nodes/maintenance', {'action': 'drain'}, token)
                    draining = True
                    deadline = time.monotonic() + 280
                    while time.monotonic() < deadline and not STOP:
                        status = json_request(manager, '/internal/nodes/maintenance', {'action': 'status'}, token)
                        if status.get('maintenance') == 'ready-to-update' and not status.get('maintenanceError'):
                            break
                        time.sleep(1)
                    else:
                        raise RuntimeError('Native update drain did not finish; preserving current version.')
                    previous = current
                    child, selected, error = activate(candidate, current, child)
                    current = selected
                    if not error:
                        try:
                            collect_versions(current, previous)
                        except (OSError, ValueError, KeyError):
                            print('Native version collection postponed; local versions retained.', file=sys.stderr)
                    if error:
                        failed = candidate_id
                    json_request(manager, '/internal/nodes/maintenance', {'action': 'complete', 'image': '', 'error': error}, token)
            except (OSError, ValueError, RuntimeError, zipfile.BadZipFile, KeyError, TypeError, urllib.error.URLError, subprocess.SubprocessError) as error:
                # Do not print request bodies, response bodies, identities or credentials.
                print('Native update postponed: ' + type(error).__name__, file=sys.stderr)
                if child.poll() is not None:
                    raise RuntimeError('Native node is no longer running.') from None
                if draining:
                    try:
                        json_request(manager, '/internal/nodes/maintenance', {'action': 'complete', 'image': '', 'error': 'Native update failed; compatible prior version retained.'}, token)
                    except (OSError, ValueError):
                        pass
            for _ in range(300):
                if STOP:
                    break
                time.sleep(1)
    finally:
        if child:
            terminate(child)


def stop(_signal, _frame):
    global STOP
    STOP = True


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        if os.geteuid() != 0:
            raise ValueError('Use the root-owned native node service.')
        if len(sys.argv) == 3 and sys.argv[1] == 'install':
            install(sys.argv[2])
        elif len(sys.argv) == 2 and sys.argv[1] == 'run':
            run()
        else:
            raise ValueError('Use install <manager HTTPS origin> or run.')
    except Exception as error:
        print('Native node failed: ' + type(error).__name__, file=sys.stderr)
        sys.exit(1)
