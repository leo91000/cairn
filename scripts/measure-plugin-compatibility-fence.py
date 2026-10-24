#!/usr/bin/env python3
"""Measure the SQLite version cookie only; excludes Store/Service startup and network."""
import argparse
import json
import os
import platform
import sqlite3
import statistics
import tempfile
import time
from pathlib import Path


def summary(samples):
    ordered = sorted(samples)
    return {"medianMs": statistics.median(ordered) * 1000,
            "p95Ms": ordered[int((len(ordered) - 1) * .95)] * 1000}


def observe(path, version):
    connection = sqlite3.connect(path)
    connection.execute("PRAGMA journal_mode=WAL")
    connection.execute("PRAGMA synchronous=FULL")
    connection.execute("CREATE TABLE records(id TEXT PRIMARY KEY, kind TEXT NOT NULL, data TEXT NOT NULL, updated_at INTEGER NOT NULL)")
    records = [(str(index), "plugin-resources", json.dumps({
        "id": str(index), "connectionId": "app", "collection": "workspaces",
        "settings": {"workspaceId": "T111", "appId": "A111"},
        "secretKeys": ["token"], "revision": "fixture-revision",
    }, separators=(",", ":")), 1) for index in range(100)]
    connection.executemany("INSERT INTO records VALUES(?,?,?,?)", records)
    connection.execute("PRAGMA user_version=12")
    connection.commit()
    connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    encoded = sum(len(record[2].encode()) for record in records)
    cpu_start = time.process_time()
    writes = []
    for _ in range(100):
        connection.execute("BEGIN IMMEDIATE")
        connection.execute("PRAGMA user_version=12")
        connection.commit()
        start = time.perf_counter()
        connection.execute("BEGIN IMMEDIATE")
        connection.execute(f"PRAGMA user_version={version}")
        connection.commit()
        writes.append(time.perf_counter() - start)
    write_cpu = time.process_time() - cpu_start
    wal_bytes = Path(str(path) + "-wal").stat().st_size
    connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    assert connection.execute("SELECT * FROM records ORDER BY CAST(id AS INTEGER)").fetchall() == records
    warm = []
    cpu_start = time.process_time()
    for _ in range(1000):
        start = time.perf_counter()
        assert connection.execute("PRAGMA user_version").fetchone()[0] == version
        warm.append(time.perf_counter() - start)
    warm_cpu = time.process_time() - cpu_start
    connection.close()
    cold = []
    cpu_start = time.process_time()
    for _ in range(100):
        start = time.perf_counter()
        connection = sqlite3.connect(path)
        assert connection.execute("PRAGMA user_version").fetchone()[0] == version
        connection.close()
        cold.append(time.perf_counter() - start)
    cold_cpu = time.process_time() - cpu_start
    return {"version": version, "encodedRecordBytes": encoded,
            "databaseBytes": path.stat().st_size, "walBytesBeforeCheckpoint": wal_bytes,
            "write": summary(writes), "warmRead": summary(warm),
            "newConnectionRead": summary(cold),
            "cpuSeconds": {"writesAndReset": write_cpu, "warmReads": warm_cpu,
                           "newConnectionReads": cold_cpu}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = {"host": {"system": platform.system(), "machine": platform.machine(),
                       "python": platform.python_version(), "sqlite": sqlite3.sqlite_version,
                       "loadAverage": os.getloadavg()},
              "workload": {"records": 100, "writes": 100, "resetWrites": 100, "warmReads": 1000,
                           "newConnectionReads": 100, "synchronous": "FULL", "journal": "WAL"},
              "limits": "New SQLite connections, not OS-cache eviction; CPU includes reference reset and Python overhead; marker SQL only, excluding Store/Service startup, provider traffic, encryption, S3 and recovery qualification."}
    with tempfile.TemporaryDirectory(prefix="slack-marker-observation-", dir=Path.cwd().parent) as directory:
        root = Path(directory)
        result["reference"] = observe(root / "reference.db", 12)
        result["candidate"] = observe(root / "candidate.db", 13)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
