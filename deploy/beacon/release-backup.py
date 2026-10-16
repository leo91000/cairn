#!/usr/bin/env python3
"""Root-owned release preparation, reached through a restricted SSH command.

Hosts already require Python for the installation supervisor. No database content
or credentials leave the host except as an age-encrypted recovery artifact.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time
import urllib.request

CONFIG = Path('/etc/cairn-release-backup.json')
VERSION = re.compile(r'v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z')
IDENTIFIER = re.compile(r'[a-f0-9]{32}\Z')
DIGEST = re.compile(r'sha256:[a-f0-9]{64}\Z')


def request(value):
    parts = shlex.split(value)
    if len(parts) == 3 and parts[0] == 'prepare' and VERSION.fullmatch(parts[1]) and IDENTIFIER.fullmatch(parts[2]):
        return parts
    if len(parts) == 2 and parts[0] == 'download' and IDENTIFIER.fullmatch(parts[1]):
        return parts
    if len(parts) == 2 and parts[0] == 'verify' and DIGEST.fullmatch(parts[1]):
        return parts
    raise ValueError('Unsupported release backup request')


def run(*args, **options):
    """Keep subprocess errors private; in particular pg_restore can echo data."""
    result = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **options)
    if result.returncode:
        raise RuntimeError('Release preparation operation failed')


def output(*args):
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError('Release preparation read failed')
    return result.stdout.strip()


def save(path, value):
    temporary = path.with_suffix('.partial')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def container(config, role):
    ids = output('docker', 'ps', '-q', '--filter', 'label=com.docker.compose.project=' + config['project'], '--filter', 'label=com.docker.compose.service=' + role).splitlines()
    if len(ids) != 1:
        raise RuntimeError('Expected exactly one running ' + role)
    return ids[0]


def image(container_id):
    identity = output('docker', 'inspect', '--format', '{{.Image}}', container_id)
    digests = json.loads(output('docker', 'image', 'inspect', '--format', '{{json .RepoDigests}}', identity))
    if not digests:
        raise RuntimeError('The running image has no immutable repository digest')
    return identity, digests


def public(config, path):
    with urllib.request.urlopen(config['origin'] + path, timeout=20) as response:
        return json.loads(response.read(65536))


def tables(database):
    sql = "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE'"
    return int(output('docker', 'exec', database, 'psql', '-U', 'cairn', '-d', 'cairn_beacon', '-Atc', sql))


def recover(config, identifier):
    # Also clean an interrupted restore when systemd times out or kills a worker.
    subprocess.run(['docker', 'rm', '-fv', 'cairn-release-restore-' + identifier], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    state = Path(config['directory']) / identifier / 'state.json'
    if not state.exists():
        return
    saved = json.loads(state.read_text())
    if not saved.get('resume'):
        return
    beacon = saved['beacon']
    labels = json.loads(output('docker', 'inspect', '--format', '{{json .Config.Labels}}', beacon))
    if labels.get('com.docker.compose.project') != config['project'] or labels.get('com.docker.compose.service') != 'beacon':
        raise RuntimeError('Recovery container no longer belongs to Beacon')
    running = output('docker', 'ps', '-q', '--filter', 'label=com.docker.compose.project=' + config['project'], '--filter', 'label=com.docker.compose.service=beacon').splitlines()
    if any(item != beacon for item in running):
        raise RuntimeError('Another Beacon container is already running; refusing to start an old one')
    run('docker', 'start', beacon)
    save(state, {'beacon': beacon, 'resume': False})


def backup(config, version, identifier):
    directory = Path(config['directory']) / identifier
    directory.mkdir(mode=0o700)
    beacon = container(config, 'beacon')
    postgres = container(config, 'postgres')
    postgres_image, _ = image(postgres)
    _, beacon_images = image(beacon)
    health = public(config, '/health')
    installation = public(config, '/install/release')
    if health.get('status') != 'ok' or not re.fullmatch('[a-f0-9]{40}', health.get('commit', '')):
        raise RuntimeError('Current Beacon health identity is unavailable')
    previous = {'commit': health['commit'], 'beaconImage': beacon_images[0], 'installationImage': installation['image']}
    dump = directory / 'beacon.dump'
    save(directory / 'state.json', {'beacon': beacon, 'resume': True})
    try:
        run('docker', 'stop', '--time', '60', beacon)
        source_tables = tables(postgres)
        run('bash', config['helper'], 'backup', postgres, str(dump))
    finally:
        recover(config, identifier)

    restore = 'cairn-release-restore-' + identifier
    try:
        run('docker', 'run', '-d', '--name', restore, '--network', 'none', '--memory=512m', '--tmpfs', '/var/lib/postgresql/data:rw,nosuid,size=1g', '-e', 'POSTGRES_USER=cairn', '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', '-e', 'POSTGRES_DB=cairn_beacon', postgres_image)
        # TCP opens only after the image's temporary bootstrap server has exited.
        for _ in range(120):
            ready = subprocess.run(['docker', 'exec', restore, 'pg_isready', '-h', '127.0.0.1', '-U', 'cairn', '-d', 'cairn_beacon'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if ready.returncode == 0:
                break
            time.sleep(0.5)
        else:
            raise RuntimeError('Disposable Postgres did not become ready')
        run('bash', config['helper'], 'restore', restore, str(dump))
        restored_tables = tables(restore)
        if restored_tables != source_tables or restored_tables == 0:
            raise RuntimeError('Restored schema does not match the backup source')
    finally:
        run('docker', 'rm', '-fv', restore)

    ciphertext = directory / 'beacon.dump.age'
    run('age', '-R', config['recipient'], '-o', str(ciphertext), str(dump))
    proof = {
        'schema': 1,
        'id': identifier,
        'release': version,
        'createdAt': int(time.time() * 1000),
        'previous': previous,
        'restore': {'verified': True, 'sourceTables': source_tables, 'restoredTables': restored_tables},
        'encryptedSize': ciphertext.stat().st_size,
        'encryptedSha256': hashlib.file_digest(ciphertext.open('rb'), 'sha256').hexdigest(),
    }
    save(directory / 'release.json', proof)
    dump.unlink()


def verify(config, expected):
    try:
        result = {}
        for role in ['manager', 'runner']:
            ids = output('docker', 'ps', '-q', '--filter', 'label=com.docker.compose.project=' + config['installation_project'], '--filter', 'label=com.docker.compose.service=' + role).splitlines()
            if len(ids) != 1:
                return {'ready': False}
            _, digests = image(ids[0])
            result[role] = any(digest.endswith('@' + expected) for digest in digests)
            if role == 'manager':
                health = json.loads(output('docker', 'exec', ids[0], 'curl', '-fsS', 'http://127.0.0.1:4310/health'))
                result['health'] = health.get('status') == 'ok' and health.get('maintenance') is False and health.get('execution', {}).get('ready') is True
        return {'ready': all(result.values()), **result}
    except (RuntimeError, ValueError):
        return {'ready': False}


def main():
    os.umask(0o077)
    if os.geteuid() != 0:
        raise RuntimeError('Release helper must be installed as a root-owned forced command')
    config = json.loads(CONFIG.read_text())
    if len(sys.argv) == 4 and sys.argv[1] == 'worker':
        _, version, identifier = request('prepare ' + sys.argv[2] + ' ' + sys.argv[3])
        with Path('/run/lock/cairn-release-backup.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            backup(config, version, identifier)
        return
    if len(sys.argv) == 3 and sys.argv[1] == 'recover':
        if not IDENTIFIER.fullmatch(sys.argv[2]):
            raise ValueError('Invalid recovery identifier')
        recover(config, sys.argv[2])
        return
    if len(sys.argv) != 1:
        raise ValueError('Unexpected helper arguments')
    parts = request(os.environ.get('SSH_ORIGINAL_COMMAND', ''))
    if parts[0] == 'verify':
        print(json.dumps(verify(config, parts[1])))
        return
    identifier = parts[-1]
    directory = Path(config['directory']) / identifier
    if parts[0] == 'prepare':
        script = str(Path(__file__).resolve())
        run('systemd-run', '--quiet', '--wait', '--collect', '--unit=cairn-release-backup-' + identifier, '--property=RuntimeMaxSec=900', '--property=ExecStopPost=/usr/bin/python3 ' + script + ' recover ' + identifier, '/usr/bin/python3', script, 'worker', parts[1], identifier)
        print((directory / 'release.json').read_text(), end='')
        return
    # Only completed encrypted artifacts are downloadable, never plaintext dumps.
    proof = json.loads((directory / 'release.json').read_text())
    ciphertext = directory / 'beacon.dump.age'
    if hashlib.file_digest(ciphertext.open('rb'), 'sha256').hexdigest() != proof['encryptedSha256']:
        raise RuntimeError('Encrypted recovery artifact changed')
    with ciphertext.open('rb') as source:
        shutil.copyfileobj(source, sys.stdout.buffer)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print('Release backup request failed; inspect the private host service logs.', file=sys.stderr)
        sys.exit(1)
