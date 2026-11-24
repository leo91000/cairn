#!/usr/bin/env python3
"""Disposable-host qualification of `native-host.py migrate-pool` (ext4 to XFS).

Seeds quota-owned environments on an ext4 pool through the controller's own
code, migrates them with the real host script and systemd units, checks that
the mount survives a simulated reboot, rolls back, migrates again and exercises
the migrated pool. Run as root by scripts/qualify-native-quota.mjs only.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time

REPOSITORY = Path(__file__).resolve().parents[2]
HOST = REPOSITORY / 'deploy/nodes/native-host.py'
TEST = 'native::quota::migration::tests::kernel_pool_migration_qualification'
UNITS = Path('/etc/systemd/system')


def fixture_loop_service(root):
    return 'cairn-native-migration-fixture-' + hashlib.sha256(os.fsencode(root)).hexdigest()[:16] + '.service'


def command(*args, check=True, timeout=600, env=None):
    result = subprocess.run([str(arg) for arg in args], capture_output=True, text=True, timeout=timeout, env=env)
    if check and result.returncode != 0:
        raise AssertionError(f'{args[0]} {args[1] if len(args) > 1 else ""} failed ({result.returncode}):\n{result.stdout}\n{result.stderr}')
    return result


def phase(binary, root, name):
    environment = {
        'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'HOME': str(root / 'home'), 'CAIRN_NODE_BACKEND': 'native',
        'RUNNER_STATE_DIR': str(root / 'state'), 'CAIRN_NATIVE_DISPOSABLE_HOST': '1',
        'CAIRN_NATIVE_QUOTA_TEST_STATE': str(root / 'state'), 'CAIRN_NATIVE_MIGRATION_PHASE': name,
        'CAIRN_NATIVE_MIGRATION_EXPECTED': str(root / 'expected.json'),
    }
    result = command('setpriv', '--bounding-set=-sys_resource', binary, '--exact', TEST, '--ignored', '--nocapture',
                     '--test-threads=1', env=environment, check=False)
    print(result.stdout, result.stderr, sep='\n')
    assert result.returncode == 0 and 'pool-migration-phase-ok' in result.stdout, f'Migration fixture phase {name} failed.'


def host(root, cairn, *args, refusal=None):
    environment = {'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LANG': 'C.UTF-8', 'CAIRN_NATIVE_DISPOSABLE_HOST': '1',
                   'CAIRN_NATIVE_ROOT': str(root), 'CAIRN_NATIVE_INSTALL': str(root), 'CAIRN_NATIVE_BINARY': str(cairn),
                   'CAIRN_NATIVE_FIXTURE_LOOP_SERVICE': fixture_loop_service(root)}
    result = command(sys.executable, '-B', HOST, *args, env=environment, check=False, timeout=1800)
    print(result.stdout, result.stderr, sep='\n')
    if refusal:
        assert result.returncode != 0 and refusal in result.stderr, f'native-host.py {args[0]} did not refuse as expected.'
        return result.stderr
    assert result.returncode == 0, f'native-host.py {args[0]} failed.'
    return result.stdout


def mounted(path):
    result = command('findmnt', '--noheadings', '--mountpoint', path, '--output', 'FSTYPE', check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def direct_io(image):
    devices = command('losetup', '--noheadings', '--output', 'NAME', '--associated', image).stdout.split()
    assert len(devices) == 1, f'{image} is attached {len(devices)} times.'
    queue = Path('/sys/class/block') / Path(devices[0]).name / 'queue/discard_max_bytes'
    assert queue.read_text().strip() == '0', 'Loop discard can punch a hole in the backing file.'
    backing = image.stat()
    assert backing.st_blocks * 512 >= backing.st_size, 'The backing image is no longer fully allocated.'
    return command('losetup', '--noheadings', '--output', 'DIO', devices[0]).stdout.strip() == '1'


def eventually(check, timeout=60):
    deadline = time.monotonic() + timeout
    while not check():
        assert time.monotonic() < deadline, 'Timed out waiting for the pool mount.'
        time.sleep(0.5)


def cleanup(root, pool, unit):
    loop_service = fixture_loop_service(root)
    files = [path for name in (unit, loop_service) if (path := UNITS / name).exists() or path.is_symlink()]
    if files:
        record = json.loads((root / 'native-pool-migration/state.json').read_bytes())
        image = Path(record['target']['image'])
        assert image.parent == root, 'Refusing to clean up a loop image outside the fixture.'
        spec = importlib.util.spec_from_file_location('migration_host', HOST)
        migration_host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(migration_host)
        migration_host.LOOP_SERVICE = loop_service
        migration_host.ROOT = root
        migration_host.INSTALL = root
        expected = {
            unit: migration_host.pool_unit(pool, record['uuid'], image, record['old'].get('restrictions', ())),
            loop_service: migration_host.loop_unit(image, unit, record['uuid']),
        }
        for path in files:
            assert path.is_file() and not path.is_symlink() and path.read_text() == expected[path.name], \
                'Refusing to clean up a systemd unit outside the fixture ownership record.'
        for path in files:
            command('systemctl', 'disable', '--now', path.name)
            path.unlink()
    command('systemctl', 'daemon-reload', check=False)
    for path in (pool, *root.glob('native-pool-migration*/target')):
        if mounted(path):
            command('umount', path)
    for image in root.glob('native-data*'):
        for device in command('losetup', '--noheadings', '--output', 'NAME', '--associated', image, check=False).stdout.split():
            command('losetup', '--detach', device, check=False)
    mounts = Path('/proc/self/mountinfo').read_text()
    assert not any(line.split()[4].startswith(str(root) + '/') for line in mounts.splitlines()), \
        'Fixture still owns mounts; retained for diagnosis.'
    command('rm', '-rf', '--one-file-system', '--', root)


def main(binary, cairn, fixture):
    root = Path(fixture).resolve()
    root.chmod(0o700)
    state = root / 'state'
    pool = state / 'native-data'
    pool.mkdir(parents=True)
    state.chmod(0o700)
    (root / 'home').mkdir()
    (root / 'host.py').write_bytes(HOST.read_bytes())
    unit = command('systemd-escape', '--path', '--suffix=mount', pool).stdout.strip()
    loop_service = fixture_loop_service(root)
    for name in (unit, loop_service):
        path = UNITS / name
        assert not path.exists() and not path.is_symlink(), f'Fixture unit name is occupied: {name}.'
    try:
        original = root / 'native-data.ext4'
        command('truncate', '-s', '1G', original)
        command('mkfs.ext4', '-q', '-F', '-O', 'project,quota', '-E', 'quotatype=prjquota', original)
        command('mount', '-o', 'loop,prjquota,nosuid,nodev,noexec', original, pool)
        phase(binary, root, 'seed')

        image = root / 'native-data.xfs'
        plan = host(root, cairn, 'migrate-pool', '--image', image, '--size', '2G', '--dry-run')
        assert 'nothing changed' in plan and not image.exists(), 'Dry run changed the host.'
        host(root, cairn, 'migrate-pool', '--image', image, '--size', '2G')
        assert mounted(pool) == 'xfs' and direct_io(image), 'Migration did not mount the direct-I/O XFS pool.'
        options = command('findmnt', '--noheadings', '--mountpoint', pool, '--output', 'OPTIONS').stdout.strip().split(',')
        assert {'nosuid', 'nodev', 'noexec'}.issubset(options), 'Cutover dropped source mount restrictions.'
        command('fstrim', pool, check=False)
        assert direct_io(image), 'Trimming the pool changed its backing allocation.'
        assert (UNITS / unit).exists() and (UNITS / loop_service).exists(), 'Persistent pool units are missing.'

        # A reboot detaches the loop device; the units must restore both.
        command('systemctl', 'stop', unit)
        command('systemctl', 'stop', loop_service)
        assert not command('losetup', '--noheadings', '--associated', image).stdout.strip(), 'Loop device survived its unit.'
        retained_image = image.with_suffix('.retained')
        image.rename(retained_image)
        try:
            image.touch(mode=0o600)
            failed = command('systemctl', 'start', loop_service, check=False)
            assert failed.returncode != 0, 'Boot accepted a replaced pool backing inode.'
            assert not command('losetup', '--noheadings', '--associated', image).stdout.strip(), 'Boot attached the replacement.'
        finally:
            image.unlink(missing_ok=True)
            retained_image.rename(image)
        command('systemctl', 'start', unit, timeout=180)
        eventually(lambda: mounted(pool) == 'xfs')
        assert direct_io(image), 'The boot-time loop device lost direct I/O.'
        record = json.loads((root / 'native-pool-migration/state.json').read_bytes())
        device = Path('/run') / f'cairn-native-pool-{record["uuid"]}' / 'device'
        assert device.is_symlink() and pool.stat().st_dev == device.stat().st_rdev, 'Boot mounted outside the verified loop attachment.'
        assert 'migrated to XFS' in host(root, cairn, 'migrate-pool'), 'A completed migration did not rerun idempotently.'
        phase(binary, root, 'migrated')

        host(root, cairn, 'rollback-pool')
        assert mounted(pool) == 'ext4', 'Rollback did not remount the original pool.'
        assert not (UNITS / loop_service).exists(), 'Rollback kept the loop unit.'
        assert not (UNITS / 'local-fs.target.wants' / unit).is_symlink(), 'Rollback kept the XFS boot link for a manually mounted ext4 pool.'
        phase(binary, root, 'rolled-back')

        second = root / 'native-data-2.xfs'
        host(root, cairn, 'migrate-pool', '--image', second, '--size', '2G')
        assert mounted(pool) == 'xfs' and direct_io(second), 'Second migration did not mount its pool.'
        phase(binary, root, 'migrated')
        host(root, cairn, 'activate-pool', '--dry-run')
        phase(binary, root, 'migrated')
        command('mount', '-o', 'remount,ro,prjquota,nosuid,nodev,noexec', pool)
        host(root, cairn, 'activate-pool', refusal='read-only')
        command('mount', '-o', 'remount,rw,prjquota,nosuid,nodev,noexec', pool)
        host(root, cairn, 'activate-pool')
        command('mount', '-o', 'remount,ro,prjquota,nosuid,nodev,noexec', pool)
        phase(binary, root, 'authorized-read-only')
        receipt = json.loads((root / 'native-pool-migration/state.json').read_bytes())
        assert not receipt.get('activated'), 'A read-only pool consumed rollback authority.'
        command('mount', '-o', 'remount,rw,prjquota,nosuid,nodev,noexec', pool)
        phase(binary, root, 'exercise')
    finally:
        cleanup(root, pool, unit)
    print('native pool migration qualification passed')


if __name__ == '__main__':
    assert os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') == '1', 'Requires an explicitly designated disposable host.'
    assert os.geteuid() == 0, 'Run as root on a disposable host.'
    main(*sys.argv[1:4])
