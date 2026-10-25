"""CLI fixtures keep real update logic; kernel checks have dedicated unit tests."""
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def write_supervisors(root):
    directory = root / 'supervisors'
    directory.mkdir(exist_ok=True)
    source = (REPO / 'deploy/nodes/host.py').read_text()
    start = source.index('def block_device_arguments(')
    end = source.index('\n\ndef vm_arguments(', start)
    source = source[:start] + '''def block_device_arguments(transport='ublk'):
    if os.environ.get('FAILURE') == 'ublk':
        raise RuntimeError('Fixture ublk prerequisites unavailable')
    return ['--cap-add=SYS_RESOURCE', '--device=/dev/ublk-control',
            '--device-cgroup-rule=c 238:* rwm', '--device-cgroup-rule=b 259:* rwm']
''' + source[end:]
    node = directory / 'node-host.py'
    node.write_text(source)
    installation = directory / 'host.py'
    installation.write_bytes((REPO / 'deploy/installations/host.py').read_bytes())
    return installation, node
