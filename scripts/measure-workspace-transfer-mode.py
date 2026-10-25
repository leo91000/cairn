#!/usr/bin/env python3
"""Observe the POSIX permission step only, excluding workspace wire/Tokio/network."""
import argparse
import json
import os
import platform
import stat
import statistics
import tempfile
import time
from pathlib import Path


def summary(samples):
    values = sorted(samples)
    return {"medianMs": statistics.median(values) * 1000,
            "p95Ms": values[int((len(values) - 1) * .95)] * 1000}


def observe(root, restore):
    root.mkdir()
    payload = b"workspace-mode-fixture\n" * 200
    paths = []
    writes = []
    cpu_start = time.process_time()
    for index in range(100):
        path = root / str(index)
        start = time.perf_counter()
        fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o755)
        try:
            assert os.write(fd, payload) == len(payload)
            if restore:
                os.fchmod(fd, 0o755)
            os.fsync(fd)
        finally:
            os.close(fd)
        writes.append(time.perf_counter() - start)
        paths.append(path)
    write_cpu = time.process_time() - cpu_start
    new_reads = []
    cpu_start = time.process_time()
    for path in paths:
        start = time.perf_counter()
        assert path.read_bytes() == payload
        new_reads.append(time.perf_counter() - start)
    new_cpu = time.process_time() - cpu_start
    warm = []
    cpu_start = time.process_time()
    with paths[0].open("rb", buffering=0) as file:
        for _ in range(1000):
            start = time.perf_counter()
            file.seek(0)
            assert file.read() == payload
            warm.append(time.perf_counter() - start)
    warm_cpu = time.process_time() - cpu_start
    modes = sorted({oct(stat.S_IMODE(path.stat().st_mode)) for path in paths})
    assert modes == (["0o755"] if restore else ["0o700"])
    return {"filePayloadBytes": len(payload), "totalFileBytes": sum(p.stat().st_size for p in paths),
            "modes": modes, "write": summary(writes), "newDescriptorRead": summary(new_reads),
            "warmRead": summary(warm), "cpuSeconds": {"writes": write_cpu,
            "newDescriptorReads": new_cpu, "warmReads": warm_cpu}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = {"host": {"system": platform.system(), "machine": platform.machine(),
                       "python": platform.python_version(), "loadAverage": os.getloadavg()},
              "workload": {"files": 100, "announcedMode": "0755", "umask": "0077", "warmReads": 1000,
                           "synchronous": "fsync", "newDescriptorReads": 100},
              "limits": "Python POSIX observation only, not Rust/Tokio or workspace protocol; private local fixture, OS cache uncontrolled, reference before candidate, CPU includes Python overhead; no network or native-host trust qualification."}
    previous = os.umask(0o077)
    try:
        with tempfile.TemporaryDirectory(prefix="workspace-mode-observation-", dir=Path.cwd().parent) as directory:
            root = Path(directory)
            result["reference"] = observe(root / "reference", False)
            result["candidate"] = observe(root / "candidate", True)
    finally:
        os.umask(previous)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
