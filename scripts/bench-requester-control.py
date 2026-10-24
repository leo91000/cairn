#!/usr/bin/env python3
"""Synthetic requester-control and OAuth-event KV overhead, not native latency."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import sqlite3
import statistics
import tempfile
import time
import uuid

UPSERT = "INSERT INTO kv VALUES(?,?,NULL) ON CONFLICT(key) DO UPDATE SET data=excluded.data,expires=excluded.expires"
READ = "SELECT data FROM kv WHERE key=? AND (expires IS NULL OR expires>?)"


def identity(name):
    return str(uuid.uuid5(uuid.NAMESPACE_URL, "requester-control-benchmark/" + name))


def measure(path, candidate, oauth=False):
    chat, run, initial, answer = [identity(name) for name in ["chat", "run", "initial", "answer"]]
    turn = f"plugin-requester-suspended:{chat}:{run}:{initial}"
    marker = f"plugin-requester-suspended:{chat}:answer:{answer}"
    requester = f"plugin-message-requester:{chat}:{initial}"
    actor = json.dumps({"account": identity("account"), "accessId": identity("access")}, separators=(",", ":"))
    rows = [(requester, actor), (turn, "true")]
    if candidate:
        rows.append((marker, json.dumps(turn)))
    if oauth:
        resource, event = identity("resource"), identity("event")
        subject = hashlib.sha256(b"synthetic-person").hexdigest()
        flow = f"plugin-oauth-external-flow:{resource}:personal:{subject}"
        receipt = f"plugin-oauth-event:{event}:{resource}:personal:{subject}"
        pending = "plugin-oauth-pending:" + "0" * 64
        rows = [(flow, json.dumps({"key": pending, "event": event}, separators=(",", ":")))]
        if candidate:
            rows.append((receipt, json.dumps({"key": pending, "scopes": ["messages:read"]}, separators=(",", ":"))))
    db = sqlite3.connect(path)
    db.executescript("PRAGMA journal_mode=WAL; CREATE TABLE kv(key TEXT PRIMARY KEY,data TEXT NOT NULL,expires INTEGER);")
    db.executemany(UPSERT, rows)
    db.commit()
    pragmas = {key: db.execute("PRAGMA " + key).fetchone()[0] for key in ["synchronous", "journal_mode", "page_size"]}
    read_keys = [turn] if not candidate else [requester, turn, marker]
    if oauth:
        read_keys = [flow] if not candidate else [receipt, flow]

    def read(connection):
        for key in read_keys:
            json.loads(connection.execute(READ, (key, 1800000000000)).fetchone()[0])

    def samples(count, action):
        values = []
        cpu = time.process_time_ns()
        for _ in range(count):
            start = time.perf_counter_ns()
            action()
            values.append(time.perf_counter_ns() - start)
        return {"samples": count, "p50_ms": statistics.median(values) / 1e6,
                "p95_ms": sorted(values)[int(count * 0.95)] / 1e6,
                "cpu_total_ms": (time.process_time_ns() - cpu) / 1e6}

    def write():
        db.execute("BEGIN IMMEDIATE")
        if oauth:
            db.executemany(UPSERT, rows)
        else:
            db.execute(UPSERT, (turn, "true"))
            if candidate:
                db.execute(UPSERT, (marker, json.dumps(turn)))
            db.execute("DELETE FROM kv WHERE key=?", (f"plugin-message-requester:{chat}:{answer}",))
        db.commit()

    warm = samples(1000, lambda: read(db))
    writes = samples(100, write)
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.close()

    def first_read():
        connection = sqlite3.connect(path)
        try:
            read(connection)
        finally:
            connection.close()

    return {"rows": len(rows), "encoded_json_bytes": sum(len(value.encode()) for _, value in rows),
            "key_bytes": sum(len(key.encode()) for key, _ in rows),
            "database_bytes": path.stat().st_size, "warm_read": warm,
            "new_connection_read": samples(50, first_read), "write_commit": writes, "pragmas": pragmas}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="requester-control-") as directory:
        result = {"scope": "Python/SQLite KV shape only; excludes Rust, database actor, audit and ordinary chat writes, authentication, provider I/O and network",
                  "reference": "R16 suspension and original requester, no owner-answer marker",
                  "candidate": "Same records plus exact chat/run/initial/input owner-answer marker",
                  "cold_definition": "New SQLite connection; OS caches retained, temporary filesystem",
                  "host": {"kernel": platform.release(), "python": platform.python_version(), "sqlite": sqlite3.sqlite_version,
                           "cpus": os.cpu_count(), "load": os.getloadavg(), "memory": [line for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith(('MemTotal:', 'MemAvailable:'))], "context": args.context},
                  "before": measure(Path(directory) / "before.db", False),
                  "after": measure(Path(directory) / "after.db", True),
                  "oauth_event": {
                      "scope": "External flow index before; same flow plus event receipt after; excludes encrypted pending consent, event/authority validation, authorization lookup and provider calls",
                      "before": measure(Path(directory) / "oauth-before.db", False, True),
                      "after": measure(Path(directory) / "oauth-after.db", True, True),
                  }}
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
