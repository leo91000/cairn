#!/usr/bin/env python3
"""Local SQLite comparison for mission waits; no production data or credentials."""
import json
import os
import platform
import sqlite3
import statistics
import tempfile
import time
from pathlib import Path

ROWS = 10000
SAMPLES = 50
WRITES = 300


def measure(operation, samples):
    wall, cpu = [], []
    for _ in range(samples):
        started, process = time.perf_counter_ns(), time.process_time_ns()
        operation()
        wall.append((time.perf_counter_ns() - started) / 1000000)
        cpu.append((time.process_time_ns() - process) / 1000000)
    return {
        "p50_ms": round(statistics.median(wall), 4),
        "p95_ms": round(sorted(wall)[int(len(wall) * .95) - 1], 4),
        "cpu_mean_ms": round(statistics.mean(cpu), 4),
    }


def compare(directory, waiting):
    filename = directory / ("candidate.db" if waiting else "reference.db")
    db = sqlite3.connect(filename)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=FULL")
    db.execute("CREATE TABLE runs(id TEXT PRIMARY KEY,task_id TEXT,status TEXT,created_at INTEGER,data TEXT)")
    states = "'queued','running','waiting'" if waiting else "'queued','running'"
    db.execute(f"CREATE UNIQUE INDEX runs_active_task ON runs(task_id) WHERE status IN ({states})")
    db.execute("CREATE INDEX runs_status ON runs(status,created_at)")
    documents = []
    for index in range(ROWS):
        active = index % 10 == 0
        status = ("waiting" if waiting else "queued") if active else "succeeded"
        run = {"id": f"run-{index}", "taskId": f"task-{index}", "status": status,
               "createdAt": index, "summary": "Synthetic mission", "snapshot": {"agent": {"id": "fixture"}}}
        if waiting and active:
            run["wakeup"] = {"id": "00000000-0000-4000-8000-000000000001", "messageId": None,
                             "reason": "Wait for mission CI", "seconds": 600,
                             "createdAt": 1000, "wakeAt": 601000,
                             "timeCondition": {"type": "delay", "seconds": 600}}
        encoded = json.dumps(run, separators=(",", ":"))
        documents.append(encoded)
        db.execute("INSERT INTO runs VALUES(?,?,?,?,?)", (run["id"], run["taskId"], status, index, encoded))
    db.commit()
    query = f"SELECT data FROM runs WHERE status IN ({states}) ORDER BY created_at"

    def read(connection):
        values = [json.loads(row[0]) for row in connection.execute(query)]
        assert len(values) == ROWS // 10

    def cold():
        # Cold SQLite pager and statements; the OS page cache remains warm.
        with sqlite3.connect(filename) as connection:
            read(connection)

    writes = 0

    def write():
        nonlocal writes
        writes += 1
        value = json.loads(documents[0])
        value["measurementWrite"] = writes
        encoded = json.dumps(value, separators=(",", ":"))
        db.execute("UPDATE runs SET data=? WHERE id='run-0'", (encoded,))
        db.commit()

    result = {"active_document_bytes": len(documents[0].encode()),
              "encoded_total_bytes": sum(len(document.encode()) for document in documents),
              "cold_pager_read": measure(cold, SAMPLES),
              "warm_read": measure(lambda: read(db), SAMPLES),
              "write": measure(write, WRITES)}
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    result["database_bytes"] = filename.stat().st_size
    db.close()
    return result


with tempfile.TemporaryDirectory(prefix="cairn-mission-wait-measure-") as temporary:
    directory = Path(temporary)
    print(json.dumps({"host": {"system": platform.platform(), "sqlite": sqlite3.sqlite_version,
                               "cpus_visible": os.cpu_count()},
                      "workload": {"runs": ROWS, "active": ROWS // 10, "read_samples": SAMPLES,
                                   "write_samples": WRITES, "journal": "WAL", "synchronous": "FULL",
                                   "cold": "new connection; cold SQLite pager, warm OS cache",
                                   "network": "none; local overlay filesystem"},
                      "reference": compare(directory, False), "candidate": compare(directory, True)}, indent=2))
