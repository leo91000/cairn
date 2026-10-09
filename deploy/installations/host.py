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
IMAGE = re.compile(r'^ghcr\.io/leo91000/leo-agent-manager@sha256:[a-f0-9]{64}$')


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


def official_origin(value):
    parsed = urllib.parse.urlsplit(value)
    loopback = parsed.hostname in ('localhost', '127.0.0.1', '::1')
    if not (parsed.scheme == 'https' or parsed.scheme == 'http' and loopback):
        raise RuntimeError('The official service requires HTTPS (HTTP only on loopback).')
    if not parsed.hostname or parsed.username or parsed.password or parsed.path not in ('', '/') or parsed.query or parsed.fragment:
        raise RuntimeError('Invalid official service origin.')
    return value.rstrip('/')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RuntimeError('Official service redirects are not accepted.')


def release(origin):
    with urllib.request.build_opener(NoRedirect).open(origin + '/install/release', timeout=30) as response:
        body = response.read(8193)
        if len(body) > 8192:
            raise RuntimeError('Invalid installation release response.')
        image = json.loads(body).get('image', '')
        if not IMAGE.fullmatch(image):
            raise RuntimeError('The official service must approve an immutable installation image.')
        return image


def compose(image, origin):
    logs = {'driver': 'json-file', 'options': {'max-size': '10m', 'max-file': '3'}}
    return {
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
                'cap_add': ['SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE'],
                'security_opt': ['apparmor:unconfined', 'seccomp:unconfined'],
                'devices': ['/dev/kvm:/dev/kvm', '/dev/net/tun:/dev/net/tun', '/dev/fuse:/dev/fuse'],
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
        raise RuntimeError('The saved installation identity is incomplete or unsafe. Restore its private backup, or detach the previous installation in the official app before recovering it. The file is retained.') from None
    return True


def install(origin, code):
    origin = official_origin(origin)
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
            raise RuntimeError('This installation belongs to another official origin; its identity is retained.')
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
        print('Cairn installed and claimed. Open the official app and refresh installations.')
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
    origin = official_origin(origin)
    config_file = ROOT / 'installation.json'
    config = json.loads(config_file.read_text())
    if config['origin'] != origin or not IMAGE.fullmatch(config['image']):
        raise RuntimeError('Invalid saved installation configuration.')
    docker = ['docker', 'compose', '--project-directory', str(ROOT), '-f', str(ROOT / 'compose.json')]

    def lease(owner, release=False):
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
}}).then(response => {{
    if (!response.ok) process.exit(1);
}}).catch(() => process.exit(1));
"""
        run(docker + ['exec', '-T', 'manager', 'node', '-e', script], timeout=10)

    def inspect(image):
        metadata = docker_output(['image', 'inspect', image])[0]
        if image not in metadata.get('RepoDigests', []):
            raise RuntimeError('Downloaded image digest does not match the approved release.')
        runtime = next((value.removeprefix('APP_RUNTIME_ID=') for value in metadata['Config'].get('Env', [])
                        if value.startswith('APP_RUNTIME_ID=')), '')
        if not runtime:
            raise RuntimeError('Approved image lacks runtime identity.')
        return runtime

    def manager_health(runtime=None):
        expected_runtime = json.dumps(runtime)
        script = f"""
fetch('http://127.0.0.1:4310/health', {{ signal: AbortSignal.timeout(5000) }}).then(response => {{
    if (!response.ok) throw Error();
    return response.json();
}}).then(value => {{
    const expected = {expected_runtime};
    console.log(JSON.stringify({{ healthy: value.status === 'ok' && (expected === null || value.runtimeId === expected) }}));
}}).catch(() => console.log(JSON.stringify({{ healthy: false }})));
"""
        result = docker_output(docker[1:] + ['exec', '-T', 'manager', 'node', '-e', script])
        if not isinstance(result, dict) or not isinstance(result.get('healthy'), bool):
            raise RuntimeError('Manager health probe did not return a verdict.')
        return result['healthy']

    def failed_candidate_container(image):
        # A Docker command error alone does not prove an image unhealthy. Only
        # inspect the exact candidate's containers, including the dependency that
        # can keep the manager from starting at all.
        for service in ('manager', 'runner'):
            try:
                containers = docker_output(docker[1:] + ['ps', '--all', '--format', 'json', service])
                container = containers[0] if isinstance(containers, list) and containers else containers
                if container and container.get('Image') == image:
                    if container.get('State') in ('exited', 'dead', 'restarting') or container.get('Health') == 'unhealthy':
                        return True
            except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
                continue
        return False

    health_failed = False

    def launch(image):
        nonlocal health_failed
        # Inspection + stop + startup + HTTP verification total at most 550s.
        # Candidate and rollback, including failure inspection, fit in the lease.
        runtime = inspect(image)
        deployment = json.loads((ROOT / 'compose.json').read_text())
        deployment['services']['manager']['image'] = image
        deployment['services']['manager']['environment']['CAIRN_NODE_IMAGE'] = image
        deployment['services']['runner']['image'] = image

        # The manager saves checkpoints before the runner is stopped. Persistent
        # volumes and Garage are never recreated or removed by this supervisor.
        run(docker + ['stop', '--timeout', '300', 'manager'], timeout=310)
        run(docker + ['stop', '--timeout', '60', 'runner'], timeout=70)
        atomic(ROOT / 'compose.json', json.dumps(deployment, indent=2))
        try:
            run(docker + ['up', '-d', '--wait', '--wait-timeout', '120', '--pull', 'never', '--no-deps', 'runner', 'manager'], timeout=150)
        except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
            if image == config.get('pendingImage'):
                health_failed = failed_candidate_container(image)
            raise

        # A JSON verdict proves the probe ran. Docker exec failures before the
        # probe starts remain transient, rather than blacklisting an untested image.
        if not manager_health(runtime):
            if image == config.get('pendingImage'):
                health_failed = True
            raise RuntimeError('Installation runtime health check failed.')

    def finish(updated):
        pending = config.pop('pendingImage')
        config.pop('leaseAcquired', None)
        if updated:
            config.update(previousImage=config['image'], image=pending, failedImage=None)
        elif updated is False:
            config['failedImage'] = pending
        # Keep the lease acknowledgement durable, including a lost DELETE reply.
        atomic(config_file, json.dumps(config))

    if config.get('pendingImage'):
        # A stopped supervisor never guesses whether the candidate was healthy.
        # Recover the last committed approved image before accepting another.
        managers = docker_output(docker[1:] + ['ps', '--all', '--format', 'json', 'manager'])
        manager = managers[0] if isinstance(managers, list) and managers else managers
        manager_state = manager.get('State') if manager else None
        pending_unavailable = False
        if manager and manager.get('Image') == config['pendingImage']:
            pending_unavailable = manager_state in ('created', 'restarting')
            if manager_state == 'running' and config.get('leaseAcquired'):
                # Docker's running state does not imply HTTP readiness after a
                # reboot. Probe only our journaled candidate, never a foreign image.
                try:
                    pending_unavailable = not manager_health()
                except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
                    # No verdict: the candidate may be healthy and another
                    # deployment may hold the lease. Its lease still wins.
                    pending_unavailable = False
        manager_active = manager_state not in (None, 'exited', 'dead') and not pending_unavailable
        if not config.get('leaseAcquired') or manager_active:
            # A persisted acknowledgement may outlive the twenty-minute lease.
            # Reacquire before stopping a running manager; another owner wins.
            # If our replacement removed/stopped it or its exact candidate is
            # not started or unhealthy, the acknowledgement and host lock permit restoring
            # only the last committed approved image without its HTTP endpoint.
            lease(config['leaseOwner'])
        launch(config['image'])
        finish(None)

    if config.get('leaseOwner'):
        lease(config['leaseOwner'], release=True)
        config.pop('leaseOwner')
        atomic(config_file, json.dumps(config))
        return

    image = release(origin)
    if image in (config['image'], config.get('failedImage')):
        return

    def pull(image):
        run(['docker', 'pull', image], timeout=1200)
        inspect(image)

    def prepare():
        owner = str(uuid.uuid4())
        # Record before requesting the lease: its response can be lost.
        config.update(pendingImage=image, leaseOwner=owner)
        atomic(config_file, json.dumps(config))

        lease(owner)

        config['leaseAcquired'] = True
        atomic(config_file, json.dumps(config))
        return True

    module_path = Path(__file__).with_name('node-host.py')
    if not module_path.exists():
        module_path = Path(__file__).parent.parent / 'nodes/host.py'
    spec = importlib.util.spec_from_file_location('node_supervisor', module_path)
    supervisor = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(supervisor)

    updated = supervisor.update_image(config['image'], image, pull, prepare, launch)
    # Restoring the previous image alone does not prove the candidate failed.
    # None preserves eligibility after a transient Docker failure.
    if updated is False and not health_failed:
        updated = None

    finish(updated)

    lease(config['leaseOwner'], release=True)
    config.pop('leaseOwner')
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
            else:
                install(args.origin, args.claim_code)
    except RuntimeError as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
    except (OSError, ValueError, subprocess.TimeoutExpired):
        print('Cairn installation failed. Check prerequisites, outbound connectivity and Docker. Existing identity and storage are retained.', file=sys.stderr)
        sys.exit(1)
