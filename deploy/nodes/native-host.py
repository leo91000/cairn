#!/usr/bin/env python3
"""Root-owned native node installation and versioned, verified updates.

The manager's HTTPS origin authenticates the release manifest. Exact binary and
tool digests authenticate its payloads. Credentials never enter command arguments.
"""
import ctypes
import errno
from contextlib import ExitStack
import getpass
import fcntl
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
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import zipfile

ROOT = Path('/var/lib/cairn-native')
INSTALL = Path('/opt/cairn-native')
if os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') == '1':
    # Disposable qualification hosts run the pool migration on a private fixture.
    ROOT = Path(os.environ.get('CAIRN_NATIVE_ROOT', ROOT))
    INSTALL = Path(os.environ.get('CAIRN_NATIVE_INSTALL', INSTALL))
SERVICE = 'cairn-native-node.service'
UNIT_DIRECTORY = Path('/etc/systemd/system')
NATIVE_LOOP_SERVICE = 'cairn-native-data-loop.service'
FIXTURE_LOOP_SERVICE = re.compile(r'cairn-native-migration-fixture-[0-9a-f]{16}\.service')
LOOP_SERVICE = NATIVE_LOOP_SERVICE
if os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') == '1' and os.environ.get('CAIRN_NATIVE_FIXTURE_LOOP_SERVICE'):
    LOOP_SERVICE = os.environ['CAIRN_NATIVE_FIXTURE_LOOP_SERVICE']
    if not FIXTURE_LOOP_SERVICE.fullmatch(LOOP_SERVICE):
        raise ValueError('Invalid disposable fixture loop service name.')
CGROUP_ROOT = Path('/sys/fs/cgroup')
STOP = False

# Only fixed, credential-free prerequisite messages reach the CLI. Other
# exceptions can carry manager URLs, credentials or response bodies and remain
# masked by the entrypoint's generic handler.
PREREQUISITES = {
    'systemd': 'Native rootless enclaves require systemd 255 or newer; older versions force NoNewPrivileges on mapping helpers.',
    'userns': 'Native Docker requires enabled unprivileged user namespaces; global restrictions are not changed automatically.',
    'uidmap': 'Native Docker requires the distribution uidmap package (newuidmap/newgidmap).',
    'mapping': 'Native Docker requires root-owned non-writable mapping helpers with setuid or the exact effective CAP_SETUID/CAP_SETGID file capability.',
    'accounts': 'Native Docker requires iptables, iproute2 and account tools (useradd/groupadd/usermod/getent); install distribution packages before enrollment.',
    'subids': 'Native Docker requires root-managed /etc/subuid and /etc/subgid databases.',
    'tun': 'Native Docker requires the existing /dev/net/tun character device.',
    'kernel': 'Native storage requires Linux 6.11 or newer for legacy device retirement and directory consumer fencing.',
    'apparmor': 'Restricted user namespaces require AppArmor tooling and a writable root-managed profile for the managed RootlessKit binary.',
    'e2fsprogs': 'Native storage migration requires e2fsprogs (mkfs.ext4/e2fsck/resize2fs).',
    'rsync': 'Native directory seeding and verified migration require rsync.',
    'quota': 'Mount a dedicated XFS (reflink=1) or ext4 filesystem with prjquota at /var/lib/cairn-native/state/native-data, on a different filesystem from controller state. See docs/NATIVE-NODES.md.',
    'ublk': 'Native storage requires the ublk_drv kernel module and /dev/ublk-control character device; install the matching kernel modules before enrolling.',
    'cgroup': 'Native execution requires cgroup v2 with CPU, memory, PID and IO controllers.',
}


class PrerequisiteError(ValueError):
    def __init__(self, requirement):
        self.diagnostic = PREREQUISITES[requirement]
        super().__init__(self.diagnostic)


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


def require_systemd():
    try:
        version = subprocess.run(['systemctl', '--version'], check=True, capture_output=True, text=True, timeout=10).stdout.split()[1]
        version = int(version)
    except (OSError, subprocess.SubprocessError, ValueError, IndexError) as error:
        raise PrerequisiteError('systemd') from error
    if version < 255:
        raise PrerequisiteError('systemd')


def require_docker_host():
    require_systemd()
    for name in ('/proc/sys/user/max_user_namespaces', '/proc/sys/kernel/unprivileged_userns_clone'):
        path = Path(name)
        try:
            disabled = path.exists() and int(path.read_text().strip()) == 0
        except (OSError, ValueError) as error:
            raise PrerequisiteError('userns') from error
        if disabled:
            raise PrerequisiteError('userns')
    for name, required_bit in (('newuidmap', 1 << 7), ('newgidmap', 1 << 6)):
        path = shutil.which(name)
        if not path:
            raise PrerequisiteError('uidmap')
        try:
            trusted = trusted_mapping_helper(path, required_bit)
        except OSError as error:
            raise PrerequisiteError('mapping') from error
        if not trusted:
            raise PrerequisiteError('mapping')
    for name in ('iptables', 'ip6tables', 'ip', 'usermod', 'useradd', 'groupadd', 'getent'):
        if not shutil.which(name):
            raise PrerequisiteError('accounts')
    for name in ('/etc/subuid', '/etc/subgid'):
        try:
            metadata = Path(name).stat()
        except OSError as error:
            raise PrerequisiteError('subids') from error
        if metadata.st_uid != 0 or metadata.st_mode & 0o022:
            raise PrerequisiteError('subids')
    try:
        tun = Path('/dev/net/tun').stat()
    except OSError as error:
        raise PrerequisiteError('tun') from error
    if not stat.S_ISCHR(tun.st_mode):
        raise PrerequisiteError('tun')
    try:
        kernel = tuple(int(part) for part in os.uname().release.split('.')[:2])
    except ValueError as error:
        raise PrerequisiteError('kernel') from error
    if kernel < (5, 12):
        raise PrerequisiteError('kernel')


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
        raise PrerequisiteError('apparmor')
    executable = str(target / 'bin/rootlesskit')
    if any(character in executable for character in ('"', '\n', '\r')):
        raise ValueError('Invalid managed RootlessKit path.')
    profile = Path('/etc/apparmor.d/cairn-native-rootlesskit')
    try:
        profile.write_text('abi <abi/4.0>,\ninclude <tunables/global>\n'
                           'profile cairn-native-rootlesskit "' + executable + '" flags=(unconfined) {\n  userns,\n}\n'
                           'profile cairn-native-rootlesskit-enclave "/opt/cairn-docker/bin/rootlesskit" flags=(unconfined) {\n  userns,\n}\n')
        profile.chmod(0o644)
        subprocess.run([parser, '--replace', str(profile)], check=True, capture_output=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as error:
        raise PrerequisiteError('apparmor') from error


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


def sync_directory(path):
    directory = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def atomic_bytes(path, value, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor, temporary = tempfile.mkstemp(dir=path.parent)
    try:
        with os.fdopen(descriptor, 'wb') as output:
            os.fchmod(output.fileno(), mode)
            output.write(value)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        Path(temporary).unlink(missing_ok=True)


def atomic_json(path, value):
    atomic_bytes(path, json.dumps(value).encode())


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


# The first tool bundle is capped by the limits of supervisors installed before
# this one. Tools that do not fit it ship in a separate bundle with its own limits.
EXTRA_FIELDS = ('extraToolsSha256', 'extraToolsSize', 'extraToolsPath')
EXTRA_EXTRACTED = 512 * 1024 * 1024
EXTRA_COMPRESSED = 256 * 1024 * 1024
ARTIFACTS = {
    'binary': ('sha256', 'size', 'binaryPath', 'cairn', 0o555),
    'tools': ('toolsSha256', 'toolsSize', 'toolsPath', 'tools.zip', 0o400),
    'extra': ('extraToolsSha256', 'extraToolsSize', 'extraToolsPath', 'extra-tools.zip', 0o400),
}


def has_extra_tools(release):
    return 'extraToolsSha256' in release


def version_name(release):
    name = release['sha256'] + '-' + release['toolsSha256']
    return name + '-' + release['extraToolsSha256'] if has_extra_tools(release) else name


def validate_release(value):
    for field in ('sha256', 'toolsSha256'):
        if not re.fullmatch('[0-9a-f]{64}', value.get(field, '')):
            raise ValueError('Invalid native release digest.')
    if value.get('os') != 'linux' or value.get('arch') != 'x86_64' or value.get('nativeHostProtocol') != 2 or value.get('executionProtocol') != 2:
        raise ValueError('Native release platform/protocol is incompatible.')
    if value.get('binaryPath') != '/internal/nodes/native-binary' or value.get('toolsPath') != '/internal/nodes/native-tools':
        raise ValueError('Unexpected native artifact endpoint.')
    for field, maximum in (('size', 256 * 1024 * 1024), ('toolsSize', 512 * 1024 * 1024)):
        if not isinstance(value.get(field), int) or not 0 < value[field] <= maximum:
            raise ValueError('Invalid native artifact size.')
    # Managers older than the extra tool bundle omit it; a release that names it names all of it.
    if any(field in value for field in EXTRA_FIELDS):
        if not re.fullmatch('[0-9a-f]{64}', value.get('extraToolsSha256', '')):
            raise ValueError('Invalid native release digest.')
        if value.get('extraToolsPath') != '/internal/nodes/native-extra-tools':
            raise ValueError('Unexpected native artifact endpoint.')
        if not isinstance(value.get('extraToolsSize'), int) or not 0 < value['extraToolsSize'] <= EXTRA_COMPRESSED:
            raise ValueError('Invalid native artifact size.')
    if not re.fullmatch('[A-Za-z0-9._-]{1,100}', value.get('runtimeId', '')):
        raise ValueError('Invalid native runtime identifier.')
    return value


def download(manager, release, directory, tools=False, code=None, token=None, extra=False):
    digest_field, size_field, endpoint_field, name, mode = ARTIFACTS['extra' if extra else 'tools' if tools else 'binary']
    digest = release[digest_field]
    size = release[size_field]
    endpoint = release[endpoint_field]
    target = directory / name
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
    target.chmod(mode)
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


def extra_tool_inventory(manifest):
    if manifest.get('version') != 1 or manifest.get('os') != 'linux' or manifest.get('arch') != 'x86_64' \
            or manifest.get('bundle') != 'extra':
        raise ValueError('Unknown native tool manifest.')
    entries = manifest.get('files', [])
    paths = {entry.get('path') for entry in entries}
    if paths != {'bin/agy'} or len(entries) != len(paths):
        raise ValueError('Incomplete native tool inventory.')
    return entries


def unpack_tools(archive, directory):
    destination = directory / 'tools'
    destination.mkdir(mode=0o755)
    destination.chmod(0o755)
    (destination / 'bin').mkdir(mode=0o755)
    (destination / 'bin').chmod(0o755)
    extract_tools(archive, destination, tool_inventory, 1024 * 1024 * 1024, 'manifest.json')


def unpack_extra_tools(archive, directory):
    # Extra tools join the first bundle's bin directory, which workloads already
    # receive, but never replace one of its files.
    extract_tools(archive, directory / 'tools', extra_tool_inventory, EXTRA_EXTRACTED, 'extra-manifest.json')


def extract_tools(archive, destination, inventory, budget, manifest_name):
    with zipfile.ZipFile(archive) as bundle:
        records = bundle.infolist()
        names = [record.filename for record in records]
        if sum(record.file_size for record in records) > budget:
            raise ValueError('Native tools exceed extracted size budget.')
        if bundle.getinfo('manifest.json').file_size > 65536:
            raise ValueError('Native tool manifest exceeds limit.')
        manifest = json.loads(bundle.read('manifest.json'))
        entries = inventory(manifest)
        expected = {entry['path'] for entry in entries}
        if len(names) != len(set(names)) or set(names) != expected | {'bin/', 'manifest.json'}:
            raise ValueError('Unexpected native tool bundle files.')
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
        atomic_json(destination / manifest_name, manifest)


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
    if has_extra_tools(release):
        entries = entries + extra_tool_inventory(json.loads((directory / 'tools/extra-manifest.json').read_text()))
    for entry in entries:
        verified(directory / 'tools' / entry['path'], entry['size'], entry['sha256'])


def installed_matches(current, release):
    installed = json.loads((current / 'release.json').read_text())
    # A supervisor that predates the extra tool bundle installed its version without
    # it, under a name without its digest, while recording the release it was offered.
    return all(release.get(field) == installed.get(field) for field in ('sha256', 'toolsSha256', 'extraToolsSha256')) \
        and current.name == version_name(release)


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
        if directory.is_symlink() or not directory.is_dir() or not re.fullmatch('[0-9a-f]{64}-[0-9a-f]{64}(-[0-9a-f]{64})?', directory.name):
            continue
        release = json.loads((directory / 'release.json').read_text())
        if directory.resolve() not in keep and release['runtimeId'] not in pinned:
            shutil.rmtree(directory)


def prepare(manager, release, code=None, token=None):
    release = validate_release(release)
    versions = INSTALL / 'versions'
    versions.mkdir(mode=0o755, parents=True, exist_ok=True)
    versions.chmod(0o755)
    destination = versions / version_name(release)
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
        if has_extra_tools(release):
            archive = download(manager, release, temporary, code=code, token=token, extra=True)
            unpack_extra_tools(archive, temporary)
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
        environment = {'PATH': str(directory / 'tools/bin') + ':/usr/bin:/bin', 'HOME': str(home), 'LANG': 'C.UTF-8',
                       'AGY_CLI_DISABLE_AUTO_UPDATE': 'true'}
        executables = [(directory / 'cairn', '--version')]
        entries = tool_inventory(json.loads((directory / 'tools/manifest.json').read_text()))
        if (directory / 'tools/extra-manifest.json').exists():
            entries = entries + extra_tool_inventory(json.loads((directory / 'tools/extra-manifest.json').read_text()))
        for entry in entries:
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


def require_storage_kernel():
    try:
        kernel = tuple(int(part) for part in os.uname().release.split('.')[:2])
    except ValueError as error:
        raise PrerequisiteError('kernel') from error
    if len(kernel) != 2 or kernel < (6, 11):
        raise PrerequisiteError('kernel')


def require_data_pool():
    pool = ROOT / 'state/native-data'
    try:
        metadata = pool.stat()
        controller = pool.parent.stat()
        result = subprocess.run(['findmnt', '--json', '--target', str(pool), '--output', 'TARGET,FSTYPE,OPTIONS'],
                                check=True, capture_output=True, text=True, timeout=10)
        mounts = json.loads(result.stdout).get('filesystems', [])
        # XFS and ext4 both list prjquota only while project quotas are
        # enforced; an accounting-only XFS mount shows pqnoenforce instead.
        supported = len(mounts) == 1 and mounts[0].get('target') == str(pool) and mounts[0].get('fstype') in ('ext4', 'xfs') \
            and 'prjquota' in mounts[0].get('options', '').split(',')
        if not supported or not stat.S_ISDIR(metadata.st_mode) or metadata.st_dev == controller.st_dev \
                or metadata.st_uid != 0 or metadata.st_mode & 0o022 or pool.resolve() != pool:
            raise PrerequisiteError('quota')
    except (OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
        raise PrerequisiteError('quota') from error


def legacy_storage_present():
    return any((ROOT / 'state/ublk-devices').glob('*.json')) or any((ROOT / 'state/native-block').glob('*/root/disk.blk')) \
        or any(any((ROOT / 'state' / base).glob(f'*/{source}/journal-v2.sqlite'))
               for base in ('disks', 'environments') for source in ('lazy', 'resize-source'))


def require_storage():
    require_storage_kernel()
    require_data_pool()
    if shutil.which('rsync') is None:
        raise PrerequisiteError('rsync')
    for binary in ('mkfs.ext4', 'e2fsck', 'resize2fs'):
        if shutil.which(binary) is None:
            raise PrerequisiteError('e2fsprogs')
    if not legacy_storage_present():
        return
    if not Path('/sys/module/ublk_drv').is_dir():
        try:
            subprocess.run(['modprobe', 'ublk_drv'], check=True, capture_output=True, timeout=10)
        except (OSError, subprocess.SubprocessError) as error:
            raise PrerequisiteError('ublk') from error
    try:
        control = Path('/dev/ublk-control').stat()
    except OSError as error:
        raise PrerequisiteError('ublk') from error
    if not stat.S_ISCHR(control.st_mode):
        raise PrerequisiteError('ublk')


def install(manager):
    manager = origin(manager)
    if (ROOT / 'data/node/identity.json').exists():
        raise ValueError('Native node is already enrolled.')
    require_systemd()
    try:
        controllers = set((CGROUP_ROOT / 'cgroup.controllers').read_text().split())
    except OSError as error:
        raise PrerequisiteError('cgroup') from error
    if not {'cpu', 'memory', 'pids', 'io'} <= controllers:
        raise PrerequisiteError('cgroup')
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
LimitNOFILE=65536
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
                if not installed_matches(current, release):
                    require_storage()
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
                diagnostic = error.diagnostic if isinstance(error, PrerequisiteError) else type(error).__name__
                print('Native update postponed: ' + diagnostic, file=sys.stderr)
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


# Offline data-pool migration. The node is stopped: no controller or workload
# can write the old pool, which stays untouched as the rollback source.
XFS_ACL_MIRRORS = ['--filter=-x trusted.SGI_ACL_FILE', '--filter=-x trusted.SGI_ACL_DEFAULT']
SAFE_PATH = re.compile(r'/[A-Za-z0-9._/-]+')
PHASES = ('planned', 'formatted', 'copied', 'switched', 'complete')
OFFLINE_LOCK_FDS = ()
CONTROLLER_RECEIPT_RESERVE = 64 * 1024 ** 2
MOUNT_RESTRICTIONS = ('nosuid', 'nodev', 'noexec', 'nosymfollow')
GET_INODE_FLAGS = 0x80086601  # FS_IOC_GETFLAGS on the supported Linux x86-64 host.
XFS_GEOMETRY = 0x80705864  # XFS_IOC_FSGEOMETRY_V1
XFS_GEOMETRY_V1 = struct.Struct('=64x16s32x')  # UUID after eight u32 and four u64 fields.
GET_FILE_EXTENTS = 0xc020660b  # FS_IOC_FIEMAP
EXTENT_HEADER = struct.Struct('=QQIIII')
FILE_EXTENT = struct.Struct('=QQQQQIIII')
EXTENT_PRIVATE_FLAGS = 0x0001 | 0x0800 | 0x1000  # LAST, UNWRITTEN, MERGED
EXT4_LAYOUT_FLAGS = 0x00001000 | 0x00040000 | 0x00080000 | 0x00200000 | 0x00400000 | 0x10000000
PROJECT_INHERIT_FLAG = 0x20000000


class MigrationError(ValueError):
    """An operator-facing refusal; messages never contain credentials."""


def migration_directory():
    return ROOT / 'native-pool-migration'


def pool_path():
    return ROOT / 'state/native-data'


def require_migration_running():
    if STOP:
        raise MigrationError('Pool migration interrupted. Rerun the command to resume the current phase.')


def offline_command(args, timeout, env=None, descriptors=()):
    require_migration_running()
    deadline = None if timeout is None else time.monotonic() + timeout
    # Each offline command inherits the lock's open file description. Even a
    # killed supervisor cannot release the fence while its writer remains live.
    with subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                          start_new_session=True, env=env, pass_fds=OFFLINE_LOCK_FDS + descriptors) as child:
        try:
            while True:
                require_migration_running()
                remaining = 0.2 if deadline is None else deadline - time.monotonic()
                if remaining <= 0:
                    raise subprocess.TimeoutExpired(args, timeout)
                try:
                    stdout, stderr = child.communicate(timeout=min(0.2, remaining))
                except subprocess.TimeoutExpired:
                    continue
                require_migration_running()
                return subprocess.CompletedProcess(args, child.returncode, stdout, stderr)
        except BaseException:
            if child.returncode is None:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                # Wait for the inherited pipes too, so a killed rsync helper
                # cannot outlive its leader while the pool lock is released.
                child.communicate()
            raise


def checked(*args, timeout=120, descriptors=()):
    try:
        result = offline_command(args, timeout, descriptors=descriptors)
        result.check_returncode()
        return result.stdout
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or '').strip().splitlines()
        raise MigrationError(f'{args[0]} failed: ' + (detail[-1][:300] if detail else f'exit status {error.returncode}')) from None
    except (OSError, subprocess.TimeoutExpired) as error:
        raise MigrationError(f'{args[0]} failed: {type(error).__name__}') from None


def parse_size(value):
    match = re.fullmatch(r'([0-9]+)([KMGT]?)i?B?', value.strip(), re.IGNORECASE)
    if not match:
        raise MigrationError('Use a size such as 500G or a byte count.')
    size = int(match.group(1)) * 1024 ** ' KMGT'.index(match.group(2).upper() or ' ')
    if size < 1024 ** 3:
        raise MigrationError('The new data pool must hold at least 1 GiB.')
    return size


def safe_path(value):
    path = Path(value)
    if not SAFE_PATH.fullmatch(str(path)) or '..' in path.parts or not path.is_absolute():
        raise MigrationError(f'Use an absolute path of letters, digits, ".", "_", "-" and "/": {value}')
    return path


def mountinfo_text(value):
    return re.sub(r'\\(040|011|012|134)', lambda match: chr(int(match[1], 8)), value)


def mount_of(target):
    # An absence is only authoritative after a complete, readable mount table
    # names no record for this path. Stacked mounts need operator review.
    try:
        lines = Path('/proc/self/mountinfo').read_text().splitlines()
    except OSError as error:
        raise MigrationError(f'Cannot inspect mounts before operating on {target}.') from error
    if not lines:
        raise MigrationError('The kernel mount table is empty.')
    found = None
    for line in lines:
        mount, separator, filesystem = line.partition(' - ')
        fields, fs = mount.split(), filesystem.split()
        if not separator or len(fields) < 6 or len(fs) != 3:
            raise MigrationError('The kernel mount table contains an invalid entry.')
        if Path(mountinfo_text(fields[4])) != target:
            continue
        if found is not None:
            raise MigrationError(f'{target} has stacked mounts. Resolve them before migrating.')
        if fields[3] != '/':
            raise MigrationError(f'{target} does not expose a whole pool filesystem.')
        effective = fields[5].split(',')
        super_options = fs[2].split(',')
        if 'ro' in super_options:
            effective = ['ro', *(option for option in effective if option not in ('rw', 'ro'))]
        effective.extend(option for option in super_options if option not in ('rw', 'ro'))
        found = {'target': str(target), 'source': mountinfo_text(fs[1]), 'fstype': fs[0],
                 'options': ','.join(dict.fromkeys(effective))}
    return found


def filesystem_uuid(device, descriptors=()):
    return checked('blkid', '-p', '-s', 'UUID', '-o', 'value', str(device), descriptors=descriptors).strip()


def mount_unit_name(path):
    return checked('systemd-escape', '--path', '--suffix=mount', str(path)).strip()


def active_units():
    output = checked('systemctl', 'list-units', '--plain', '--no-legend', '--no-pager',
                     '--state=active,activating,deactivating,reloading,refreshing', 'cairn-native-*')
    # Slices only group workloads and may stay active while empty.
    return [unit for unit in (line.split()[0] for line in output.splitlines() if line.split())
            if unit != NATIVE_LOOP_SERVICE and not FIXTURE_LOOP_SERVICE.fullmatch(unit) and not unit.endswith('.slice')]


def controller_running():
    try:
        descriptor = os.open(ROOT / 'state/controller.lock', os.O_RDONLY | os.O_CLOEXEC)
    except FileNotFoundError:
        return False
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return False
    except BlockingIOError:
        return True
    finally:
        os.close(descriptor)


def require_stopped(stop_services):
    units = active_units()
    if units and stop_services:
        ordered = [SERVICE] + [unit for unit in units if unit != SERVICE] if SERVICE in units else units
        for unit in ordered:
            checked('systemctl', 'stop', unit, timeout=600)
        deadline = time.monotonic() + 300
        while (units := active_units()) and time.monotonic() < deadline:
            time.sleep(1)
    if units:
        raise MigrationError('Stop the native node first (or pass --stop); still active: ' + ', '.join(sorted(units)))
    if controller_running():
        raise MigrationError('A native controller still holds state/controller.lock; stop it before migrating.')


def settle_pool_jobs():
    # PID 1 owns jobs once a systemctl request has been accepted. Reaping the
    # client after cancellation does not guarantee a stable mount observation.
    units = (mount_unit_name(pool_path()), LOOP_SERVICE)
    deadline = time.monotonic() + 300
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise MigrationError('The pool still has pending systemd jobs. Wait for them before retrying.')
        output = checked('systemctl', 'list-jobs', '--no-legend', '--no-pager', '--plain', '--full', *units,
                         timeout=min(10, remaining))
        if not any(fields and fields[0].isdigit() for fields in map(str.split, output.splitlines())):
            return
        time.sleep(min(0.2, remaining))


def require_installed_supervisor():
    installed = INSTALL / 'host.py'
    try:
        matches = installed.read_bytes() == Path(__file__).read_bytes()
    except FileNotFoundError:
        matches = False
    if not matches:
        raise MigrationError(f'Refresh the native supervisor from the same release before migrating. Verify {installed}.')


def offline_pool_operation(operation, arguments):
    global OFFLINE_LOCK_FDS
    if operation is migrate_pool:
        pool_unit_properties(mount_unit_name(pool_path()))
        if not arguments.dry_run:
            record = load_migration()
            target = migration_target(arguments, record)
            probe_migration_device(target, arguments.force, record)
            if not record or record['phase'] in ('planned', 'formatted'):
                preview = dict(record or {'phase': 'planned', 'target': target})
                if 'image' in target:
                    check_migration_image(preview, Path(target['image']), arguments.force)
                else:
                    check_controller_receipt_headroom(preview)
                original = mount_of(pool_path())
                if original and original['fstype'] == 'ext4':
                    check_migration_space(preview, arguments.force, migration_plan(ROOT / 'state'))
    if arguments.dry_run:
        return operation(arguments)
    require_stopped(arguments.stop)
    lock = ROOT / 'state/controller.lock'
    descriptor = os.open(lock, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise MigrationError('The native controller or another migration acquired state/controller.lock.') from None
        OFFLINE_LOCK_FDS = (descriptor,)
        try:
            settle_pool_jobs()
            return operation(arguments)
        finally:
            OFFLINE_LOCK_FDS = ()
    finally:
        os.close(descriptor)


def load_migration():
    try:
        return json.loads((migration_directory() / 'state.json').read_text())
    except FileNotFoundError:
        return None


def save_migration(record):
    require_migration_running()
    atomic_json(migration_directory() / 'state.json', record)
    sync_directory(migration_directory().parent)


def cairn_binary():
    override = os.environ.get('CAIRN_NATIVE_BINARY') if os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') == '1' else None
    return Path(override) if override else (INSTALL / 'current').resolve(strict=True) / 'cairn'


def pool_tool(*args):
    """Project IDs, limits and ownership records use the controller's own code."""
    result = offline_command([str(cairn_binary()), 'native-pool-migrate', *[str(arg) for arg in args]], None,
                             env={'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LANG': 'C.UTF-8', 'RUNNER_STATE_DIR': str(ROOT / 'state')})
    if result.returncode != 0:
        detail = (result.stderr.strip().splitlines() or ['no diagnostic'])[-1]
        if 'cairn --help' in result.stderr:
            detail = 'the installed native release predates pool migration; update the node first'
        raise MigrationError(f'native-pool-migrate {args[0]} failed: {detail}')
    return json.loads(result.stdout)


def pool_unit_properties(unit):
    properties = dict(line.split('=', 1) for line in checked(
        'systemctl', 'show', unit, '--property=FragmentPath,SourcePath,DropInPaths').splitlines() if '=' in line)
    if properties.get('SourcePath') == '/etc/fstab':
        raise MigrationError(f'{unit} is mounted from /etc/fstab; replace that entry with a persistent mount unit before migrating.')
    if properties.get('DropInPaths'):
        raise MigrationError(f'{unit} uses systemd drop-ins. Fold its effective mount configuration into the persistent unit before migrating.')
    fragment = properties.get('FragmentPath')
    if (UNIT_DIRECTORY / unit).is_symlink() or (fragment and Path(fragment) != UNIT_DIRECTORY / unit):
        raise MigrationError(f'{unit} must be a regular local unit in {UNIT_DIRECTORY}, or the ext4 pool must be mounted manually.')
    return properties


def pool_boot_link(unit):
    return UNIT_DIRECTORY / 'local-fs.target.wants' / unit


def boot_link_target(unit):
    link = pool_boot_link(unit)
    try:
        target = os.readlink(link)
    except FileNotFoundError:
        return None
    except OSError as error:
        raise MigrationError(f'{link} is not a systemd dependency link.') from error
    if (link.parent / target).resolve() != UNIT_DIRECTORY / unit:
        raise MigrationError(f'{link} points to another unit.')
    return target


def old_pool(pool):
    mounted = mount_of(pool)
    if not mounted or mounted.get('fstype') != 'ext4' or 'prjquota' not in mounted.get('options', '').split(','):
        raise MigrationError(f'{pool} must be the mounted ext4 project-quota pool to migrate.')
    unit = mount_unit_name(pool)
    properties = pool_unit_properties(unit)
    fragment = properties.get('FragmentPath') or ''
    source = mounted.get('source', '')
    effective_options = mounted['options'].split(',')
    if 'ro' in effective_options:
        raise MigrationError('The ext4 source is read-only. Restore a verified writable source before migrating.')
    what, options = source, list(effective_options)
    fragment_bytes = None
    if fragment and Path(fragment).is_file():
        fragment_bytes = Path(fragment).read_bytes()
        for line in fragment_bytes.decode().splitlines():
            key, _, value = line.partition('=')
            if key.strip() == 'What':
                what = value.strip()
            elif key.strip() == 'Options':
                options = value.strip().split(',')
    elif source.startswith('/dev/loop'):
        geometry = checked('losetup', '--noheadings', '--output', 'OFFSET,SIZELIMIT,DIO,LOG-SEC', source).split()
        if geometry != ['0', '0', '0', '512']:
            raise MigrationError('The ext4 pool uses nondefault manual loop settings. Preserve them in a persistent mount unit before migrating.')
        what = checked('losetup', '--noheadings', '--output', 'BACK-FILE', source).strip()
        options.insert(0, 'loop')
    return {'unit': unit, 'fragment': fragment if fragment_bytes is not None else None,
            'unitSha256': hashlib.sha256(fragment_bytes).hexdigest() if fragment_bytes is not None else None,
            'bootLink': boot_link_target(unit),
            'restrictions': [option for option in MOUNT_RESTRICTIONS if option in effective_options],
            'what': what, 'type': 'ext4', 'options': ','.join(options),
            'uuid': filesystem_uuid(source)}


def has_partitions(sysfs):
    return sysfs.is_dir() and any((child / 'partition').is_file() for child in sysfs.iterdir())


def check_device(device, force, uuid=None, descriptor=None):
    try:
        metadata = device.stat() if descriptor is None else os.fstat(descriptor)
    except OSError as error:
        raise MigrationError(f'{device} is not available.') from error
    if not stat.S_ISBLK(metadata.st_mode):
        raise MigrationError(f'{device} is not a block device.')
    device_id = f'{os.major(metadata.st_rdev)}:{os.minor(metadata.st_rdev)}'
    sysfs = Path('/sys/dev/block') / device_id
    if has_partitions(sysfs):
        raise MigrationError(f'{device} contains partitions; select the unused partition or logical volume itself.')
    mounts = Path('/proc/self/mountinfo').read_text().splitlines()
    mounted = any(line.split()[2] == device_id for line in mounts)
    if mounted or subprocess.run(['findmnt', '--source', str(device)], capture_output=True, timeout=10).returncode == 0:
        raise MigrationError(f'{device} is mounted; choose an unused partition or logical volume.')
    holders = sysfs / 'holders'
    if holders.is_dir() and any(holders.iterdir()):
        raise MigrationError(f'{device} is in use by another device (LVM, RAID or dm).')
    # blkid exits 2 when it finds no signature at all.
    path = str(device) if descriptor is None else f'/proc/self/fd/{descriptor}'
    descriptors = () if descriptor is None else (descriptor,)
    probe = subprocess.run(['blkid', '-p', '-o', 'export', path], capture_output=True, text=True, timeout=30, pass_fds=descriptors)
    if probe.returncode not in (0, 2):
        raise MigrationError(f'blkid could not probe {device}.')
    fields = dict(line.split('=', 1) for line in probe.stdout.splitlines() if '=' in line)
    ours = uuid is not None and fields.get('TYPE') == 'xfs' and fields.get('UUID') == uuid
    if fields and not ours and not force:
        raise MigrationError(f'{device} already contains {fields.get("TYPE", "data")}; pass --force to overwrite it.')
    return ours


def check_migration_block_device(device, force, record, descriptor=None):
    retry = record is not None and 'formatDevice' in record
    expected = record['uuid'] if record else None
    ours = check_device(device, force or retry, uuid=expected, descriptor=descriptor)
    # Device numbers may be reassigned during a reboot or a volume change.
    # Once formatting has been attempted, only its UUID proves this target.
    if retry and not ours:
        raise MigrationError(f'{device} no longer proves the planned XFS UUID. '
                             'Run rollback-pool, verify the intended volume, then start a new migration.')


def attached_loop(image):
    devices = checked('losetup', '--noheadings', '--output', 'NAME', '--associated', str(image)).split()
    if len(devices) > 1:
        raise MigrationError(f'{image} is attached to several loop devices: ' + ', '.join(devices))
    return Path(devices[0]) if devices else None


def loop_unit(image, mount_unit, filesystem):
    image_text = str(image)
    attached = f'set -- $$(losetup --noheadings --output NAME --associated {image_text}); test "$$#" -le 1; '
    same_filesystem = f'test "$$(blkid -p -s UUID -o value "$$1")" = {filesystem}'
    ready = (
        'ready() { '
        f'{same_filesystem} && '
        'test "$$(losetup --noheadings --output DIO "$$1" | tr -d " ")" = 1 && '
        'printf "0\\n" >/sys/class/block/$${1##*/}/queue/discard_max_bytes; }; '
    )
    start = (
        ready + attached +
        f'if test "$$#" = 0; then device=$$(losetup --direct-io=on --find --show {image_text}); '
        'if ready "$$device"; then exit 0; fi; losetup --detach "$$device"; exit 1; fi; ready "$$1"'
    )
    stop = attached + 'if test "$$#" = 0; then exit 0; fi; ' + same_filesystem + '; losetup --detach "$$1"'
    return f'''[Unit]
Description=Cairn native data pool loop device (direct I/O)
DefaultDependencies=no
RequiresMountsFor={image.parent}
Before={mount_unit} umount.target
Conflicts=umount.target
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -ec '{start}'
ExecStop=/bin/sh -ec '{stop}'
[Install]
WantedBy=local-fs.target
'''


NEW_POOL_UNIT = 'Description=Cairn native data pool (XFS, reflink, project quotas)'


def pool_unit(pool, uuid, image, restrictions=()):
    loop = f'Requires={LOOP_SERVICE}\nAfter={LOOP_SERVICE}\n' if image else ''
    options = ','.join(['prjquota', *(option for option in MOUNT_RESTRICTIONS if option in restrictions)])
    return f'''[Unit]
{NEW_POOL_UNIT}
{loop}Before={SERVICE}
[Mount]
What=/dev/disk/by-uuid/{uuid}
Where={pool}
Type=xfs
Options={options}
[Install]
WantedBy=local-fs.target
'''


def plan_steps(record, old, plan, pool):
    target = record['target']
    image = target.get('image')
    device = target.get('device') or f'a new loop device for {image}'
    steps = []
    if image:
        steps.append(f'Fully allocate image {image} with {target["size"]} bytes and attach it with losetup --direct-io=on.')
    steps.append(f'Format {device} with mkfs.xfs -m crc=1,reflink=1 -i projid32bit=1 and mount it with prjquota at {migration_directory() / "target"}.')
    for environment in plan['environments']:
        if environment['project']:
            quota = (f'project {environment["project"]}, {environment["usedBytes"]} bytes and {environment["inodes"]} inodes used, '
                     f'limit {environment["blockHardBytes"]} bytes / {environment["inodeHard"]} inodes')
        else:
            quota = 'no project quota assigned yet'
        steps.append(f'Copy environment {environment["run"]} ({quota}) with rsync -aHAXc --numeric-ids --sparse, verify it with a '
                     'checksum pass, then apply the same project ID and limits and stage its rewritten ownership records.')
    if plan['skipped']:
        steps.append('Skip rebuildable or filesystem-owned entries: ' + ', '.join(plan['skipped']) + '.')
    unit = UNIT_DIRECTORY / old['unit']
    steps.append(f'Unmount the ext4 pool ({old["what"]}) and keep it unchanged as the rollback source.')
    if image:
        steps.append(f'Write {UNIT_DIRECTORY / LOOP_SERVICE} to attach {image} with direct I/O at boot.')
    steps.append(f'Write {unit} (backing up the current unit) to mount the XFS pool by UUID at {pool}, before {SERVICE}.')
    steps.append('Switch the controller ownership records to the new pool and run the controller pool check.')
    return steps


def back_up_unit(old):
    if old['fragment'] is None:
        return None
    backup = migration_directory() / 'units' / old['unit']
    present = backup.is_file()
    source = backup if present else Path(old['fragment'])
    try:
        content = source.read_bytes()
    except FileNotFoundError:
        raise MigrationError(f'Cannot prove the original mount unit {old["unit"]}; keep both pool volumes.') from None
    if hashlib.sha256(content).hexdigest() != old.get('unitSha256'):
        raise MigrationError(f'Original mount unit {old["unit"]} changed; inspect it before continuing.')
    if not present:
        atomic_bytes(backup, content)
        sync_directory(backup.parent.parent)
    return content


def remount_old_pool(old, pool):
    if mount_of(pool) is not None:
        return
    if old['fragment']:
        checked('systemctl', 'start', old['unit'], timeout=300)
    else:
        checked('mount', '-t', old['type'], '-o', old['options'], old['what'], str(pool), timeout=300)


def verify_old_pool(old, mounted):
    if not mounted or mounted.get('fstype') != 'ext4' or filesystem_uuid(mounted['source']) != old['uuid']:
        raise MigrationError('The mounted ext4 filesystem does not match the original pool.')


def verify_restored_pool(old, mounted):
    verify_old_pool(old, mounted)
    options = set(mounted.get('options', '').split(','))
    if 'rw' not in options or 'ro' in options or 'prjquota' not in options:
        raise MigrationError('Restore the original pool writable with project quotas before completing rollback.')
    if not set(old.get('restrictions', ())).issubset(options):
        raise MigrationError('Restore the original pool mount restrictions before completing rollback.')


def mount_new_pool(record, old, pool):
    image = record['target'].get('image')
    unit = UNIT_DIRECTORY / old['unit']
    back_up_unit(old)
    if image:
        loop_path = UNIT_DIRECTORY / LOOP_SERVICE
        expected = loop_unit(Path(image), old['unit'], record['uuid']).encode()
        if loop_path.exists() and loop_path.read_bytes() != expected:
            raise MigrationError(f'{LOOP_SERVICE} belongs to another pool.')
        atomic_bytes(loop_path, expected, 0o644)
    atomic_bytes(unit, pool_unit(pool, record['uuid'], image, old.get('restrictions', ())).encode(), 0o644)
    link = pool_boot_link(old['unit'])
    if boot_link_target(old['unit']) is None:
        link.parent.mkdir(exist_ok=True)
        link.symlink_to(unit)
    sync_directory(link.parent)
    sync_directory(UNIT_DIRECTORY)
    checked('systemctl', 'daemon-reload')
    units = ([LOOP_SERVICE] if image else []) + [old['unit']]
    if image:
        checked('systemctl', 'enable', LOOP_SERVICE)
    wants = UNIT_DIRECTORY / 'local-fs.target.wants'
    if wants.is_dir():
        sync_directory(wants)
    sync_directory(UNIT_DIRECTORY)
    for name in units:
        checked('systemctl', 'start', name, timeout=300)
    mounted = mount_of(pool)
    if not mounted or mounted.get('fstype') != 'xfs' or filesystem_uuid(mounted['source']) != record['uuid']:
        raise MigrationError(f'{pool} did not mount the new XFS pool; inspect systemctl status {old["unit"]}.')


def unmount_old_pool(old, pool):
    if mount_of(pool) is None:
        return
    if old['fragment']:
        checked('systemctl', 'stop', old['unit'], timeout=300)
    else:
        checked('umount', str(pool), timeout=300)
    if mount_of(pool) is not None:
        raise MigrationError(f'{pool} is still mounted; nothing was switched.')


def verify_portable_inode_flags(source, project):
    # rsync's archive/xattr flags cannot attest chattr semantics. Ext4's
    # internal layout can change, and XFS recreates quota inheritance.
    allowed = EXT4_LAYOUT_FLAGS | (PROJECT_INHERIT_FLAG if project else 0)

    def check(descriptor, path):
        flags = bytearray(8)
        fcntl.ioctl(descriptor, GET_INODE_FLAGS, flags)
        unsupported = struct.unpack_from('=I', flags)[0] & ~allowed
        if unsupported:
            raise MigrationError(f'{path} has inode flags 0x{unsupported:x} that this migration cannot preserve. The ext4 source was retained.')

    def unreadable(error):
        raise MigrationError(f'Cannot complete inode flag inspection for {source}.') from error

    for directory, _, files, descriptor in os.fwalk(source, onerror=unreadable, follow_symlinks=False):
        check(descriptor, directory)
        for name in files:
            metadata = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
            if not stat.S_ISREG(metadata.st_mode):
                continue
            file = os.open(name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=descriptor)
            try:
                check(file, Path(directory) / name)
            finally:
                os.close(file)


def copy_environment(run, source, target, project=0):
    verify_portable_inode_flags(source / run, project)
    source_path, target_path = f'{source}/{run}/', f'{target}/{run}/'
    # A resumed target can have the same size and timestamps as the original
    # despite an interrupted write. Content checks make this retry converge.
    checked('rsync', '-aHAXc', '--numeric-ids', '--sparse', '--delete', *XFS_ACL_MIRRORS, '--', source_path, target_path, timeout=None)
    differences = checked('rsync', '-aHAXnc', '--numeric-ids', '--delete', '--itemize-changes', *XFS_ACL_MIRRORS, '--',
                          source_path, target_path, timeout=None)
    if differences.strip():
        raise MigrationError(f'Verification of environment {run} found differences; rerun the migration:\n' + differences[:2000])


def flush_migration_copy(descriptor):
    # The descriptor predates the first write, so another sync caller cannot
    # consume a writeback error before this migration sees it.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.syncfs(descriptor) != 0:
        raise MigrationError(f'The XFS copy could not be written to disk: {os.strerror(ctypes.get_errno())}. The ext4 source was retained.')
    try:
        os.fsync(descriptor)
    except OSError as error:
        raise MigrationError('The XFS copy directory could not be synchronized. The ext4 source was retained.') from error
    if os.fstatvfs(descriptor).f_flag & os.ST_RDONLY:
        raise MigrationError('The XFS copy became read-only. The ext4 source was retained.')


def filesystem_path(path):
    # A pathname can have stacked mounts. The open descriptor identifies
    # the mount whose inode the caller can actually use.
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        fdinfo = Path(f'/proc/self/fdinfo/{descriptor}').read_text()
        mount_id = next((line.split()[1] for line in fdinfo.splitlines() if line.startswith('mnt_id:')), None)
        for line in Path('/proc/self/mountinfo').read_text().splitlines():
            fields = line.split()
            if fields[0] == mount_id:
                root, target = (Path(mountinfo_text(value)) for value in fields[3:5])
                return root / path.relative_to(target)
    finally:
        os.close(descriptor)
    raise MigrationError(f'Cannot identify the underlying filesystem of {path}.')


def verify_image_location(image, state, pool):
    parent = image.parent
    if image.is_symlink() or (image.exists() and not image.is_file()) or parent.resolve(strict=True) != parent:
        raise MigrationError('Choose a real directory and regular file for the new pool image.')
    if image.is_relative_to(state) or image.is_relative_to(pool):
        raise MigrationError('Keep the new pool image outside the data pool and controller state directories.')
    target_device = parent.stat().st_dev
    control_device = state.stat().st_dev
    if (pool.exists() and target_device == pool.stat().st_dev != control_device) or (
        target_device == control_device and filesystem_path(parent).is_relative_to(filesystem_path(state))
    ):
        raise MigrationError('Keep the new image outside aliases of the old pool or controller state.')


def staged_image_path(image, receipt):
    name = receipt['name']
    if Path(name).name != name or not name.startswith('.cairn-native-xfs-'):
        raise MigrationError('Invalid staged XFS image name.')
    return image.with_name(name)


def image_identity(descriptor):
    metadata = os.fstat(descriptor)
    if not stat.S_ISREG(metadata.st_mode):
        raise MigrationError('A native pool backing file must be a regular file.')
    # XFS f_fsid encodes the current device number. The UUID remains
    # stable when the backing filesystem is reattached after a reboot.
    geometry = bytearray(XFS_GEOMETRY_V1.size)
    try:
        fcntl.ioctl(descriptor, XFS_GEOMETRY, geometry)
    except OSError as error:
        if error.errno not in (errno.ENOTTY, errno.EOPNOTSUPP):
            raise
        filesystem = os.fstatvfs(descriptor).f_fsid
    else:
        (identifier,) = XFS_GEOMETRY_V1.unpack(geometry)
        if identifier == bytes(16):
            raise MigrationError('The XFS image host has no stable filesystem UUID.')
        filesystem = str(uuid.UUID(bytes=identifier))
    return {'filesystem': filesystem, 'inode': metadata.st_ino}


def check_image_inode(descriptor, receipt):
    if image_identity(descriptor) != {key: receipt[key] for key in ('filesystem', 'inode')}:
        raise MigrationError('New pool image does not match the inode created for this migration.')


def image_allocation(descriptor, size):
    """Count private allocation inside the future loop device's byte range."""
    cursor = 0
    allocated = 0
    limit = 64
    while cursor < size:
        query = bytearray(EXTENT_HEADER.size + limit * FILE_EXTENT.size)
        EXTENT_HEADER.pack_into(query, 0, cursor, size - cursor, 1, 0, limit, 0)
        try:
            fcntl.ioctl(descriptor, GET_FILE_EXTENTS, query)
        except OSError as error:
            raise MigrationError('Cannot verify private allocation for the pool image. Use a supported image filesystem or a block device.') from error
        mapped = EXTENT_HEADER.unpack_from(query)[3]
        if mapped > limit:
            raise MigrationError('Invalid file extent allocation report.')
        if mapped == 0:
            break
        for index in range(mapped):
            logical, _, length, _, _, flags, *_ = FILE_EXTENT.unpack_from(query, EXTENT_HEADER.size + index * FILE_EXTENT.size)
            if logical >= size:
                return allocated
            end = min(size, logical + length)
            start = max(cursor, logical)
            if end <= start:
                raise MigrationError('Invalid file extent allocation range.')
            if flags & ~EXTENT_PRIVATE_FLAGS:
                raise MigrationError('The pool image contains shared or unverified file extents.')
            allocated += end - start
            cursor = end
        if flags & 1:
            break
    return allocated


def check_image_headroom(record, image, allocated=0):
    reserve = record.get('receiptReserveBytes', CONTROLLER_RECEIPT_RESERVE)
    remaining = max(0, record['target']['size'] - allocated)
    image_device = image.parent.stat().st_dev
    receipt_paths = (ROOT, ROOT / 'state')
    image_reserve = reserve if any(path.stat().st_dev == image_device for path in receipt_paths) else 0
    def free(path):
        space = os.statvfs(path)
        return space.f_bavail * space.f_frsize
    if free(image.parent) < remaining + image_reserve:
        raise MigrationError(f'The image needs at least {remaining + image_reserve} free bytes for full backing allocation and controller recovery receipts.')
    for path in receipt_paths:
        required = reserve + (remaining if path.stat().st_dev == image_device else 0)
        if free(path) < required:
            raise MigrationError(f'Free at least {required} bytes on {path} for allocation and controller recovery receipts.')


def check_controller_receipt_headroom(record):
    reserve = record.get('receiptReserveBytes', CONTROLLER_RECEIPT_RESERVE)
    for path in (ROOT, ROOT / 'state'):
        space = os.statvfs(path)
        if space.f_bavail * space.f_frsize < reserve:
            raise MigrationError(f'Free at least {reserve} bytes on {path} for controller recovery receipts.')


def check_migration_image(record, image, force, plan=None):
    verify_image_location(image, ROOT / 'state', pool_path())
    size = record['target']['size']
    if plan is not None:
        needed = plan['usedBytes'] + plan['usedBytes'] // 10 + 1024 ** 3
        if size < needed:
            raise MigrationError(f'The new pool needs at least {needed} bytes as --size.')
    created = record.get('createdImage')
    published = image.exists()
    if published and not created and record['phase'] == 'planned' and not force:
        raise MigrationError(f'{image} already exists; pass --force to reuse and overwrite it.')
    actual = image if published or not created else staged_image_path(image, created)
    identity = None
    if actual.exists():
        descriptor = os.open(actual, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
        try:
            metadata = os.fstat(descriptor)
            identity = image_identity(descriptor)
            if created:
                check_image_inode(descriptor, created)
            if published and metadata.st_size != size:
                raise MigrationError('An existing target image must be fully allocated at the requested size.')
            if not published and metadata.st_size > size:
                raise MigrationError('The recorded staging image exceeds the requested size.')
            allocated = image_allocation(descriptor, size)
        finally:
            os.close(descriptor)
        if published and allocated < size:
            raise MigrationError('An existing target image must be fully allocated at the requested size.')
    elif created or record['phase'] != 'planned':
        raise MigrationError(f'The image for this migration is missing: {actual}.')
    else:
        allocated = 0
    check_image_headroom(record, image, allocated)
    return identity


def prepare_image(record, image, force):
    identity = check_migration_image(record, image, force)
    created = record.get('createdImage')
    if not created:
        if image.exists():
            return identity
        # Commit ownership while the staging file is empty, so an interrupted
        # full allocation can always be resumed or released by rollback.
        with tempfile.NamedTemporaryFile(prefix='.cairn-native-xfs-', dir=image.parent, delete=False) as output:
            os.fsync(output.fileno())
            created = {'name': Path(output.name).name, **image_identity(output.fileno())}
        sync_directory(image.parent)
        record['createdImage'] = created
        save_migration(record)

    staged = staged_image_path(image, created)
    published = image.exists()
    size = record['target']['size']
    if not published:
        descriptor = os.open(staged, os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            measured = os.fstat(descriptor)
            check_image_inode(descriptor, created)
            allocated = image_allocation(descriptor, size)
            check_image_headroom(record, image, allocated)
            if measured.st_size != size or allocated < size:
                if measured.st_size > size:
                    raise MigrationError('The staging image exceeds the requested size.')
                require_migration_running()
                os.posix_fallocate(descriptor, 0, size)
                os.fsync(descriptor)
                if image_allocation(descriptor, size) != size:
                    raise MigrationError('The new pool image is not fully privately allocated.')
        finally:
            os.close(descriptor)
    require_migration_running()
    if not published:
        os.link(staged, image, follow_symlinks=False)
        sync_directory(image.parent)
    if staged.exists():
        descriptor = os.open(staged, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
        try:
            check_image_inode(descriptor, created)
        finally:
            os.close(descriptor)
        staged.unlink()
        sync_directory(image.parent)
    return created


def owned_loop(record, image):
    receipt = record.get('imageLoop')
    if receipt is None:
        return None
    try:
        descriptor = os.open(image, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    except FileNotFoundError:
        if record['phase'] in ('planned', 'formatted'):
            return None
        raise
    try:
        check_image_inode(descriptor, receipt)
    finally:
        os.close(descriptor)
    device = attached_loop(image)
    if device is None:
        return None
    if record['phase'] == 'planned':
        if str(device) != receipt['loop']:
            raise MigrationError('The image is attached to a different loop than the planned migration.')
    elif filesystem_uuid(device) != record['uuid']:
        raise MigrationError('The attached loop does not contain the migration filesystem.')
    return device


def disable_loop_discard(device):
    if not re.fullmatch(r'/dev/loop[0-9]+', str(device)):
        raise MigrationError(f'Cannot identify the queue of the migration loop device {device}.')
    queue = Path('/sys/class/block') / device.name / 'queue/discard_max_bytes'
    # Discard punches holes in a loop backing file, including discards
    # initiated by the host's periodic fstrim service.
    if queue.read_text().strip() != '0':
        queue.write_text('0\n')
    if queue.read_text().strip() != '0':
        raise MigrationError(f'Cannot preserve the allocated backing space of {device}.')


def prepare_loop(record, force, handles):
    image = Path(record['target']['image'])
    identity = prepare_image(record, image, force)
    if identity is None:
        raise MigrationError('The pool image changed while preparing it.')
    image_fd = os.open(image, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    handles.callback(os.close, image_fd)
    check_image_inode(image_fd, identity)
    device = owned_loop(record, image)
    if device is None:
        if attached_loop(image) is not None:
            raise MigrationError('The new image is already attached outside the migration.')
        device = Path(checked('losetup', '--find').strip())
        record['imageLoop'] = {**image_identity(image_fd), 'loop': str(device)}
        save_migration(record)
        checked('losetup', '--direct-io=on', str(device), f'/proc/self/fd/{image_fd}', descriptors=(image_fd,))
    # Keep a kernel reference during attestation and formatting. A detached
    # loop cannot be assigned another backing file while this reference lives.
    device_fd = os.open(device, os.O_RDONLY | os.O_CLOEXEC | os.O_NONBLOCK)
    handles.callback(os.close, device_fd)
    backing = os.fstat(image_fd)
    expected_device = f'{os.major(backing.st_dev)}:{os.minor(backing.st_dev)}'
    actual = checked('losetup', '--noheadings', '--output', 'BACK-INO,BACK-MAJ:MIN,DIO', str(device)).split()
    if actual[:2] != [str(backing.st_ino), expected_device]:
        raise MigrationError('The loop device does not bind the verified pool inode.')
    if actual[2:] != ['1']:
        raise MigrationError(f'{device} does not support direct I/O for this image. Choose a partition, logical volume or another image filesystem.')
    disable_loop_discard(device)
    return device, device_fd


def verify_target_device_path(device, device_id):
    metadata = device.stat()
    if not stat.S_ISBLK(metadata.st_mode) or metadata.st_rdev != device_id:
        raise MigrationError(f'{device} no longer identifies the verified formatting target.')


def prepare_target(record, force):
    with ExitStack() as handles:
        return prepare_target_with_handles(record, force, handles)


def prepare_target_with_handles(record, force, handles):
    target = record['target']
    work = migration_directory()
    if 'image' in target:
        device, descriptor = prepare_loop(record, force, handles)
    else:
        device = Path(target['device'])
        descriptor = os.open(device, os.O_RDONLY | os.O_CLOEXEC | os.O_NONBLOCK)
        handles.callback(os.close, descriptor)
    device_id = os.fstat(descriptor).st_rdev
    pinned = f'/proc/self/fd/{descriptor}'
    inherited = (descriptor,)
    expected = record['uuid']
    if record['phase'] == 'planned':
        if 'device' in target:
            check_migration_block_device(device, force, record, descriptor)
            if 'formatDevice' not in record:
                record['formatDevice'] = device_id
                save_migration(record)
        else:
            check_device(device, True, uuid=expected, descriptor=descriptor)
        verify_target_device_path(device, device_id)
        # A partial mkfs can already have written the recorded UUID. The
        # planned phase contains no copied environments and can format again.
        discard = ['-K'] if 'image' in target else []
        checked('mkfs.xfs', '-f', '-q', *discard, '-L', 'cairn-native', '-m', f'crc=1,reflink=1,uuid={expected}', '-i', 'projid32bit=1',
                pinned, timeout=1800, descriptors=inherited)
        if filesystem_uuid(pinned, inherited) != expected:
            raise MigrationError(f'{device} did not retain the migration XFS UUID.')
        record['phase'] = 'formatted'
        save_migration(record)
    elif filesystem_uuid(pinned, inherited) != expected:
        raise MigrationError(f'{device} no longer holds the XFS filesystem this migration formatted.')
    mountpoint = work / 'target'
    mountpoint.mkdir(mode=0o700, exist_ok=True)
    mounted = mount_of(mountpoint)
    if mounted is None:
        verify_target_device_path(device, device_id)
        checked('mount', '-t', 'xfs', '-o', 'prjquota', str(device), str(mountpoint), timeout=300)
        mounted = mount_of(mountpoint)
    if not matches_new_pool(record, mounted):
        raise MigrationError(f'{mountpoint} does not hold the migration filesystem.')
    # The new pool root becomes the pool path: root-owned, as the old one.
    os.chown(mountpoint, 0, 0)
    os.chmod(mountpoint, pool_path().stat().st_mode & 0o755)
    return mountpoint


def device_path(value):
    path = Path(value)
    if not path.is_absolute() or '\n' in value:
        raise MigrationError(f'Use the absolute path of a block device: {value}')
    return path


def matches_new_pool(record, mounted):
    return bool(mounted and mounted.get('fstype') == 'xfs' and record.get('uuid')
                and filesystem_uuid(mounted['source']) == record['uuid'])


def new_pool_mounted(record, pool):
    return matches_new_pool(record, mount_of(pool))


def migration_target(arguments, record):
    if arguments.device:
        target = {'device': str(device_path(arguments.device))}
    elif arguments.image:
        image = safe_path(arguments.image)
        if image.is_relative_to(ROOT / 'state'):
            raise MigrationError('Keep the new pool image outside the data pool and controller state directories.')
        target = {'image': str(image), 'size': parse_size(arguments.size or '')}
    elif record:
        target = record['target']
    else:
        raise MigrationError('Choose a target with --device <block device> or --image <path> --size <size>.')
    if record and record['target'] != target:
        raise MigrationError('Another pool migration is in progress (' + json.dumps(record['target']) + '); finish it or run rollback-pool.')
    return target


def probe_migration_device(target, force, record):
    if 'device' in target and (not record or record['phase'] == 'planned'):
        check_migration_block_device(Path(target['device']), force, record)


def migration_plan(state):
    plan = pool_tool('plan', state)
    if plan['blockers']:
        raise MigrationError('Resolve these before migrating:\n- ' + '\n- '.join(plan['blockers']))
    return plan


def check_migration_space(record, force, plan):
    record['receiptReserveBytes'] = max(record.get('receiptReserveBytes', 0),
                                        CONTROLLER_RECEIPT_RESERVE + len(plan['environments']) * 64 * 1024)
    target = record['target']
    if 'image' in target:
        check_migration_image(record, Path(target['image']), force, plan)
    else:
        check_controller_receipt_headroom(record)


def migrate_pool(arguments):
    pool = pool_path()
    state = ROOT / 'state'
    record = load_migration()
    target = migration_target(arguments, record)
    if arguments.dry_run:
        probe_migration_device(target, arguments.force, record)
    if record and record['phase'] in ('copied', 'switched', 'complete'):
        return finish_migration(record, arguments, pool, state)
    safe_path(str(pool))
    if record and not arguments.dry_run:
        # Before the copy has settled, reattach the original after a reboot.
        remount_old_pool(record['old'], pool)
    old = old_pool(pool)
    if record and (record['old']['unit'], record['old']['uuid']) != (old['unit'], old['uuid']):
        raise MigrationError('The pool mount changed since the migration started; run rollback-pool.')
    old = record['old'] if record else old
    plan = migration_plan(state)
    record = record or {'version': 1, 'target': target, 'phase': 'planned', 'old': old, 'uuid': str(uuid.uuid4())}
    check_migration_space(record, arguments.force, plan)
    if arguments.dry_run:
        units = active_units()
        print('Native data pool migration plan (dry run, nothing changed):')
        for number, step in enumerate(plan_steps(record, old, plan, pool), 1):
            print(f'{number}. {step}')
        if units:
            print('The node is running; stop it or pass --stop: ' + ', '.join(units))
        return
    probe_migration_device(target, arguments.force, record)
    migration_directory().mkdir(mode=0o700, exist_ok=True)
    save_migration(record)
    back_up_unit(old)
    mountpoint = prepare_target(record, arguments.force)
    runs = [environment['run'] for environment in plan['environments']]
    rewrite = record.get('copyInProgress', False)
    record['copyInProgress'] = True
    save_migration(record)
    descriptor = os.open(mountpoint, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        for entry in mountpoint.iterdir():
            if rewrite or entry.name not in runs:
                # A failed writeback can leave bytes visible in page cache.
                # Recreate the interrupted copy before trusting checksums.
                shutil.rmtree(entry)
        for environment in plan['environments']:
            run = environment['run']
            pool_tool('prepare', state, migration_directory(), mountpoint, run)
            copy_environment(run, pool, mountpoint, environment['project'])
            pool_tool('commit', state, migration_directory(), mountpoint, run)
            print(f'Copied and verified environment {run}.')
        flush_migration_copy(descriptor)
    finally:
        os.close(descriptor)
    pool_tool('seal', state, migration_directory(), mountpoint)
    checked('umount', str(mountpoint), timeout=300)
    record.pop('copyInProgress')
    record['phase'] = 'copied'
    save_migration(record)
    finish_migration(record, arguments, pool, state)


def finish_migration(record, arguments, pool, state):
    if arguments.dry_run:
        print(f'The pool migration reached phase {record["phase"]}; a rerun only completes it.')
        return
    pool_unit_properties(record['old']['unit'])
    if not new_pool_mounted(record, pool):
        mounted = mount_of(pool)
        if mounted is not None:
            if record['phase'] != 'copied':
                raise MigrationError(f'{pool} holds another filesystem than the migrated pool.')
            verify_old_pool(record['old'], mounted)
            unmount_old_pool(record['old'], pool)
        mount_new_pool(record, record['old'], pool)
    if record['phase'] == 'copied':
        record['phase'] = 'switched'
        save_migration(record)
    if not record.get('activated'):
        pool_tool('switch', state, migration_directory())
    result = pool_tool('check', state)
    record['phase'] = 'complete'
    save_migration(record)
    print(f'Native data pool migrated to XFS (reflink: {str(result["reflink"]).lower()}), {result["environments"]} environments checked.')
    if not record.get('activated') and not record.get('activationRequested'):
        print(f'Authorize activation with: python3 {INSTALL}/host.py activate-pool')
        print(f'The ext4 pool {record["old"]["what"]} was preserved. To roll back before activation, run '
              f'"python3 {INSTALL}/host.py rollback-pool" while the node is stopped.')
    else:
        print(f'Start the node with: systemctl start {SERVICE}')


def activate_pool(arguments):
    record = load_migration()
    if not record or record.get('phase') != 'complete':
        raise MigrationError('Finish the XFS pool migration before activating it.')
    mounted = mount_of(pool_path())
    if not matches_new_pool(record, mounted):
        raise MigrationError('The mounted pool does not match the completed XFS migration.')
    if 'rw' not in mounted['options'].split(','):
        raise MigrationError('The verified XFS pool is read-only. Remount it writable before authorizing activation.')
    if arguments.dry_run:
        print('Would authorize the node to open the verified XFS pool on its next start.')
        return
    pool_tool('check', ROOT / 'state')
    record['activationRequested'] = True
    save_migration(record)
    print(f'XFS activation authorized. Start the node with: systemctl start {SERVICE}')


def rollback_pool(arguments):
    pool = pool_path()
    state = ROOT / 'state'
    record = load_migration()
    if record is None:
        raise MigrationError('No pool migration to roll back.')
    if record.get('activated'):
        raise MigrationError('The node has opened the XFS pool. Keep it mounted and restore from a verified backup if needed.')
    if arguments.dry_run:
        print(f'Rollback would unmount the XFS pool (phase {record["phase"]}), restore {record["old"]["unit"]} and the original ownership records.')
        return
    old = record['old']
    image = record['target'].get('image')
    mountpoint = migration_directory() / 'target'
    staging = mount_of(mountpoint)
    if staging:
        if staging.get('fstype') != 'xfs' or not record.get('uuid') or filesystem_uuid(staging['source']) != record['uuid']:
            raise MigrationError(f'{mountpoint} holds a filesystem outside the migration.')
    loop = owned_loop(record, Path(image)) if image else None
    if staging:
        checked('umount', str(mountpoint), timeout=300)
    if record['phase'] in ('planned', 'formatted'):
        # Switch never touched the original mount or its systemd files.
        remount_old_pool(old, pool)
        verify_restored_pool(old, mount_of(pool))
        if loop:
            checked('losetup', '--detach', str(loop))
        if image and record.get('createdImage'):
            image_path = Path(image)
            staged = staged_image_path(image_path, record['createdImage'])
            if staged.exists():
                descriptor = os.open(staged, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
                try:
                    check_image_inode(descriptor, record['createdImage'])
                finally:
                    os.close(descriptor)
                staged.unlink()
                sync_directory(image_path.parent)
        finish_rollback(record)
        return
    original_unit = back_up_unit(old)
    mounted = mount_of(pool)
    if mounted and new_pool_mounted(record, pool):
        checked('systemctl', 'stop', old['unit'], timeout=300)
    elif mounted:
        verify_old_pool(old, mounted)
    if image:
        loop_path = UNIT_DIRECTORY / LOOP_SERVICE
        if loop_path.exists():
            if loop_path.read_bytes() != loop_unit(Path(image), old['unit'], record['uuid']).encode():
                raise MigrationError(f'{LOOP_SERVICE} belongs to another pool.')
            if not record.get('imageLoop'):
                raise MigrationError('Cannot prove ownership of the installed loop service.')
            checked('systemctl', 'disable', '--now', LOOP_SERVICE, timeout=300)
        loop_path.unlink(missing_ok=True)
        loop = owned_loop(record, Path(image))
        if loop:
            checked('losetup', '--detach', str(loop))
    unit = UNIT_DIRECTORY / old['unit']
    link = pool_boot_link(old['unit'])
    previous_link = old.get('bootLink')
    current_link = boot_link_target(old['unit'])
    if previous_link is None and current_link == str(unit):
        link.unlink()
        sync_directory(link.parent)
    elif previous_link is not None and current_link is None:
        link.parent.mkdir(exist_ok=True)
        link.symlink_to(previous_link)
        sync_directory(link.parent)
    elif current_link != previous_link:
        raise MigrationError(f'{link} changed during migration. Inspect the dependency before rollback.')
    if original_unit is not None:
        atomic_bytes(unit, original_unit, 0o644)
    elif old['fragment'] is None and unit.exists() and NEW_POOL_UNIT in unit.read_text():
        # The original pool was mounted by hand: remove only the unit we wrote.
        unit.unlink()
    sync_directory(UNIT_DIRECTORY)
    checked('systemctl', 'daemon-reload')
    mounted = mount_of(pool)
    if mounted is None:
        if original_unit is not None:
            checked('systemctl', 'start', old['unit'], timeout=300)
        else:
            checked('mount', '-t', old['type'], '-o', old['options'], old['what'], str(pool), timeout=300)
    mounted = mount_of(pool)
    verify_restored_pool(old, mounted)
    pool_tool('rollback', state, migration_directory())
    pool_tool('check', state)
    finish_rollback(record)


def finish_rollback(record):
    migration_directory().rename(migration_directory().with_name(f'native-pool-migration.rolled-back-{time.time_ns()}'))
    sync_directory(migration_directory().parent)
    target = record['target'].get('image') or record['target']['device']
    print(f'Rolled back to the ext4 pool {record["old"]["what"]}. The target {target} was preserved.')


def pool_arguments(argv):
    import argparse
    parser = argparse.ArgumentParser(prog='native-host.py ' + argv[0])
    if argv[0] == 'migrate-pool':
        target = parser.add_mutually_exclusive_group()
        target.add_argument('--device', help='unused partition or logical volume for the new XFS pool')
        target.add_argument('--image', help='fully allocated image file with direct I/O')
        parser.add_argument('--size', help='image size, such as 500G')
        parser.add_argument('--force', action='store_true', help='overwrite an existing filesystem signature or image')
    parser.add_argument('--stop', action='store_true', help='stop cairn-native-node.service and cairn-native-* units first')
    parser.add_argument('--dry-run', action='store_true', help='print the plan without changing anything')
    arguments = parser.parse_args(argv[1:])
    if argv[0] == 'migrate-pool' and bool(arguments.image) != bool(arguments.size):
        parser.error('--image and --size are used together')
    return arguments


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
        elif len(sys.argv) >= 2 and sys.argv[1] == 'migrate-pool':
            require_installed_supervisor()
            offline_pool_operation(migrate_pool, pool_arguments(sys.argv[1:]))
        elif len(sys.argv) >= 2 and sys.argv[1] == 'activate-pool':
            require_installed_supervisor()
            offline_pool_operation(activate_pool, pool_arguments(sys.argv[1:]))
        elif len(sys.argv) >= 2 and sys.argv[1] == 'rollback-pool':
            offline_pool_operation(rollback_pool, pool_arguments(sys.argv[1:]))
        else:
            raise ValueError('Use install <manager HTTPS origin>, run, migrate-pool, activate-pool or rollback-pool.')
    except PrerequisiteError as error:
        print('Native node prerequisites unavailable: ' + error.diagnostic, file=sys.stderr)
        sys.exit(1)
    except MigrationError as error:
        print('Native pool migration stopped: ' + str(error), file=sys.stderr)
        sys.exit(1)
    except Exception as error:
        print('Native node failed: ' + type(error).__name__, file=sys.stderr)
        sys.exit(1)
