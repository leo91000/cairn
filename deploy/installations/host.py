#!/usr/bin/env python3
"""One-machine installation. The manager owns claiming; containers have no Docker socket."""
import argparse
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import stat
import uuid
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

ROOT = Path(os.environ.get('CAIRN_INSTALLATION_ROOT', '/var/lib/cairn-installation'))
GARAGE_IMAGE = 'dxflrs/garage@sha256:866bd13ed2038ba7e7190e840482bc27234c4afaf77be8cfa439ae088c1e4690'
IMAGE = re.compile(r'^ghcr\.io/leo91000/cairn@sha256:[a-f0-9]{64}$')
COMPOSE_WAIT_TIMEOUT = 240
COMPOSE_COMMAND_TIMEOUT = 300
UPDATE_PHASES = frozenset(('startup', 'configuration', 'release_lookup', 'image_pull', 'image_inspection',
                          'lease_acquire', 'lease_release', 'lease_ack_write', 'candidate_inspection',
                          'manager_health', 'runner_health', 'configure_runner', 'stop_manager',
                          'stop_runner', 'compose_write', 'compose_up', 'commit_candidate',
                          'clear_owner', 'complete'))
UPDATE_PHASE = 'startup'


class LeaseRejected(RuntimeError):
    """An explicit manager response must never authorize unavailable-manager recovery."""


def update_phase(value):
    global UPDATE_PHASE
    if value not in UPDATE_PHASES:
        raise ValueError('Invalid update phase')
    UPDATE_PHASE = value
    print(json.dumps({'event': 'installation_update_phase', 'phase': value}), file=sys.stderr, flush=True)


def update_failure(error):
    if isinstance(error, subprocess.TimeoutExpired):
        category = 'TimeoutExpired'
    elif isinstance(error, OSError):
        category = 'OSError'
    elif isinstance(error, ValueError):
        category = 'ValueError'
    elif isinstance(error, LeaseRejected):
        category = 'LeaseRejected'
    else:
        category = 'RuntimeError'
    phase = UPDATE_PHASE if UPDATE_PHASE in UPDATE_PHASES else 'startup'
    print(json.dumps({'event': 'installation_update_failed', 'phase': phase, 'exception': category}),
          file=sys.stderr, flush=True)


def atomic(path, data):
    with tempfile.NamedTemporaryFile('w', dir=path.parent, encoding='utf-8', delete=False) as file:
        temporary = Path(file.name)
        try:
            file.write(data)
            file.flush()
            os.fsync(file.fileno())
        except BaseException:
            temporary.unlink(missing_ok=True)
            raise
    try:
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)
    descriptor = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def run(args, timeout=120):
    # Docker errors can contain environment values. Keep credentials out of diagnostics.
    result = subprocess.run(args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=timeout, check=False,
                            env={key: value for key, value in os.environ.items()
                                 if key != 'CAIRN_INSTALLATION_CLAIM_CODE'})
    if result.returncode:
        operation = next((arg for arg in args if arg in ('pull', 'up', 'exec')), 'operation')
        raise RuntimeError(f'Docker {operation} failed (exit {result.returncode}). Check the daemon and outbound registry connectivity; existing data is retained.')


def beacon_origin(value):
    parsed = urllib.parse.urlsplit(value)
    loopback = parsed.hostname in ('localhost', '127.0.0.1', '::1')
    if not (parsed.scheme == 'https' or parsed.scheme == 'http' and loopback):
        raise RuntimeError('The Beacon requires HTTPS (HTTP only on loopback).')
    if not parsed.hostname or parsed.username or parsed.password or parsed.path not in ('', '/') or parsed.query or parsed.fragment:
        raise RuntimeError('Invalid Beacon origin.')
    return value.rstrip('/')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RuntimeError('Beacon redirects are not accepted.')


def release(origin):
    with urllib.request.build_opener(NoRedirect).open(origin + '/install/release', timeout=30) as response:
        body = response.read(8193)
        if len(body) > 8192:
            raise RuntimeError('Invalid installation release response.')
        image = json.loads(body).get('image', '')
        if not IMAGE.fullmatch(image):
            raise RuntimeError('Beacon must approve an immutable installation image.')
        return image


def node_supervisor():
    module_path = Path(__file__).with_name('node-host.py')
    if not module_path.exists():
        module_path = Path(__file__).parent.parent / 'nodes/host.py'
    spec = importlib.util.spec_from_file_location('node_supervisor', module_path)
    supervisor = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(supervisor)
    return supervisor


def configure_ublk(runner):
    # Detect and validate the running host before stopping any workload. Reuse
    # the standalone node's exact kernel/device checks and capability contract.
    arguments = node_supervisor().block_device_arguments()
    rules = [value.split('=', 1)[1] for value in arguments if value.startswith('--device-cgroup-rule=')]
    capabilities = runner.setdefault('cap_add', [])
    devices = runner.setdefault('devices', [])
    environment = runner.setdefault('environment', {})
    if not isinstance(capabilities, list) or not isinstance(devices, list) or not isinstance(environment, dict):
        raise ValueError('Runner capabilities/devices must be lists and environment a mapping')
    for capability in ('SYS_PTRACE', 'SYS_NICE', 'SYS_RESOURCE'):
        if capability not in capabilities:
            capabilities.append(capability)
    control = '/dev/ublk-control:/dev/ublk-control'
    if control not in devices:
        devices.append(control)
    runner['device_cgroup_rules'] = rules


def compose(image, origin):
    logs = {'driver': 'json-file', 'options': {'max-size': '10m', 'max-file': '3'}}
    deployment = {
        'name': 'cairn-installation',
        'services': {
            'garage': {
                'image': GARAGE_IMAGE, 'restart': 'unless-stopped',
                'command': ['/garage', 'server', '--single-node', '--default-bucket'],
                'env_file': ['garage.env'],
                'volumes': ['./garage.toml:/etc/garage.toml:ro', './garage:/var/lib/garage'],
                'healthcheck': {
                    'test': ['CMD', '/garage', 'bucket', 'info', 'cairn-disks'],
                    'interval': '5s', 'timeout': '5s', 'retries': 24,
                },
                'logging': logs,
            },
            'manager': {
                'image': image, 'user': '1000:1000', 'init': True, 'restart': 'unless-stopped',
                'stop_grace_period': '60s',
                'depends_on': {
                    'garage': {'condition': 'service_healthy'},
                    'runner': {'condition': 'service_healthy'},
                },
                'environment': {
                    'DATA_DIR': '/data', 'AGENT_HOME': '/home/node',
                    'HOST': '0.0.0.0', 'WORKSPACE_ROOTS': '/workspaces',
                    'PUBLIC_URL': 'http://manager:4310',
                    'RUNNER_URL': 'http://runner:4311',
                    'CAIRN_BEACON_ORIGIN': origin, 'CAIRN_NODE_IMAGE': image,
                    'CAIRN_INSTALLATION_CLAIM_CODE': '${CAIRN_INSTALLATION_CLAIM_CODE:-}',
                },
                'volumes': ['./data:/data', './home:/home/node', './workspaces:/workspaces'],
                'mem_limit': '4g', 'logging': logs,
                'healthcheck': {
                    'test': ['CMD', 'node', '-e', "fetch('http://127.0.0.1:4310/health').then(r=>{if(!r.ok)process.exit(1)}).catch(()=>process.exit(1))"],
                    'start_period': '120s', 'interval': '5s', 'timeout': '5s', 'retries': 24,
                },
            },
            'runner': {
                'image': image, 'user': '0:0', 'restart': 'unless-stopped',
                'entrypoint': ['/usr/local/bin/cairn', 'runner-broker'],
                'stop_grace_period': '30s', 'read_only': True,
                'environment': {'DATA_DIR': '/data'},
                'cap_drop': ['ALL'],
                'cap_add': ['SYS_ADMIN', 'SYS_PTRACE', 'SYS_NICE', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE'],
                'security_opt': ['apparmor:unconfined', 'seccomp:unconfined'],
                'devices': ['/dev/kvm:/dev/kvm', '/dev/net/tun:/dev/net/tun'],
                'sysctls': {'net.ipv4.ip_forward': '1', 'net.ipv6.conf.all.disable_ipv6': '1'},
                'tmpfs': ['/run', '/tmp'],
                'volumes': ['./data:/data', './runner-state:/runner-state'],
                'mem_limit': '20g', 'logging': logs,
                'healthcheck': {
                    'test': ['CMD', 'node', '-e', "fetch('http://127.0.0.1:4311/health').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"],
                    'start_period': '120s', 'interval': '5s', 'timeout': '5s', 'retries': 24,
                },
            },
        },
    }

    configure_ublk(deployment['services']['runner'])
    return deployment


def claimed_identity(origin):
    identity = ROOT / 'data/installation-relay/identity.json'
    if not identity.exists() and not identity.is_symlink():
        return False
    try:
        metadata = identity.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_mode & 0o077:
            raise ValueError('Private regular file required')
        data = json.loads(identity.read_text())
        uuid.UUID(data['installationId'])
        if data['origin'] != origin or not isinstance(data['token'], str) or not data['token']:
            raise ValueError('Invalid identity')
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        raise RuntimeError('The saved installation identity is incomplete or unsafe. Restore its private backup, or detach the previous installation in the beacon app before recovering it. The file is retained.') from None
    return True


def install(origin, code):
    origin = beacon_origin(origin)
    if code and not re.fullmatch('[a-f0-9]{64}', code):
        raise RuntimeError('Invalid claim code. Copy a new command from Add an installation.')
    cli = Path('/usr/local/bin/cairn')
    if os.getuid() == 0 and cli.exists() and not cli.read_bytes().startswith(b'#!/usr/bin/env bash\n# Cairn installation wrapper\n'):
        raise RuntimeError('An unrelated /usr/local/bin/cairn already exists. Move it before installing Cairn; it will not be overwritten.')
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(ROOT, 0o700)
    claimed = claimed_identity(origin)
    config_file = ROOT / 'installation.json'
    if config_file.exists():
        config = json.loads(config_file.read_text())
        if config['origin'] != origin:
            raise RuntimeError('This installation belongs to another beacon origin; its identity is retained.')
        image = config['image']
    else:
        image = release(origin)
        atomic(config_file, json.dumps({'image': image, 'origin': origin}))
    if not IMAGE.fullmatch(image):
        raise RuntimeError('Invalid saved installation image.')

    for name in ('data', 'home', 'workspaces', 'runner-state', 'garage'):
        directory = ROOT / name
        directory.mkdir(exist_ok=True, mode=0o700)
        if name in ('data', 'home', 'workspaces') and os.getuid() == 0:
            os.chown(directory, 1000, 1000)

    garage_env = ROOT / 'garage.env'
    if not garage_env.exists():
        access = 'GK' + secrets.token_hex(16)
        secret = secrets.token_hex(32)
        atomic(garage_env, f'GARAGE_DEFAULT_ACCESS_KEY={access}\nGARAGE_DEFAULT_SECRET_KEY={secret}\nGARAGE_DEFAULT_BUCKET=cairn-disks\n')
    values = dict(line.split('=', 1) for line in garage_env.read_text().splitlines())
    garage_config = ROOT / 'garage.toml'
    if not garage_config.exists():
        atomic(garage_config, f'''metadata_dir = "/var/lib/garage/meta"
data_dir = "/var/lib/garage/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "127.0.0.1:3901"
rpc_public_addr = "127.0.0.1:3901"
rpc_secret = "{secrets.token_hex(32)}"
[s3_api]
s3_region = "garage"
api_bind_addr = "0.0.0.0:3900"
''')
    storage = ROOT / 'data/storage-s3.json'
    if not storage.exists():
        atomic(storage, json.dumps({
            'bucket': 'cairn-disks', 'endpoint': 'http://garage:3900', 'region': 'garage',
            'integrated': True,
            'accessKeyId': values['GARAGE_DEFAULT_ACCESS_KEY'],
            'secretAccessKey': values['GARAGE_DEFAULT_SECRET_KEY'],
        }))
    if os.getuid() == 0:
        os.chown(storage, 1000, 1000)

    atomic(ROOT / 'compose.json', json.dumps(compose(image, origin), indent=2))
    claim_file = ROOT / 'claim.env'
    identity = ROOT / 'data/installation-relay/identity.json'
    # Always replace a claim left by an interrupted install with the current code.
    atomic(claim_file, 'CAIRN_INSTALLATION_CLAIM_CODE=' + (code if not claimed else '') + '\n')
    docker = ['docker', 'compose', '--project-directory', str(ROOT), '--env-file', str(claim_file), '-f', str(ROOT / 'compose.json')]
    print('Downloading Cairn and Garage images…', flush=True)
    startup_attempted = False
    try:
        run(docker + ['pull'], timeout=1200)
        print('Starting the manager, local runner and private S3 storage…', flush=True)
        startup_attempted = True
        run(docker + ['up', '-d'], timeout=360)
        health = "fetch('http://127.0.0.1:4310/health').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"
        deadline = time.monotonic() + 120
        while True:
            try:
                run(docker + ['exec', '-T', 'manager', 'node', '-e', health], timeout=10)
                break
            except RuntimeError:
                if time.monotonic() >= deadline:
                    raise RuntimeError('The manager did not become healthy. Inspect docker compose logs; existing data is retained.')
                time.sleep(1)
        if code:
            deadline = time.monotonic() + 20
            while not identity.exists() and time.monotonic() < deadline:
                time.sleep(1)
    finally:
        # Remove the one-use code from both disk and the manager environment.
        atomic(claim_file, '')
        try:
            if startup_attempted:
                run(docker + ['up', '-d', '--no-deps', 'manager'], timeout=180)
        finally:
            claim_file.unlink(missing_ok=True)
    if not (ROOT / '.env').exists():
        atomic(ROOT / '.env', '')
    if os.getuid() == 0:
        wrapper = '''#!/usr/bin/env bash
# Cairn installation wrapper
set -euo pipefail
[[ $(id -u) == 0 ]] || { echo 'Run sudo cairn claim.' >&2; exit 1; }
[[ "$*" == claim ]] || { echo 'Usage: sudo cairn claim' >&2; exit 1; }
exec docker compose --project-directory ROOT -f COMPOSE exec -T manager /usr/local/bin/cairn claim
'''.replace('ROOT', shlex.quote(str(ROOT))).replace('COMPOSE', shlex.quote(str(ROOT / 'compose.json')))
        atomic(Path('/usr/local/bin/cairn'), wrapper)
        os.chmod('/usr/local/bin/cairn', 0o755)
    if claimed_identity(origin):
        print('Cairn installed and claimed. Open the beacon app and refresh installations.')
    else:
        print('Cairn installed but unclaimed. Copy a fresh command from Add an installation and rerun it.')


def docker_output(args):
    result = subprocess.run(['docker', *args], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=10, check=False)
    if result.returncode:
        raise RuntimeError('Docker inspection failed; current image and data are retained.')
    # Compose emits no JSON records when a service has no container yet.
    return json.loads(result.stdout.strip() or '[]')


def update(origin):
    update_phase('configuration')
    origin = beacon_origin(origin)
    config_file = ROOT / 'installation.json'
    config = json.loads(config_file.read_text())
    if config['origin'] != origin or not IMAGE.fullmatch(config['image']):
        raise RuntimeError('Invalid saved installation configuration.')
    docker = ['docker', 'compose', '--project-directory', str(ROOT), '-f', str(ROOT / 'compose.json')]

    def lease(owner, release=False):
        update_phase('lease_release' if release else 'lease_acquire')
        # Read the credential inside the manager, never in argv or diagnostics.
        method = json.dumps('DELETE' if release else 'POST')
        lease_owner = json.dumps(owner)
        script = f"""
const fs = require('fs');
const token = fs.readFileSync('/data/maintenance-token', 'utf8').trim();
fetch('http://127.0.0.1:4310/internal/deployment-lease', {{
    method: {method},
    headers: {{ 'Content-Type': 'application/json', Authorization: 'Bearer ' + token }},
    body: JSON.stringify({{ owner: {lease_owner} }}),
}}).then(async response => {{
    let paused = null;
    try {{
        const value = await response.json();
        if (typeof value?.paused === 'boolean') paused = value.paused;
    }} catch {{}}
    console.log(JSON.stringify({{ status: response.status, paused }}));
}}).catch(() => process.exit(1));
"""
        verdict = docker_output(docker[1:] + ['exec', '-T', 'manager', 'node', '-e', script])
        status = verdict.get('status') if isinstance(verdict, dict) else None
        if status == 200 and verdict.get('paused') is (not release):
            return
        if status == 409:
            raise LeaseRejected('Another deployment owns the installation lease. Existing image and data are retained.')
        raise LeaseRejected('The manager refused the deployment lease. Existing image and data are retained.')

    def inspect(image):
        update_phase('image_inspection')
        metadata = docker_output(['image', 'inspect', image])[0]
        if image not in metadata.get('RepoDigests', []):
            raise RuntimeError('Downloaded image digest does not match the approved release.')
        runtime = next((value.removeprefix('APP_RUNTIME_ID=') for value in metadata['Config'].get('Env', [])
                        if value.startswith('APP_RUNTIME_ID=')), '')
        if not runtime:
            raise RuntimeError('Approved image lacks runtime identity.')
        return runtime

    def manager_health(runtime=None, service='manager'):
        update_phase('manager_health' if service == 'manager' else 'runner_health')
        expected_runtime = json.dumps(runtime)
        port = 4310 if service == 'manager' else 4311
        require_execution = json.dumps(service == 'manager')
        script = f"""
fetch('http://127.0.0.1:{port}/health', {{ signal: AbortSignal.timeout(5000) }}).then(response => {{
    if (!response.ok) throw Error();
    return response.json();
}}).then(value => {{
    const expected = {expected_runtime};
    console.log(JSON.stringify({{ healthy: value.status === 'ok' && (expected === null || value.runtimeId === expected)
        && (!{require_execution} || value.execution?.ready === true) }}));
}}).catch(() => console.log(JSON.stringify({{ healthy: false }})));
"""
        result = docker_output(docker[1:] + ['exec', '-T', service, 'node', '-e', script])
        if not isinstance(result, dict) or not isinstance(result.get('healthy'), bool):
            raise RuntimeError('Manager health probe did not return a verdict.')
        return result['healthy']

    def candidate_state(image):
        update_phase('candidate_inspection')
        deployment = json.loads((ROOT / 'compose.json').read_text())
        if not isinstance(deployment, dict):
            return None
        services = deployment.get('services', {})
        if not isinstance(services, dict):
            return None
        for role in ('manager', 'runner'):
            if not isinstance(services.get(role), dict) or services[role].get('image') != image:
                return None
        environment = services['manager'].get('environment', {})
        if not isinstance(environment, dict) or environment.get('CAIRN_NODE_IMAGE') != image:
            return None
        identities = {}
        for role in ('manager', 'runner'):
            rows = docker_output(docker[1:] + ['ps', '--all', '--format', 'json', role])
            container = rows[0] if isinstance(rows, list) and len(rows) == 1 else rows
            if not isinstance(container, dict) or not isinstance(container.get('ID'), str) or not container['ID']:
                return None
            if container.get('Image') != image or container.get('State') != 'running' or container.get('Health') != 'healthy':
                return None
            identities[role] = container['ID']
        return identities

    def healthy_candidate(image):
        runtime = inspect(image)
        before = candidate_state(image)
        if not before:
            return False
        if not manager_health(runtime) or not manager_health(runtime, 'runner'):
            return False
        return candidate_state(image) == before

    def launch(image):
        runtime = inspect(image)
        deployment = json.loads((ROOT / 'compose.json').read_text())
        deployment['services']['manager']['image'] = image
        deployment['services']['manager']['environment']['CAIRN_NODE_IMAGE'] = image
        deployment['services']['runner']['image'] = image
        update_phase('configure_runner')
        configure_ublk(deployment['services']['runner'])

        # The manager saves checkpoints before the runner is stopped. Persistent
        # volumes and Garage are never recreated or removed by this supervisor.
        update_phase('stop_manager')
        run(docker + ['stop', '--timeout', '300', 'manager'], timeout=310)
        update_phase('stop_runner')
        run(docker + ['stop', '--timeout', '60', 'runner'], timeout=70)
        update_phase('compose_write')
        atomic(ROOT / 'compose.json', json.dumps(deployment, indent=2))
        # Compose starts the runner before the manager. Allow up to a minute of
        # startup overhead in addition to the bounded health-wait budget.
        update_phase('compose_up')
        run(docker + ['up', '-d', '--wait', '--wait-timeout', str(COMPOSE_WAIT_TIMEOUT), '--pull', 'never', '--no-deps', 'runner', 'manager'], timeout=COMPOSE_COMMAND_TIMEOUT)

        # A JSON verdict proves the probe ran. Docker exec failures before the
        # probe starts remain transient, rather than blacklisting an untested image.
        if not manager_health(runtime):
            raise RuntimeError('Installation runtime health check failed.')

    def finish():
        update_phase('commit_candidate')
        config['image'] = config.pop('pendingImage')
        config.pop('leaseAcquired', None)
        # Keep the lease acknowledgement durable, including a lost DELETE reply.
        atomic(config_file, json.dumps(config))

    if config.get('pendingImage'):
        lease_confirmed = False
        try:
            lease(config['leaseOwner'])
        except LeaseRejected:
            raise
        except RuntimeError:
            containers = docker_output(docker[1:] + ['ps', '--all', '--format', 'json', 'manager'])
            manager = containers[0] if isinstance(containers, list) and containers else containers
            owned_image = manager and manager.get('Image') in (config['image'], config['pendingImage'])
            stopped = not manager or (owned_image and manager.get('State') in ('exited', 'dead', 'created', 'restarting'))
            unavailable_candidate = (owned_image and manager.get('Image') == config['pendingImage']
                                     and manager.get('State') == 'running'
                                     and not manager_health(inspect(config['pendingImage'])))
            if not config.get('leaseAcquired') or not (stopped or unavailable_candidate):
                raise
        else:
            lease_confirmed = True
            config['leaseAcquired'] = True
            update_phase('lease_ack_write')
            atomic(config_file, json.dumps(config))
        # An acknowledged, already-ready candidate only needs its durable commit
        # and lease release. Re-launching it repeats potentially slow restoration.
        if not (lease_confirmed and healthy_candidate(config['pendingImage'])):
            launch(config['pendingImage'])
        finish()

    if config.get('leaseOwner'):
        lease(config['leaseOwner'], release=True)
        config.pop('leaseOwner')
        update_phase('clear_owner')
        atomic(config_file, json.dumps(config))
        return

    update_phase('release_lookup')
    image = release(origin)
    if image == config['image']:
        return

    def pull(image):
        update_phase('image_pull')
        run(['docker', 'pull', image], timeout=1200)
        inspect(image)

    def prepare():
        owner = str(uuid.uuid4())
        # Record before requesting the lease: its response can be lost.
        config.update(pendingImage=image, leaseOwner=owner)
        atomic(config_file, json.dumps(config))

        lease(owner)

        config['leaseAcquired'] = True
        update_phase('lease_ack_write')
        atomic(config_file, json.dumps(config))
        return True

    pull(image)
    prepare()
    launch(image)
    finish()

    lease(config['leaseOwner'], release=True)
    config.pop('leaseOwner')
    update_phase('clear_owner')
    atomic(config_file, json.dumps(config))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('origin')
    parser.add_argument('--claim-code', default=os.environ.pop('CAIRN_INSTALLATION_CLAIM_CODE', ''))
    parser.add_argument('--update', action='store_true')
    args = parser.parse_args()
    try:
        ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
        with open(ROOT / 'install.lock', 'w', opener=lambda path, flags: os.open(path, flags, 0o600)) as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise RuntimeError('Another Cairn installer is running. Wait for it to finish and retry.') from None
            if args.update:
                update(args.origin)
                update_phase('complete')
            else:
                install(args.origin, args.claim_code)
    except RuntimeError as error:
        if args.update:
            update_failure(error)
        print(str(error), file=sys.stderr)
        sys.exit(1)
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        if args.update:
            update_failure(error)
        print('Cairn installation failed. Check prerequisites, outbound connectivity and Docker. Existing identity and storage are retained.', file=sys.stderr)
        sys.exit(1)
