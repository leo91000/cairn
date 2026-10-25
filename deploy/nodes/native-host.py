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
import tarfile
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

# Managed tools are independent of the conversation's pinned coding engines.
# No host Docker service is installed, joined, stopped or reconfigured.
DOCKER_TOOLS = (
    ('https://download.docker.com/linux/static/stable/x86_64/docker-29.9.0.tgz',
     '33e1ab8b63d14bca449f7a3d30d7f6aa669daa544e60a86b17fec87f61afe2b4',
     {'docker/' + name: 'bin/' + name for name in ('docker', 'dockerd', 'containerd', 'containerd-shim-runc-v2', 'runc', 'docker-init', 'docker-proxy')}),
    ('https://github.com/rootless-containers/rootlesskit/releases/download/v3.3.0/rootlesskit-x86_64.tar.gz',
     '0e757c8f80510d764df8c9a5f169805580b48d02329f0a200900e261ecec27ed', {'rootlesskit': 'bin/rootlesskit'}),
    ('https://github.com/rootless-containers/slirp4netns/releases/download/v1.3.6/slirp4netns-x86_64',
     '411167abceff1fc8a7c562f53ed9f26d3ec8a17df4e2fe6eaa3bd53f1c1e1d48', {'': 'bin/slirp4netns'}),
    ('https://github.com/docker/buildx/releases/download/v0.38.1/buildx-v0.38.1.linux-amd64',
     'e25d5cd675092bb5d1d03632fdc46cdf3fede7b514dc5f023a105e00d3ce5ad4', {'': 'plugins/docker-buildx'}),
    ('https://github.com/docker/compose/releases/download/v5.6.1/docker-compose-linux-x86_64',
     '47737f4236e6d3ccea7291995203f8f018058783b4c3a9469119811a0b5db8c0', {'': 'plugins/docker-compose'}),
)

DOCKER_WRAPPER = '''#!/bin/sh
set -eu
if test "${1:-}" = cairn-reset; then
  if test "$#" -ne 2 || test "$2" != --confirm-delete-state; then
    echo 'To delete ALL conversation Docker images, containers, build cache and volumes, use docker cairn-reset --confirm-delete-state.' >&2
    exit 2
  fi
  exec /run/cairn-entrypoint/cairn native-docker-reset
fi
client_config=${DOCKER_CONFIG:-/home/node/.docker}
mkdir -p "$client_config/cli-plugins"
for plugin in buildx compose; do
  if ! test -e "$client_config/cli-plugins/docker-$plugin"; then
    ln -s "/opt/cairn-docker/plugins/docker-$plugin" "$client_config/cli-plugins/docker-$plugin"
  fi
done
case "${1:-}" in
  compose|buildx)
    plugin=$1
    case "${2:-}" in
      version|--help|help)
        shift
        exec "/opt/cairn-docker/plugins/docker-$plugin" "$@"
        ;;
    esac
    ;;
esac
export DOCKER_HOST=unix:///run/cairn-docker/docker.sock
exec /opt/cairn-docker/bin/docker "$@"
'''


def trusted_mapping_helper(path, required_bit):
    metadata = Path(path).stat()
    if metadata.st_uid != 0 or metadata.st_mode & 0o022 or not metadata.st_mode & stat.S_IXOTH or not stat.S_ISREG(metadata.st_mode):
        return False
    if metadata.st_mode & stat.S_ISUID:
        return True
    try:
        capability = os.getxattr(path, 'security.capability')
    except OSError:
        return False
    if len(capability) not in (20, 24):
        return False
    words = [int.from_bytes(capability[offset:offset + 4], 'little') for offset in range(0, len(capability), 4)]
    expected_revision = 0x02000001 if len(words) == 5 else 0x03000001
    return words[0] == expected_revision and words[1:5] == [required_bit, 0, 0, 0] and (len(words) == 5 or words[5] == 0)


def require_docker_host():
    version = subprocess.run(['systemctl', '--version'], check=True, capture_output=True, text=True, timeout=10).stdout.split()[1]
    if int(version) < 255:
        raise ValueError('Native rootless enclaves require systemd 255 or newer; older versions force NoNewPrivileges on mapping helpers.')
    for name in ('/proc/sys/user/max_user_namespaces', '/proc/sys/kernel/unprivileged_userns_clone'):
        path = Path(name)
        if path.exists() and int(path.read_text().strip()) == 0:
            raise ValueError('Native Docker requires enabled unprivileged user namespaces; global restrictions are not changed automatically.')
    for name, required_bit in (('newuidmap', 1 << 7), ('newgidmap', 1 << 6)):
        path = shutil.which(name)
        if not path:
            raise ValueError('Native Docker requires the distribution uidmap package (newuidmap/newgidmap).')
        if not trusted_mapping_helper(path, required_bit):
            raise ValueError('Native Docker requires root-owned non-writable mapping helpers with setuid or the exact effective CAP_SETUID/CAP_SETGID file capability.')
    for name in ('iptables', 'ip6tables', 'ip', 'usermod', 'useradd', 'groupadd', 'getent'):
        if not shutil.which(name):
            raise ValueError('Native Docker requires iptables, iproute2 and account tools (useradd/groupadd/usermod/getent); install distribution packages before enrollment.')
    for name in ('/etc/subuid', '/etc/subgid'):
        metadata = Path(name).stat()
        if metadata.st_uid != 0 or metadata.st_mode & 0o022:
            raise ValueError('Native Docker requires root-managed subordinate UID/GID databases.')
    if not stat.S_ISCHR(Path('/dev/net/tun').stat().st_mode):
        raise ValueError('Native Docker requires the existing /dev/net/tun device.')
    if tuple(int(part) for part in os.uname().release.split('.')[:2]) < (5, 12):
        raise ValueError('Native Docker bind isolation requires Linux 5.12 or newer.')


def prepare_docker_tools():
    # Bump this package generation when wrapper or generated metadata changes.
    target = INSTALL / 'docker-tools/v2'
    if target.exists():
        manifest = json.loads((target / 'manifest.json').read_text())
        for name, digest in manifest.items():
            path = target / name
            if path.is_symlink() or not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
                raise ValueError('Managed native Docker tool integrity check failed.')
        return target
    target.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
    with tempfile.TemporaryDirectory(prefix='.docker-tools-', dir=target.parent) as temporary:
        stage = Path(temporary)
        stage.chmod(0o755)
        (stage / 'bin').mkdir(mode=0o755)
        (stage / 'plugins').mkdir(mode=0o755)
        for url, digest, names in DOCKER_TOOLS:
            archive = stage / 'download'
            # These public downloads carry no enrollment/node credential.
            with urllib.request.urlopen(url, timeout=60) as response, archive.open('wb') as output:
                total = 0
                while chunk := response.read(65536):
                    total += len(chunk)
                    if total > 256 * 1024 * 1024:
                        raise ValueError('Native Docker tool download exceeds its size bound.')
                    output.write(chunk)
            if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
                raise ValueError('Native Docker tool download digest mismatch.')
            if url.endswith('.tar.gz') or url.endswith('.tgz'):
                with tarfile.open(archive) as bundle:
                    for source, name in names.items():
                        record = bundle.getmember(source)
                        if not record.isfile() or record.size > 256 * 1024 * 1024:
                            raise ValueError('Invalid managed Docker executable.')
                        with bundle.extractfile(record) as source_file, (stage / name).open('xb') as output:
                            shutil.copyfileobj(source_file, output)
            else:
                shutil.copyfile(archive, stage / names[''])
            archive.unlink()
        (stage / 'docker').write_text(DOCKER_WRAPPER)
        # Extract the paired client's API version without ever contacting a daemon.
        # Client version output is emitted even when the explicit missing socket
        # makes the server half of `docker version` fail.
        client = stage / 'bin/docker'
        client.chmod(0o555)
        probe = subprocess.run([str(client), 'version', '--format', '{{json .Client}}'],
            env=dict(PATH=os.defpath, HOME=str(stage), DOCKER_CONFIG=str(stage / 'client-probe'),
                     DOCKER_HOST='unix://' + str(stage / 'unavailable.sock')),
            capture_output=True, text=True, timeout=20)
        version = json.loads(probe.stdout)['ApiVersion']
        if not re.fullmatch(r'[0-9]+\.[0-9]+', version):
            raise ValueError('Managed Docker API version probe failed.')
        (stage / 'engine-api.json').write_text(json.dumps(dict(apiVersion=version)))
        inventory = {}
        for path in [stage / 'docker', stage / 'engine-api.json', *(stage / 'bin').iterdir(), *(stage / 'plugins').iterdir()]:
            path.chmod(0o444 if path.name == 'engine-api.json' else 0o555)
            inventory[str(path.relative_to(stage))] = hashlib.sha256(path.read_bytes()).hexdigest()
        atomic_json(stage / 'manifest.json', inventory)
        (stage / 'manifest.json').chmod(0o444)
        os.rename(stage, target)
    return target


def configure_docker_apparmor(target):
    restriction = Path('/proc/sys/kernel/apparmor_restrict_unprivileged_userns')
    if not restriction.exists() or restriction.read_text().strip() != '1':
        return
    parser = shutil.which('apparmor_parser')
    if not parser:
        raise ValueError('Restricted user namespaces require AppArmor tooling for the managed RootlessKit profile.')
    executable = str(target / 'bin/rootlesskit')
    if any(character in executable for character in ('"', '\n', '\r')):
        raise ValueError('Invalid managed RootlessKit path.')
    profile = Path('/etc/apparmor.d/cairn-native-rootlesskit')
    profile.write_text('abi <abi/4.0>,\ninclude <tunables/global>\n'
                       'profile cairn-native-rootlesskit "' + executable + '" flags=(unconfined) {\n  userns,\n}\n'
                       'profile cairn-native-rootlesskit-enclave "/opt/cairn-docker/bin/rootlesskit" flags=(unconfined) {\n  userns,\n}\n')
    profile.chmod(0o644)
    subprocess.run([parser, '--replace', str(profile)], check=True, capture_output=True, timeout=30)


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
    if value.get('os') != 'linux' or value.get('arch') != 'x86_64' or value.get('nodeProtocol') != 2:
        raise ValueError('Native release platform/protocol is incompatible.')
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


def tool_inventory(manifest):
    if manifest.get('version') != 1 or manifest.get('os') != 'linux' or manifest.get('arch') != 'x86_64':
        raise ValueError('Unknown native tool manifest.')
    expected = {'bin/codex', 'bin/codex-code-mode-host', 'bin/claude', 'bin/opencode', 'bin/node', 'bin/gh'}
    entries = manifest.get('files', [])
    paths = {entry.get('path') for entry in entries}
    if paths != expected or len(entries) != len(paths):
        raise ValueError('Incomplete native tool inventory.')
    return entries


def unpack_tools(archive, directory):
    destination = directory / 'tools'
    destination.mkdir(mode=0o755)
    destination.chmod(0o755)
    with zipfile.ZipFile(archive) as bundle:
        records = bundle.infolist()
        names = [record.filename for record in records]
        if sum(record.file_size for record in records) > 1024 * 1024 * 1024:
            raise ValueError('Native tools exceed extracted size budget.')
        if bundle.getinfo('manifest.json').file_size > 65536:
            raise ValueError('Native tool manifest exceeds limit.')
        manifest = json.loads(bundle.read('manifest.json'))
        entries = tool_inventory(manifest)
        expected = {entry['path'] for entry in entries}
        if len(names) != len(set(names)) or set(names) != expected | {'bin/', 'manifest.json'}:
            raise ValueError('Unexpected native tool bundle files.')
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
    entries = tool_inventory(manifest)
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
        executables = [(directory / 'cairn', '--version')]
        manifest = json.loads((directory / 'tools/manifest.json').read_text())
        for entry in tool_inventory(manifest):
            argument = '--help' if entry['path'] == 'bin/codex-code-mode-host' else '--version'
            executables.append((directory / 'tools' / entry['path'], argument))
        for executable, argument in executables:
            subprocess.run([str(executable), argument], check=True, timeout=30, cwd=home,
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
            'CAIRN_NATIVE_SUPERVISOR_UNIT': SERVICE, 'APP_RUNTIME_ID': release['runtimeId'],
            'CAIRN_NATIVE_DOCKER_TOOLS': str(INSTALL / 'docker-tools/v2')}


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
    if int(version) < 255:
        raise ValueError('Native rootless execution requires systemd 255 or newer.')
    try:
        controllers = set((CGROUP_ROOT / 'cgroup.controllers').read_text().split())
    except OSError as error:
        raise ValueError('Native execution requires cgroup v2 with CPU, memory and PID controllers.') from error
    if not {'cpu', 'memory', 'pids'} <= controllers:
        raise ValueError('Native execution requires cgroup v2 with CPU, memory and PID controllers.')
    require_storage()
    require_docker_host()
    configure_docker_apparmor(prepare_docker_tools())
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
After=network-online.target cairn-node-gateway.service
Wants=network-online.target cairn-node-gateway.service
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
    print('Native node enrolled in protected mode. Enable it in Cairn and select trusted people and native agents. Administrator mode additionally requires sudo with the standard /etc/sudoers.d include; it is confirmed from the node details and gives control of the real host.')


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


def activate(candidate, child):
    terminate(child)
    select(candidate)
    trial = start_node(candidate)
    if trial.poll() is None and health(candidate):
        return trial
    terminate(trial)
    raise RuntimeError('Native candidate failed health checks; candidate retained for retry.')


def complete_update(manager, token, current):
    pending = ROOT / 'update-pending.json'
    if not pending.exists() or json.loads(pending.read_text())['candidate'] != current.name:
        return
    if not health(current):
        raise RuntimeError('Pending native candidate is not healthy.')
    json_request(manager, '/internal/nodes/maintenance', {'action': 'complete', 'image': '', 'error': None}, token)
    pending.unlink()


def run():
    require_storage()
    require_docker_host()
    configure_docker_apparmor(prepare_docker_tools())
    manager = origin(json.loads((ROOT / 'settings.json').read_text())['manager'])
    identity = json.loads((ROOT / 'data/node/identity.json').read_text())
    token = identity['token']
    current = (INSTALL / 'current').resolve(strict=True)
    child = None
    pending_update = ROOT / 'update-pending.json'
    try:
        child = start_node(current)
        if not health(current):
            raise RuntimeError('Native controller failed startup health checks.')
        while not STOP:
            if child.poll() is not None:
                raise RuntimeError('Native node process exited unexpectedly.')
            draining = False
            try:
                complete_update(manager, token, current)
                release = validate_release(json_request(manager, '/internal/nodes/native-release', token=token))
                installed = json.loads((current / 'release.json').read_text())
                same = release['sha256'] == installed['sha256'] and release['toolsSha256'] == installed['toolsSha256']
                if not same:
                    candidate = prepare(manager, release, token=token)
                    require_docker_host()
                    configure_docker_apparmor(prepare_docker_tools())
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
                    atomic_json(pending_update, {'candidate': candidate.name})
                    previous = current
                    child = activate(candidate, child)
                    current = candidate
                    try:
                        collect_versions(current, previous)
                    except (OSError, ValueError, KeyError):
                        print('Native version collection postponed; local versions retained.', file=sys.stderr)
                    complete_update(manager, token, current)
            except (OSError, ValueError, RuntimeError, zipfile.BadZipFile, KeyError, TypeError, urllib.error.URLError, subprocess.SubprocessError) as error:
                # Do not print request bodies, response bodies, identities or credentials.
                print('Native update postponed: ' + type(error).__name__, file=sys.stderr)
                if child.poll() is not None:
                    raise RuntimeError('Native node is no longer running.') from None
                if draining and not pending_update.exists():
                    try:
                        json_request(manager, '/internal/nodes/maintenance', {'action': 'complete', 'image': '', 'error': 'Native update failed; inspect the retained candidate.'}, token)
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
