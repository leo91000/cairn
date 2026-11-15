"""Real Docker network/cgroup qualification, on an explicitly disposable host.

No ublk is used here. The fixture has private mount/network namespaces and
bounded owned systemd units; native-runtime separately qualifies ext4/ublk.
"""

import argparse
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import uuid


def bind_directory(descriptor, target):
    # util-linux otherwise resolves /proc/self/fd back to its old pathname,
    # which may now refer to the empty tmpfs hiding the retained directory.
    subprocess.run(
        ["mount", "--no-canonicalize", "--bind", "/proc/self/fd/" + str(descriptor), target],
        pass_fds=(descriptor,),
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    retained, restored = os.fstat(descriptor), os.stat(target)
    if (retained.st_dev, retained.st_ino) != (restored.st_dev, restored.st_ino):
        raise AssertionError("Retained directory was not restored at " + str(target))


def live_thread_roots(pid):
    # The leader's root may be unavailable, or its mount namespace different
    # from another thread's. Inspect every observable live thread's view.
    roots = []
    for thread in Path("/proc", str(pid), "task").iterdir():
        root = thread / "root"
        try:
            root.stat()
        except FileNotFoundError:
            continue
        roots.append(root)
    if not roots:
        raise AssertionError("Managed Docker has no observable live thread roots: " + str(pid))
    return roots


def inside(arguments):
    attempt, binary, archive, go, tools = arguments
    assert Path("/proc/self/ns/mnt").stat().st_ino != Path("/proc/1/ns/mnt").stat().st_ino
    assert Path("/proc/self/ns/net").stat().st_ino != Path("/proc/1/ns/net").stat().st_ino
    archive_bytes = Path(archive).read_bytes()
    ca = next(
        path.read_bytes()
        for path in [Path("/etc/ssl/cert.pem"), Path("/etc/ssl/certs/ca-certificates.crt")]
        if path.exists()
    )
    docker = os.open(tools, os.O_RDONLY | os.O_DIRECTORY)
    # Debian's iptables executables traverse /etc/alternatives. Retain this
    # tool-only directory before replacing /etc with the synthetic fixture.
    alternatives = (
        os.open("/etc/alternatives", os.O_RDONLY | os.O_DIRECTORY)
        if Path("/etc/alternatives").is_dir()
        else None
    )

    def command(*args, **kw):
        return subprocess.run(
            args, check=True, capture_output=True, text=True, timeout=30, **kw
        ).stdout.strip()

    def mount(target):
        command("mount", "-t", "tmpfs", "-o", "size=256m,mode=755", "tmpfs", target)

    cgroups = os.open("/sys/fs/cgroup", os.O_RDONLY | os.O_DIRECTORY)
    for target in ["/run", "/tmp", "/opt", "/etc", "/var/lib"]:
        mount(target)
    command("mount", "-t", "sysfs", "sysfs", "/sys")
    mount("/sys/fs")
    Path("/sys/fs/cgroup").mkdir()
    bind_directory(cgroups, "/sys/fs/cgroup")
    os.close(cgroups)
    Path("/opt/cairn-docker").mkdir()
    bind_directory(docker, "/opt/cairn-docker")
    os.close(docker)
    if alternatives is not None:
        Path("/etc/alternatives").mkdir()
        bind_directory(alternatives, "/etc/alternatives")
        os.close(alternatives)
    for directory in ["/run/cairn-docker", "/run/docker/plugins"]:
        Path(directory).mkdir(parents=True)
    Path("/etc/passwd").write_text("root:x:0:0:root:/run:/bin/sh\n")
    Path("/etc/group").write_text("root:x:0:\n")
    Path("/etc/ssl/certs").mkdir(parents=True)
    Path("/etc/ssl/cert.pem").write_bytes(ca)
    Path("/etc/ssl/certs/ca-certificates.crt").write_bytes(ca)
    Path("/etc/hosts").write_text("127.0.0.1 localhost\n")
    resolver = Path("/run/systemd/resolve/stub-resolv.conf")
    resolver.parent.mkdir(parents=True)
    resolver.write_text("nameserver 127.0.0.53\n")
    Path("/etc/resolv.conf").symlink_to(resolver)
    resolver_before = resolver.read_bytes()
    Path("/run/cairn-plan.json").write_text(
        json.dumps(
            dict(
                id=attempt, runId=attempt, nativeMode=dict(mode="administrator"), nativeDockerGid=0
            )
        )
    )
    Path("/run/cairn-docker/daemon.json").write_text('{"storage-driver":"vfs"}')
    Path("/run/rootfs.tar").write_bytes(archive_bytes)
    # Model another run already mounted when this daemon starts. Only this
    # synthetic /var/lib tree is shared, never the real host mount tree.
    command("mount", "--make-shared", "/var/lib")
    foreign = Path("/var/lib/foreign-run")
    foreign.mkdir()
    mount(str(foreign))
    (foreign / "mounted").touch()
    command("ip", "link", "set", "lo", "up")
    firewall = command("iptables-save")
    links = command("ip", "-json", "link", "show")
    log = Path("/tmp/supervisor.log").open("w")
    helper = subprocess.Popen([binary, "native-administrator-docker"], stdout=log, stderr=log)
    try:
        parent = command(binary, "native-administrator-docker-ready")
        assert resolver.read_bytes() == resolver_before, "Docker changed the outer resolver"
        assert (foreign / "mounted").exists(), "A managed helper unmounted the parent's filesystem"
        daemon_pid = int(Path("/run/cairn-docker/dockerd.pid").read_text())
        relative_marker = foreign.relative_to("/") / "mounted"
        roots = live_thread_roots(daemon_pid)
        if not any((root / relative_marker).exists() for root in roots):
            views = []
            for root in roots[:16]:
                try:
                    with (root.parent / "mountinfo").open() as mounts:
                        matches = [line.strip() for line in mounts if str(foreign) in line][:4]
                    views.append(
                        dict(thread=root.parent.name, root=os.readlink(root), mounts=matches)
                    )
                except FileNotFoundError:
                    continue
            raise AssertionError(
                "Foreign mount was not inherited before the test: "
                + json.dumps(
                    dict(pid=daemon_pid, outerMarker=(foreign / "mounted").exists(), views=views)
                )
            )
        command("umount", str(foreign))
        assert not any(
            (root / relative_marker).exists() for root in live_thread_roots(daemon_pid)
        ), "Docker retained another run's unmounted filesystem"
        client = "/opt/cairn-docker/bin/docker"
        env = dict(
            PATH="/usr/sbin:/usr/bin:/sbin:/bin",
            HOME="/run/cairn-client",
            DOCKER_HOST="unix:///run/cairn-docker/docker.sock",
        )
        command(client, "import", "/run/rootfs.tar", "cairn-network-probe", env=env)
        reserve = socket.socket()
        reserve.bind(("127.0.0.1", 0))
        port = reserve.getsockname()[1]
        reserve.close()
        container = command(
            client,
            "run",
            "-d",
            "-p",
            f"127.0.0.1:{port}:26379",
            "cairn-network-probe",
            "/probe",
            env=env,
        )
        info = json.loads(command(client, "inspect", container, env=env))[0]
        membership = Path("/proc", str(info["State"]["Pid"]), "cgroup").read_text().strip()
        assert membership.startswith("0::" + parent + "/"), membership
        assert urllib.request.urlopen(f"http://127.0.0.1:{port}", timeout=5).read() == b"managed"
        assert firewall == command("iptables-save"), "Docker changed outer firewall"
        assert json.loads(links) == json.loads(
            command("ip", "-json", "link", "show")
        ), "Docker changed outer interfaces"
        print("READY " + membership, flush=True)
        deadline = time.monotonic() + 20
        while not Path(go).exists():
            assert time.monotonic() < deadline
            time.sleep(0.02)
        assert urllib.request.urlopen(f"http://127.0.0.1:{port}", timeout=5).read() == b"managed"
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
            reservation.bind(("127.0.0.1", 0))
            udp_port = reservation.getsockname()[1]
        command(
            client,
            "run",
            "-d",
            "-p",
            f"0.0.0.0:{udp_port}:26380/udp",
            "cairn-network-probe",
            "/probe",
            "udp",
            env=env,
        )
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as packet:
            packet.settimeout(3)
            packet.sendto(b"owned namespace", ("127.0.0.1", udp_port))
            assert packet.recv(64) == b"owned namespace"
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            once_port = reservation.getsockname()[1]
        once = command(
            client,
            "run",
            "-d",
            "-p",
            f"127.0.0.1:{once_port}:26379",
            "cairn-network-probe",
            "/probe",
            "once",
            env=env,
        )
        assert (
            urllib.request.urlopen(f"http://127.0.0.1:{once_port}", timeout=5).read() == b"managed"
        )
        deadline = time.monotonic() + 5
        while json.loads(command(client, "inspect", once, env=env))[0]["State"]["Running"]:
            assert time.monotonic() < deadline
            time.sleep(0.02)
        while True:
            try:
                with socket.socket() as available:
                    available.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    available.bind(("127.0.0.1", once_port))
                break
            except OSError:
                assert (
                    time.monotonic() < deadline
                ), "Naturally exited container retained its host port."
                time.sleep(0.05)
        command(binary, "native-docker-stop")
        info = json.loads(command(client, "inspect", container, env=env))[0]
        assert not info["State"]["Running"] and info["State"]["ExitCode"] == 0, info["State"]
        reserve = socket.socket()
        reserve.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        reserve.bind(("127.0.0.1", port))
        reserve.close()
        print(
            "PASS cgroup ancestry, TCP/UDP publication, private firewall/interfaces, foreign unmount propagation, freeze/thaw, natural exit, graceful stop and port release",
            flush=True,
        )
    except BaseException:
        for path in ["/tmp/supervisor.log", "/tmp/cairn-dockerd.log", "/tmp/cairn-slirp.log"]:
            if Path(path).exists():
                print(path, Path(path).read_text()[-6000:], file=sys.stderr)
        raise


def unit_command(unit, attempt, binary, archive, go, tools):
    return [
        "systemd-run",
        "--quiet",
        "--collect",
        "--wait",
        "--pipe",
        "--unit=" + unit,
        "--setenv=PATH=/usr/sbin:/usr/bin:/sbin:/bin",
        "--property=Delegate=yes",
        "--property=DelegateSubgroup=runner",
        "--property=KillMode=control-group",
        "--property=TimeoutStopSec=10s",
        "--property=RuntimeMaxSec=90s",
        "--property=MemoryMax=1G",
        "--property=CPUQuota=100%",
        "/usr/bin/unshare",
        "--mount",
        "--net",
        "--propagation",
        "private",
        sys.executable,
        str(Path(__file__).resolve()),
        "--inside",
        attempt,
        str(binary),
        str(archive),
        str(go),
        str(tools),
    ]


def main():
    if sys.argv[1:2] == ["--inside"]:
        inside(sys.argv[2:])
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("runtime", type=Path)
    parser.add_argument("--docker-tools", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("CAIRN_NATIVE_DISPOSABLE_HOST") != "1" or os.geteuid() != 0:
        raise RuntimeError("Requires root on an explicitly designated disposable host.")
    if any(
        "/native-block/" in line and "/root/disk.blk" in line
        for line in Path("/proc/1/mountinfo").read_text().splitlines()
    ):
        raise RuntimeError(
            "Run before starting native disks; fixture namespaces must not retain live run mounts."
        )
    binary = args.runtime.resolve(strict=True)
    tools = args.docker_tools.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="native-network-", dir=binary.parent) as temporary:
        root = Path(temporary)
        subprocess.run(
            [
                "cc",
                "-static",
                "-O2",
                "-Wall",
                "-Wextra",
                "-Werror",
                str(Path(__file__).with_name("native_administrator_network_probe.c")),
                "-o",
                str(root / "probe"),
            ],
            check=True,
        )
        with tarfile.open(root / "rootfs.tar", "w") as archive:

            def normalize(info):
                info.uid = info.gid = 0
                info.uname = info.gname = "root"
                return info

            archive.add(root / "probe", arcname="probe", filter=normalize)
        attempt = str(uuid.uuid4())
        unit = "cairn-native-" + attempt + ".service"
        go = root / "go"
        command = unit_command(unit, attempt, binary, root / "rootfs.tar", go, tools)
        process = subprocess.Popen(
            command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
        )
        try:
            while True:
                if not select.select([process.stdout], [], [], 100)[0]:
                    raise AssertionError("Network qualification produced no result.")
                line = process.stdout.readline()
                if not line:
                    break
                print(line, end="", flush=True)
                if line.startswith("READY "):
                    subprocess.run(["systemctl", "freeze", unit], check=True)
                    frozen = subprocess.check_output(
                        ["systemctl", "show", unit, "--property=FreezerState", "--value"], text=True
                    ).strip()
                    if frozen != "frozen":
                        raise AssertionError("Fixture freeze was not confirmed: " + frozen)
                    subprocess.run(["systemctl", "thaw", unit], check=True)
                    go.touch()
            result = process.wait(timeout=15)
            if result:
                raise AssertionError("Network qualification failed: " + str(result))
        finally:
            subprocess.run(["systemctl", "thaw", unit], capture_output=True)
            stopped = subprocess.run(["systemctl", "stop", unit], capture_output=True, timeout=20)
            if process.poll() is None:
                process.wait(timeout=15)
            if stopped.returncode and b"not loaded" not in stopped.stderr:
                raise AssertionError("Fixture systemd cleanup is unconfirmed.")


if __name__ == "__main__":
    main()
