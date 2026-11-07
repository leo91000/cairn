#!/usr/bin/env python3
"""Guest-only discard maintenance, including retained images with an old bridge.

Loaded from an immutable, read-only node control disk. No remote access, guest
content parsing, allocation bitmap inference, shell commands or persisted state.
"""

import json
import os
import signal
import subprocess
import sys
import time

TARGETS = ("/oldroot/run/data",)
BUSYBOX = "/oldroot/mnt/busybox"
INITIAL_DELAY = 10
INTERVAL = 1800
TIMEOUT = 120
ERROR_RETRY = 60


def targets(mounts):
    present = set()
    for line in mounts.splitlines():
        fields, separator, filesystem = line.partition(" - ")
        fields = fields.split()
        filesystem = filesystem.split()
        if separator and len(fields) >= 6 and filesystem and filesystem[0] == "ext4":
            present.add(fields[4])
    return [target for target in TARGETS if target in present]


def ready_targets(mounted, due, now):
    for target in tuple(due):
        if target not in mounted:
            del due[target]
    return [target for target in mounted if now >= due.get(target, 0)]


def sleep(seconds, stopped):
    deadline = time.monotonic() + seconds
    while not stopped():
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return
        time.sleep(min(1, remaining))


def terminate(process):
    """Own the child process group until it is reaped, including on timeout."""
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()


def trim(target, stopped):
    if target not in TARGETS:
        raise ValueError("Unexpected trim target")
    started = time.monotonic()
    invocation_succeeded = False
    process = None
    try:
        # A separate session lets cancellation kill only our maintenance child.
        process = subprocess.Popen(
            [BUSYBOX, "fstrim", "-m", "65536", target],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        deadline = started + TIMEOUT
        while process.poll() is None and not stopped() and time.monotonic() < deadline:
            time.sleep(0.25)
        # FITRIM can hide backend I/O errors; this records only tool success.
        # Backend discard counters, not this flag, certify durable zeroing.
        invocation_succeeded = process.poll() == 0 and not stopped()
    except OSError:
        pass
    finally:
        if process is not None:
            terminate(process)
    # Fixed paths and aggregate timings only; no content or child diagnostics.
    return {
        "operation": "guest_trim",
        "mount": target,
        "invocationSucceeded": invocation_succeeded,
        "elapsedMs": round((time.monotonic() - started) * 1000),
    }


def main():
    if os.geteuid() != 0 or sys.argv[1:]:
        raise SystemExit("Guest trim requires root and accepts no arguments")
    with open("/proc/self/mountinfo", encoding="utf-8") as stream:
        if TARGETS[0] not in targets(stream.read()) or not os.access(BUSYBOX, os.X_OK):
            raise SystemExit("Guest trim requires the mounted guest and control disk")
    descriptor = os.open("/run/cairn-trim.ready", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    os.close(descriptor)
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    stopped = lambda: stopping
    os.nice(10)
    due = {}
    sleep(INITIAL_DELAY, stopped)
    while not stopping:
        try:
            with open("/proc/self/mountinfo", encoding="utf-8") as stream:
                mounted = targets(stream.read())
        except OSError:
            mounted = []
        # Never hold the bridge's filesystem lock: FITRIM may wait for a frozen
        # mount. The old or new bridge must remain able to unfreeze it.
        for target in ready_targets(mounted, due, time.monotonic()):
            if stopping:
                break
            result = trim(target, stopped)
            print(json.dumps(result), flush=True)
            delay = INTERVAL if result["invocationSucceeded"] else ERROR_RETRY
            due[target] = time.monotonic() + delay
        sleep(1, stopped)


if __name__ == "__main__":
    main()
