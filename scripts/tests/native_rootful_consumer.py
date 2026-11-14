"""Owned rootful Docker daemon used only inside an administrator fixture unit.

Its daemon is deliberately killed after creating a container in a systemd scope.
The controller must attribute that surviving consumer from private metadata binds.
No host Docker daemon, image store or socket is used.
"""
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import time

assert os.environ.get('CAIRN_NATIVE_DISPOSABLE_HOST') == '1'
assert os.geteuid() == 0
binary = Path(sys.argv[1]).resolve(strict=True)
probe = Path(sys.argv[2]).resolve(strict=True)
root = Path.cwd()
runtime = Path('/tmp/cairn-rootful-qualification')
runtime.mkdir(mode=0o700)
store = root / 'administrator-docker'
volume = root / 'administrator-volume'
volume.mkdir(exist_ok=True)
if (volume / 'persisted').exists():
    assert (volume / 'persisted').read_bytes() == b'container work retained'
socket = 'unix://' + str(runtime / 'docker.sock')
environment = dict(os.environ, PATH=str(binary) + ':/usr/sbin:/usr/bin:/sbin:/bin')
log = (runtime / 'daemon.log').open('wb')
daemon = subprocess.Popen([str(binary / 'dockerd'), '--data-root=' + str(store),
    '--exec-root=' + str(runtime / 'exec'), '--pidfile=' + str(runtime / 'daemon.pid'),
    '--host=' + socket, '--exec-opt=native.cgroupdriver=systemd', '--storage-driver=overlay2',
    '--bridge=none', '--iptables=false', '--ip6tables=false', '--userland-proxy=false'],
    env=environment, stdout=log, stderr=subprocess.STDOUT)


def docker(*args, data=None):
    return subprocess.run([str(binary / 'docker'), '--host=' + socket, *args],
        env=environment, input=data, capture_output=True, check=True, timeout=60).stdout.decode().strip()


try:
    for _ in range(120):
        assert daemon.poll() is None, 'Private rootful Docker exited before readiness.'
        try:
            docker('info')
            break
        except subprocess.CalledProcessError:
            time.sleep(0.25)
    else:
        raise AssertionError('Private rootful Docker did not become ready.')
    archive = io.BytesIO()
    with tarfile.open(fileobj=archive, mode='w') as stream:
        stream.add(probe, arcname='probe', recursive=False)
    docker('import', '-', 'cairn-owned-consumer:fixture', data=archive.getvalue())
    identity = docker('run', '-d', '--network=none', '-v', str(volume) + ':/volume',
        'cairn-owned-consumer:fixture', '/probe', 'hold', '/volume')
    assert len(identity) == 64 and all(value in '0123456789abcdef' for value in identity)
    pid = int(docker('inspect', '--format', '{{.State.Pid}}', identity))
    for _ in range(100):
        if (volume / 'persisted').exists():
            break
        time.sleep(0.05)
    assert (volume / 'persisted').read_bytes() == b'container work retained'
    # The init PID reported by a daemon in the workload PID namespace differs
    # from its host PID. Resolve it using the unique systemd cgroup identity.
    scope = Path('/sys/fs/cgroup/system.slice/docker-' + identity + '.scope')
    members = scope.joinpath('cgroup.procs').read_text().split()
    assert members, 'Rootful container did not escape into the expected systemd scope.'
    metadata = scope.stat()
    record = dict(containerId=identity, cgroup=str(scope), cgroupDevice=metadata.st_dev,
        cgroupInode=metadata.st_ino, processIds=members, daemonReportedPid=pid)
    # Only this Popen-owned daemon is killed. Containers remain alive until
    # controller stop fences their independently attributed scopes.
    daemon.kill()
    daemon.wait(timeout=10)
    assert 'populated 1' in scope.joinpath('cgroup.events').read_text()
    (root / 'administrator-consumer.json').write_text(json.dumps(record))
finally:
    if daemon.poll() is None:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait(timeout=10)
    log.close()
