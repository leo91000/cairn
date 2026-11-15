"""Managed administrator Docker lifecycle on the disposable native host."""

import json
from pathlib import Path
import subprocess
import time
import sys
import socket
import urllib.request


def command(*args, succeeds=True):
    result = subprocess.run(args, capture_output=True, text=True, timeout=120)
    assert (result.returncode == 0) == succeeds, (args, result.returncode, result.stderr[-4096:])
    return result.stdout.strip()


def main():
    assert command("docker", "compose", "version")
    assert command("docker", "buildx", "version")
    if Path("administrator-docker.json").exists():
        assert (
            Path("docker-stopped").read_text().strip() == "clean"
        ), "Prior container missed graceful shutdown."
    Path("docker-stopped").unlink(missing_ok=True)
    Path("docker-writes").unlink(missing_ok=True)
    assert command("docker", "info", "--format", "{{.CgroupDriver}}") == "cgroupfs"
    assert command("docker", "ps", "-q") == "", "A retained service restarted implicitly."
    image = "alpine:3.22.2"
    command("docker", "pull", image)
    raw_uid, raw_gid = str(int(sys.argv[1])), str(int(sys.argv[2]))
    command(
        "docker",
        "run",
        "--rm",
        "-v",
        "cairn-raw-owners:/raw",
        image,
        "sh",
        "-c",
        'if test -f /raw/owner; then test "$(stat -c %u:%g /raw/owner)" = "$1:$2"; '
        'else touch /raw/owner; chown "$1:$2" /raw/owner; fi',
        "sh",
        raw_uid,
        raw_gid,
    )
    context = Path("administrator-build")
    context.mkdir(exist_ok=True)
    (context / "Dockerfile").write_text("FROM alpine:3.22.2\nRUN echo built >/hello\n")
    command("docker", "--debug", "build", "-t", "cairn-administrator-fixture", str(context))
    assert (
        command("docker", "run", "--rm", "cairn-administrator-fixture", "cat", "/hello") == "built"
    )
    for policy in ["always", "unless-stopped", "on-failure:3"]:
        command("docker", "run", "-d", "--restart", policy, image, "sleep", "300", succeeds=False)
    command(
        "docker",
        "run",
        "-d",
        "--cgroup-parent=/system.slice",
        image,
        "sleep",
        "300",
        succeeds=False,
    )
    previous = command("docker", "ps", "-aq", "--filter=name=^/cairn-held$").splitlines()
    if previous:
        command("docker", "rm", "-f", *previous)
    container = command(
        "docker",
        "run",
        "-d",
        "--name=cairn-held",
        "-v",
        str(Path.cwd()) + ":/held",
        image,
        "sh",
        "-c",
        "trap 'echo clean > /held/docker-stopped; exit 0' TERM; while :; do echo x >> /held/docker-writes; sleep 0.1; done",
    )
    deadline = time.monotonic() + 10
    while not Path("docker-writes").exists():
        assert time.monotonic() < deadline, "Container did not begin writing."
        time.sleep(0.05)
    command("docker", "update", "--restart=always", container, succeeds=False)
    info = json.loads(command("docker", "inspect", container))[0]
    response = "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nmanaged"
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        host_port = reservation.getsockname()[1]
    http = command(
        "docker",
        "run",
        "-d",
        "-p",
        "127.0.0.1:" + str(host_port) + ":80",
        image,
        "sh",
        "-c",
        "while :; do printf '%s' '" + response + "' | nc -l -p 80; done",
    )
    port = json.loads(command("docker", "inspect", http))[0]["NetworkSettings"]["Ports"]["80/tcp"][
        0
    ]["HostPort"]
    deadline = time.monotonic() + 10
    while True:
        try:
            assert (
                urllib.request.urlopen("http://127.0.0.1:" + port, timeout=1).read() == b"managed"
            )
            break
        except OSError:
            assert time.monotonic() < deadline
            time.sleep(0.05)
    Path("administrator-docker.json").write_text(
        json.dumps(dict(id=container, pid=info["State"]["Pid"], port=port))
    )


if __name__ == "__main__":
    main()
