#!/usr/bin/env python3
"""One-machine installation. The manager owns claiming; containers have no Docker socket."""
import argparse
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import time
import urllib.parse
import urllib.request

ROOT = Path(os.environ.get('LEO_INSTALLATION_ROOT', '/var/lib/leo-installation'))
GARAGE_IMAGE = 'dxflrs/garage@sha256:866bd13ed2038ba7e7190e840482bc27234c4afaf77be8cfa439ae088c1e4690'
IMAGE = re.compile(r'^ghcr\.io/leo91000/leo-agent-manager@sha256:[a-f0-9]{64}$')


def atomic(path, data):
    temporary = path.with_suffix('.tmp')
    with open(temporary, 'w', encoding='utf-8', opener=lambda p, f: os.open(p, f, 0o600)) as file:
        file.write(data)
        file.flush()
        os.fsync(file.fileno())
    os.replace(temporary, path)
    descriptor = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def run(args, timeout=120):
    # Docker errors can contain environment values. Keep credentials out of diagnostics.
    result = subprocess.run(args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=timeout, check=False)
    if result.returncode:
        raise RuntimeError('Docker operation failed. Check the daemon and outbound registry connectivity; existing data is retained.')


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
        'name': 'leo-installation',
        'services': {
            'garage': {
                'image': GARAGE_IMAGE, 'restart': 'unless-stopped',
                'command': ['/garage', 'server', '--single-node', '--default-bucket'],
                'env_file': ['garage.env'],
                'volumes': ['./garage.toml:/etc/garage.toml:ro', './garage:/var/lib/garage'],
                'logging': logs,
            },
            'manager': {
                'image': image, 'init': True, 'restart': 'unless-stopped',
                'stop_grace_period': '60s', 'depends_on': ['garage', 'runner'],
                'environment': {
                    'DATA_DIR': '/data', 'AGENT_HOME': '/home/node',
                    'RUNNER_URL': 'http://runner:4311',
                    'LEO_OFFICIAL_ORIGIN': origin, 'LEO_NODE_IMAGE': image,
                    'LEO_INSTALLATION_CLAIM_CODE': '${LEO_INSTALLATION_CLAIM_CODE:-}',
                },
                'volumes': ['./data:/data', './home:/home/node', './workspaces:/workspaces'],
                'mem_limit': '4g', 'cpus': 2, 'logging': logs,
            },
            'runner': {
                'image': image, 'user': '0:0', 'restart': 'unless-stopped',
                'entrypoint': ['/usr/local/bin/leo', 'runner-broker'],
                'stop_grace_period': '30s', 'read_only': True,
                'environment': {'DATA_DIR': '/data'},
                'cap_drop': ['ALL'],
                'cap_add': ['SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE'],
                'security_opt': ['apparmor:unconfined', 'seccomp:unconfined'],
                'devices': ['/dev/kvm:/dev/kvm', '/dev/net/tun:/dev/net/tun', '/dev/fuse:/dev/fuse'],
                'sysctls': {'net.ipv4.ip_forward': '1', 'net.ipv6.conf.all.disable_ipv6': '1'},
                'tmpfs': ['/run', '/tmp'],
                'volumes': ['./data:/data', './runner-state:/runner-state'],
                'mem_limit': '20g', 'cpus': 8, 'logging': logs,
            },
        },
    }


def install(origin, code):
    origin = official_origin(origin)
    if code and not re.fullmatch('[a-f0-9]{64}', code):
        raise RuntimeError('Invalid claim code. Copy a new command from Add an installation.')
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(ROOT, 0o700)
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
        atomic(garage_env, f'GARAGE_DEFAULT_ACCESS_KEY={access}\nGARAGE_DEFAULT_SECRET_KEY={secret}\nGARAGE_DEFAULT_BUCKET=leo-disks\n')
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
            'bucket': 'leo-disks', 'endpoint': 'http://garage:3900', 'region': 'garage',
            'accessKeyId': values['GARAGE_DEFAULT_ACCESS_KEY'],
            'secretAccessKey': values['GARAGE_DEFAULT_SECRET_KEY'],
        }))
        if os.getuid() == 0:
            os.chown(storage, 1000, 1000)

    atomic(ROOT / 'compose.json', json.dumps(compose(image, origin), indent=2))
    claim_file = ROOT / 'claim.env'
    identity = ROOT / 'data/installation-relay/identity.json'
    # Always replace a claim left by an interrupted install with the current code.
    atomic(claim_file, 'LEO_INSTALLATION_CLAIM_CODE=' + (code if not identity.exists() else '') + '\n')
    docker = ['docker', 'compose', '--project-directory', str(ROOT), '--env-file', str(claim_file), '-f', str(ROOT / 'compose.json')]
    print('Downloading Leo and Garage images…', flush=True)
    run(docker + ['pull'], timeout=1200)
    print('Starting the manager, local runner and private S3 storage…', flush=True)
    try:
        run(docker + ['up', '-d'], timeout=180)
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
            run(docker + ['up', '-d', '--no-deps', 'manager'], timeout=180)
        finally:
            claim_file.unlink(missing_ok=True)
    atomic(ROOT / '.env', '')
    if identity.exists():
        print('Leo installed and claimed. Open the official app and refresh installations.')
    else:
        print('Leo installed but unclaimed. Run sudo leo claim and confirm its code in the official app.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('origin')
    parser.add_argument('--claim-code', default='')
    args = parser.parse_args()
    try:
        install(args.origin, args.claim_code)
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
        print('Leo installation failed. Check prerequisites, outbound connectivity and Docker. Existing identity and storage are retained.', file=sys.stderr)
        sys.exit(1)
