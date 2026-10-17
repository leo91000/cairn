#!/usr/bin/env python3
"""Read-only release readiness checks through a root-owned restricted SSH command."""
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys

CONFIG = Path('/etc/cairn-release.json')
DIGEST = re.compile(r'sha256:[a-f0-9]{64}\Z')


def request(value):
    parts = shlex.split(value)
    if len(parts) == 2 and parts[0] == 'verify' and DIGEST.fullmatch(parts[1]):
        return parts
    raise ValueError('Unsupported release verification request')


def output(*args):
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError('Release preparation read failed')
    return result.stdout.strip()


def image(container_id):
    identity = output('docker', 'inspect', '--format', '{{.Image}}', container_id)
    digests = json.loads(output('docker', 'image', 'inspect', '--format', '{{json .RepoDigests}}', identity))
    if not digests:
        raise RuntimeError('The running image has no immutable repository digest')
    return identity, digests


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
    if os.geteuid() != 0 or len(sys.argv) != 1:
        raise RuntimeError('Release verifier must be a root-owned forced command')
    parts = request(os.environ.get('SSH_ORIGINAL_COMMAND', ''))
    config = json.loads(CONFIG.read_text())
    print(json.dumps(verify(config, parts[1])))


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print('Release verification failed; inspect the private host service logs.', file=sys.stderr)
        sys.exit(1)
