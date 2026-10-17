#!/usr/bin/env python3
"""Opt-in real KVM + managed ublk discard, including a deliberately blocked backend.

Uses private disposable fixtures, never existing installation data. Needs root,
Debian Python 3.11, static /bin/busybox, e2fsprogs, KVM and the loaded ublk driver.
The candidate image can supply its exact kernel/control disk and musl Firecracker.
"""
import argparse
import http.client
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import stat
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location("discard_bootstrap", Path(__file__).with_name("storage-discard-bootstrap.py"))
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)


class UnixConnection(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.host)


def api(path, method, resource, body, status):
    client = UnixConnection(str(path), timeout=2)
    started = time.monotonic()
    try:
        client.request(method, resource, json.dumps(body), {"Content-Type": "application/json"})
        response = client.getresponse()
        response.read()
        assert response.status == status, "Unexpected private VMM management response"
    finally:
        client.close()
    elapsed = (time.monotonic() - started) * 1000
    assert elapsed < 1000, "VMM control stalled behind discard"
    return {"method": method, "resource": resource, "elapsedMs": elapsed}


def wait_for(predicate, processes, seconds, description):
    deadline = time.monotonic() + seconds
    while not predicate():
        if any(process.poll() is not None for process in processes):
            raise RuntimeError(f"Private fixture exited during {description}")
        if time.monotonic() >= deadline:
            raise TimeoutError(f"Private fixture timed out during {description}")
        time.sleep(0.05)


def trial(root, data, paired, args):
    folder = root / ("paired" if paired else "flat")
    folder.mkdir(mode=0o700)
    contexts = []
    vmm = None
    vmm_log = None
    cleanup_ok = False
    try:
        drives = [{"drive_id": "root", "path_on_host": str(root / "root.ext4"), "is_root_device": True, "is_read_only": True}]
        for index in range(2 if paired else 1):
            directory = folder / f"disk-{index}"
            directory.mkdir(mode=0o700)
            image = directory / "base.ext4"
            bootstrap.run("truncate", "-s", "64M", image)
            bootstrap.run("/usr/sbin/mkfs.ext4", "-q", "-F", "-d", data, image)
            metrics = directory / "metrics"
            hold = Path(str(metrics) + ".discard-hold")
            hold.touch()
            log = (directory / "probe.log").open("wb")
            state = directory / "state"
            ready = directory / "ready"
            process = subprocess.Popen([str(args.probe), "ublk-managed", str(image), str(directory / "journal"), str(ready), str(metrics), str(state)], stdout=log, stderr=log)
            context = {"process": process, "log": log, "state": state, "metrics": metrics, "hold": hold, "deviceId": None}
            contexts.append(context)
            wait_for(ready.exists, [process], 40, "ublk readiness")
            device = Path(ready.read_text())
            device.relative_to(state)
            records = list((state / "ublk-devices").glob("*.json"))
            assert len(records) == 1
            record = json.loads(records[0].read_text())
            assert record["owner"]["pid"] == process.pid
            context["deviceId"] = record["deviceId"]
            info = device.stat()
            assert stat.S_ISBLK(info.st_mode)
            expected = Path(f'/sys/class/block/ublkb{record["deviceId"]}/dev').read_text().strip()
            assert expected == f"{os.major(info.st_rdev)}:{os.minor(info.st_rdev)}"
            drives.append({"drive_id": f"data{index}", "path_on_host": str(device), "is_root_device": False, "is_read_only": False, "io_engine": "Async", "discard": True})
        drives.append({"drive_id": "control", "path_on_host": str(root / "control.squashfs"), "is_root_device": False, "is_read_only": True})
        control_device = "/dev/vdd" if paired else "/dev/vdc"
        boot = 'console=ttyS0 loglevel=5 i8042.nokbd i8042.noaux reboot=k panic=1 pci=off root=/dev/vda ro init=/bin/sh -- -c "set -e; mount -t devtmpfs devtmpfs /dev || true; '
        boot += f'mount -t squashfs -o ro,nodev,nosuid {control_device} /mnt; exec /mnt/cairn-init"'
        config = {"boot-source": {"kernel_image_path": str(args.kernel), "boot_args": boot}, "drives": drives, "machine-config": {"vcpu_count": 1, "mem_size_mib": 256}}
        configuration = folder / "config.json"
        configuration.write_text(json.dumps(config))
        serial = folder / "serial.log"
        vmm_log = serial.open("wb")
        command = [str(args.firecracker), "--api-sock", str(folder / "api.sock"), "--config-file", str(configuration)]
        if args.no_seccomp:
            command.append("--no-seccomp")
        vmm = subprocess.Popen(command, stdout=vmm_log, stderr=vmm_log)
        processes = [vmm] + [context["process"] for context in contexts]
        entered = lambda: any(Path(str(context["metrics"]) + ".discard-entered").exists() for context in contexts)
        wait_for(entered, processes, 90, "real backend discard")
        # Keep actual kernel discard pending while the management requests run.
        time.sleep(2)
        timings = [api(folder / "api.sock", "GET", "/", {}, 200), api(folder / "api.sock", "PATCH", "/vm", {"state": "Paused"}, 204), api(folder / "api.sock", "PATCH", "/vm", {"state": "Resumed"}, 204)]
        for context in contexts:
            context["hold"].unlink(missing_ok=True)
        wait_for(lambda: "CAIRN_LIVE_DATA_VERIFIED" in serial.read_text(errors="replace"), processes, 90, "retained live files after discard")
        expected_mounts = {"/oldroot/run/data", "/data"} if paired else {"/oldroot/run/data"}
        events = []
        for line in serial.read_text(errors="replace").splitlines():
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if event.get("operation") == "guest_trim" and event.get("completed"):
                events.append(event)
        assert {event["mount"] for event in events} == expected_mounts
        performance = [json.loads(context["metrics"].read_text()) for context in contexts]
        assert all(value["discard"]["count"] > 0 and value["discard"]["errors"] == 0 for value in performance)
        result = {"paired": paired, "liveDataVerified": True, "blockedDiscardControl": timings, "trimEvents": events, "backendPerformance": performance}
    finally:
        for context in contexts:
            context["hold"].unlink(missing_ok=True)
        if vmm is not None:
            if vmm.poll() is None:
                vmm.kill()
            # Never remove a backend while a fixture VMM can still access it.
            vmm.wait(timeout=20)
        if vmm_log is not None:
            vmm_log.close()
        cleanup_errors = []
        for context in contexts:
            Path(str(context["metrics"]) + ".managed-stop").touch()
            try:
                context["process"].wait(timeout=20)
            except subprocess.TimeoutExpired:
                context["process"].kill()
                context["process"].wait(timeout=10)
            context["log"].close()
            try:
                if context["state"].exists():
                    subprocess.run([str(args.probe), "cleanup-managed", str(context["state"])], check=True, timeout=20)
                assert not list((context["state"] / "ublk-devices").glob("*.json")), "Private ownership registry remains; preserve fixture"
                device_id = context["deviceId"]
                if device_id is not None:
                    assert not Path(f"/sys/class/block/ublkb{device_id}").exists(), "Private kernel device remains; preserve fixture"
            except Exception as error:
                # Continue cleaning other owned backends after the VMM is dead.
                cleanup_errors.append(error)
        if cleanup_errors:
            raise RuntimeError("Private backend cleanup incomplete; preserve fixture") from cleanup_errors[0]
        cleanup_ok = True
    assert cleanup_ok
    result["cleanupVerified"] = True
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["firecracker", "probe", "kernel", "output"]:
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--root", type=Path, default=Path("/var/tmp"))
    parser.add_argument("--legacy-init", type=Path)
    parser.add_argument("--control-image", type=Path)
    parser.add_argument("--no-seccomp", action="store_true")
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="cairn-discard-native-", dir=args.root))
    try:
        legacy = args.legacy_init.read_bytes() if args.legacy_init else None
        data = bootstrap.prepare(root, legacy, args.control_image)
        retained = bootstrap.digest(root / "root.ext4")
        results = [trial(root, data, paired, args) for paired in [False, True]]
        assert bootstrap.digest(root / "root.ext4") == retained
        result = {"schema": 1, "kernel": os.uname().release, "firecrackerSha256": bootstrap.digest(args.firecracker), "guestKernelSha256": bootstrap.digest(args.kernel), "controlSha256": bootstrap.digest(root / "control.squashfs"), "seccompEnabled": not args.no_seccomp, "retainedImageUnchanged": True, "trials": results, "limits": "Controlled old-bridge guests, real KVM/native Async/ublk/LazyDisk and blocked backend. No installation files, Volume node-budget policy, S3 or production-network latency."}
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    except BaseException:
        # Failed cleanup evidence must survive; never delete unverified ownership.
        print(f"Private qualification failed; preserve fixture at {root}")
        raise
    else:
        shutil.rmtree(root)
        print(json.dumps(result))


if __name__ == "__main__":
    main()
