#!/usr/bin/env python3
"""Real disposable guests: current trim must work with a retained old bridge.

Requires KVM, busybox-static, Python 3.11, e2fsprogs and squashfs-tools on a Debian
host. Never mounts guest filesystems on the host. No installation data is used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

REPOSITORY = Path(__file__).resolve().parents[1]


def run(*args):
    subprocess.run([str(arg) for arg in args], check=True, stdout=subprocess.DEVNULL)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def dependencies(binary, root):
    for line in subprocess.check_output(["ldd", binary], text=True).splitlines():
        for part in line.split():
            source = Path(part)
            if part.startswith("/") and source.is_file():
                target = root / part[1:]
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(source, target)


def prepare(root, legacy_init=None, control_image=None):
    base = root / "rootfs"
    for path in ["bin", "sbin", "dev", "proc", "sys", "run", "tmp", "mnt", "data", "oldroot", "var/lib", "home/node", "etc", "usr/local/bin", "usr/bin", "usr/lib", "lib/x86_64-linux-gnu", "lib64"]:
        (base / path).mkdir(parents=True, exist_ok=True)
    (base / "etc/passwd").write_text("root:x:0:0:root:/root:/bin/sh\nnode:x:1000:1000:node:/home/node:/bin/sh\n")
    (base / "etc/group").write_text("root:x:0:\nnode:x:1000:\n")
    shutil.copy("/bin/busybox", base / "bin/busybox")
    for name in ["sh", "mount", "mkdir", "pivot_root", "chown", "chmod", "ip", "sync", "sleep", "cat", "kill", "umount", "rm"]:
        (base / "bin" / name).symlink_to("busybox")
    (base / "sbin/reboot").symlink_to("../bin/busybox")
    shutil.copy("/usr/bin/python3.11", base / "usr/bin/python3")
    shutil.copytree("/usr/lib/python3.11", base / "usr/lib/python3.11", ignore=shutil.ignore_patterns("__pycache__"))
    dependencies("/usr/bin/python3.11", base)
    # This bridge deliberately knows nothing about TRIM. It models retained code.
    bridge = base / "usr/local/bin/cairn"
    bridge.write_text('''#!/bin/sh
set -eu
rm /oldroot/run/data/gone
if [ -b /dev/vdd ]; then
 mount -t ext4 /dev/vdc /data
 rm /data/gone
fi
sync
echo CAIRN_LEGACY_BRIDGE_RUNNING
sleep 70
[ "$(cat /oldroot/run/data/keep)" = retained-live-file ]
if [ -b /dev/vdd ]; then
 [ "$(cat /data/keep)" = retained-live-file ]
fi
echo CAIRN_LIVE_DATA_VERIFIED
while true; do sleep 1; done
''')
    bridge.chmod(0o755)
    # Historical init is kept intact, without the maintenance startup.
    legacy = legacy_init if legacy_init is not None else subprocess.check_output(["git", "show", "HEAD:deploy/microvm/init"], cwd=REPOSITORY)
    (base / "sbin/cairn-init").write_bytes(legacy)
    (base / "sbin/cairn-init").chmod(0o755)
    run("truncate", "-s", "64M", root / "root.ext4")
    run("/usr/sbin/mkfs.ext4", "-q", "-F", "-d", base, root / "root.ext4")
    control = root / "control"
    control.mkdir()
    shutil.copy(REPOSITORY / "deploy/microvm/init", control / "cairn-init")
    shutil.copy(REPOSITORY / "deploy/microvm/trim.py", control / "trim.py")
    shutil.copy("/bin/busybox", control / "busybox")
    (control / "cairn-init").chmod(0o755)
    if control_image is None:
        run("/usr/bin/mksquashfs", control, root / "control.squashfs", "-comp", "xz", "-processors", "2", "-noappend", "-all-root", "-no-progress")
    else:
        shutil.copy(control_image, root / "control.squashfs")
    data = root / "datafs"
    data.mkdir()
    (data / "keep").write_text("retained-live-file\n")
    (data / "gone").write_bytes(os.urandom(8 * 1024 * 1024))
    return data


def trial(root, data, paired, maintenance, args):
    images = []
    for index in range(2 if paired else 1):
        image = root / f"data-{paired}-{maintenance}-{index}.ext4"
        run("truncate", "-s", "64M", image)
        run("/usr/sbin/mkfs.ext4", "-q", "-F", "-d", data, image)
        images.append(image)
    allocated_before = sum(image.stat().st_blocks * 512 for image in images)
    drives = [{"drive_id": "root", "path_on_host": str(root / "root.ext4"), "is_root_device": True, "is_read_only": True}]
    for index, image in enumerate(images):
        drives.append({"drive_id": f"data{index}", "path_on_host": str(image), "is_root_device": False, "is_read_only": False, "io_engine": "Async", "discard": True})
    # The reference also has a read-only placeholder so device numbers match.
    drives.append({"drive_id": "control", "path_on_host": str(root / "control.squashfs"), "is_root_device": False, "is_read_only": True})
    device = "/dev/vdd" if paired else "/dev/vdc"
    boot = "console=ttyS0 loglevel=5 i8042.nokbd i8042.noaux reboot=k panic=1 pci=off root=/dev/vda ro "
    if maintenance:
        boot += f'init=/bin/sh -- -c "set -e; mount -t devtmpfs devtmpfs /dev || true; mount -t squashfs -o ro,nodev,nosuid {device} /mnt; exec /mnt/cairn-init"'
    else:
        boot += "init=/sbin/cairn-init"
    config = {"boot-source": {"kernel_image_path": str(args.kernel), "boot_args": boot}, "drives": drives, "machine-config": {"vcpu_count": 1, "mem_size_mib": 256}}
    path = root / f"config-{paired}-{maintenance}.json"
    path.write_text(json.dumps(config))
    log_path = root / f"boot-{paired}-{maintenance}.log"
    command = [str(args.firecracker), "--no-api", "--config-file", str(path)]
    if args.no_seccomp:
        command.append("--no-seccomp")
    ready_ms = None
    started = time.monotonic()
    with log_path.open("wb") as log:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = started + 150
            while time.monotonic() < deadline:
                text = log_path.read_text(errors="replace")
                if ready_ms is None and "CAIRN_LEGACY_BRIDGE_RUNNING" in text:
                    ready_ms = round((time.monotonic() - started) * 1000)
                events = []
                for line in text.splitlines():
                    if line.startswith("{"):
                        try:
                            events.append(json.loads(line))
                        except ValueError:
                            pass
                completed = {event.get("mount") for event in events if event.get("operation") == "guest_trim" and event.get("invocationSucceeded")}
                expected = {"/oldroot/run/data", "/data"} if paired else {"/oldroot/run/data"}
                if maintenance and completed == expected and "CAIRN_LIVE_DATA_VERIFIED" in text:
                    break
                if not maintenance and ready_ms is not None:
                    break
                if process.poll() is not None:
                    break
                time.sleep(0.1)
            assert ready_ms is not None, text[-5000:]
            if maintenance:
                assert completed == expected and "CAIRN_LIVE_DATA_VERIFIED" in text, text[-5000:]
            allocated_after = sum(image.stat().st_blocks * 512 for image in images)
            reclaimed = allocated_before - allocated_after
            if maintenance:
                assert reclaimed >= 7 * 1024 * 1024 * len(images), (allocated_before, allocated_after)
            return {"paired": paired, "maintenance": maintenance, "readyMs": ready_ms, "allocatedBefore": allocated_before, "allocatedAfter": allocated_after, "reclaimedBytes": reclaimed, "liveDataVerified": bool(maintenance), "trimEvents": events}
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firecracker", type=Path, required=True)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--no-seccomp", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.firecracker = args.firecracker.resolve(strict=True)
    args.kernel = args.kernel.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="cairn-discard-bootstrap-") as temporary:
        root = Path(temporary)
        data = prepare(root)
        original = digest(root / "root.ext4")
        results = [trial(root, data, paired, maintenance, args) for paired in [False, True] for maintenance in [False, True]]
        assert digest(root / "root.ext4") == original, "Retained guest image was modified"
        report = {
            "schema": 1,
            "kernelSha256": digest(args.kernel),
            "firecrackerSha256": digest(args.firecracker),
            "controlBytes": (root / "control.squashfs").stat().st_size,
            "retainedImageUnchanged": True,
            "trials": results,
            "limits": "Disposable file-backed Async disks; no ublk/LazyDisk/S3. Boot timings are single local observations, not a production latency forecast.",
            "seccompEnabled": not args.no_seccomp,
        }
        args.output.write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report))


if __name__ == "__main__":
    main()
