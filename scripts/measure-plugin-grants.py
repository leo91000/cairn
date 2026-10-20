#!/usr/bin/env python3
"""Reproduce agent-grant query costs with synthetic local SQLite/JSON data.

This compares the R39 all-record projection with bounded R40 SQL selection. It
measures the query/JSON boundary in Python, not Rust HTTP latency or production
storage. Cold means a fresh SQLite connection; the OS file cache is not dropped.
"""
import argparse
import collections
import json
import os
import platform
import sqlite3
import statistics
import tempfile
import time
import uuid
from pathlib import Path


KIND = "plugin-resource-grants"
AGENT = str(uuid.UUID(int=1))
OTHER = str(uuid.UUID(int=2))
PAGE_SQL = """SELECT data FROM records WHERE kind=? AND json_extract(data,'$.agentId')=?
              AND id>? ORDER BY id LIMIT 33"""
COUNTS_SQL = """WITH agents AS MATERIALIZED (
                  SELECT json_extract(data,'$.agentId') AS id FROM records WHERE kind=?
                ) SELECT json_object('agentId',id,'count',count(*)) FROM agents GROUP BY id"""


def page(db, bounded):
    if bounded:
        rows = db.execute(PAGE_SQL, (KIND, AGENT, "")).fetchall()
    else:
        rows = db.execute("SELECT data FROM records WHERE kind=? ORDER BY updated_at DESC", (KIND,)).fetchall()
    decoded = [json.loads(row[0]) for row in rows]
    rules = []
    lookups = 0
    for grant in decoded:
        if grant["agentId"] != AGENT:
            continue
        item = {key: value for key, value in grant.items() if key not in ("ownerAccessId", "connectionId")}
        connection = json.loads(db.execute("SELECT data FROM records WHERE kind=? AND id=?", ("plugin-connections", grant["connectionId"])).fetchone()[0])
        resource = json.loads(db.execute("SELECT data FROM records WHERE kind=? AND id=?", ("plugin-resources", grant["resourceId"])).fetchone()[0])
        item.update(connectionId=grant["connectionId"], pluginId=connection["pluginId"], instanceName=connection["name"], collection=resource["collection"], resourceName=resource["name"])
        lookups += 2
        rules.append(item)
    rules.sort(key=lambda rule: rule["id"])
    items = []
    size = 256
    for rule in rules:
        encoded = len(json.dumps(rule, separators=(",", ":")).encode()) + 1
        if len(items) == 32 or size + encoded > 4_194_304:
            break
        size += encoded
        items.append(rule)
    more = len(items) < len(rules)
    return {"items": items, "nextCursor": items[-1]["id"] if more else None}, {
        "decodedRows": len(decoded), "selectedEncodedBytes": sum(len(row[0].encode()) for row in rows),
        "metadataLookups": lookups,
    }


def counts(db, bounded):
    if bounded:
        rows = db.execute(COUNTS_SQL, (KIND,)).fetchall()
        result = {value["agentId"]: value["count"] for row in rows if (value := json.loads(row[0]))["agentId"]}
    else:
        rows = db.execute("SELECT data FROM records WHERE kind=? ORDER BY updated_at DESC", (KIND,)).fetchall()
        result = dict(collections.Counter(json.loads(row[0])["agentId"] for row in rows))
    return result, {"decodedRows": len(rows), "selectedEncodedBytes": sum(len(row[0].encode()) for row in rows)}


def trial(db, operation, bounded):
    wall = time.perf_counter()
    cpu = time.process_time()
    result, details = operation(db, bounded)
    return result, dict(details, elapsedMs=(time.perf_counter() - wall) * 1000, cpuMs=(time.process_time() - cpu) * 1000)


def workload(directory, name, scopes):
    file = Path(directory) / (name + ".db")
    db = sqlite3.connect(file)
    db.executescript("PRAGMA journal_mode=WAL; CREATE TABLE records(id TEXT PRIMARY KEY, kind TEXT NOT NULL, data TEXT NOT NULL, updated_at INTEGER NOT NULL); CREATE INDEX records_kind ON records(kind,updated_at DESC);")
    write_start, cpu_start = time.perf_counter(), time.process_time()
    encoded_bytes = 0
    def put(kind, value):
        nonlocal encoded_bytes
        data = json.dumps(value, separators=(",", ":"))
        encoded_bytes += len(data.encode())
        db.execute("INSERT INTO records VALUES(?,?,?,?)", (value["id"], kind, data, 1))
    instance = str(uuid.UUID(int=3))
    put("plugin-connections", {"id": instance, "pluginId": "example", "name": "Synthetic app"})
    for number in range(8):
        resource = str(uuid.UUID(int=100 + number))
        put("plugin-resources", {"id": resource, "collection": "workspace", "name": "Synthetic workspace"})
        for index in range(64):
            put(KIND, {
                "id": str(uuid.UUID(int=1000 + number * 64 + index)), "connectionId": instance,
                "resourceId": resource, "agentId": AGENT if number % 2 == 0 else OTHER,
                "profile": "read", "operations": [], "deniedOperations": [], "scopes": scopes,
                "ownerAccessId": "synthetic-owner", "revision": str(uuid.UUID(int=4)), "updatedAt": 1,
            })
    db.commit()
    writes = {"records": 521, "encodedBytes": encoded_bytes, "elapsedMs": (time.perf_counter() - write_start) * 1000, "cpuMs": (time.process_time() - cpu_start) * 1000}
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.close()
    results = {"write": writes, "sqliteBytes": file.stat().st_size, "rules": 512, "matchingAgentRules": 256}
    for operation in [page, counts]:
        variants = {}
        equality = []
        for bounded, variant in [(False, "reference"), (True, "bounded")]:
            connection = sqlite3.connect(file)
            result, cold = trial(connection, operation, bounded)
            equality.append(result)
            warm = [trial(connection, operation, bounded)[1] for _ in range(5)]
            connection.close()
            variants[variant] = {"freshConnection": cold, "warmMedianMs": statistics.median(sample["elapsedMs"] for sample in warm), "warmMedianCpuMs": statistics.median(sample["cpuMs"] for sample in warm), "warmTrials": warm}
        assert equality[0] == equality[1], operation.__name__
        results[operation.__name__] = variants
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="cairn-grant-query-") as directory:
        measurements = {"host": {"platform": platform.platform(), "python": platform.python_version(), "sqlite": sqlite3.sqlite_version, "logicalCpus": os.cpu_count()}, "limits": "Local Python SQLite/JSON query boundary; not Rust HTTP, not production latency. Fresh SQLite connections retain the OS file cache. Other qualification jobs may share this host.", "workloads": {}}
        measurements["workloads"]["smallScopes"] = workload(directory, "small", {"channel": ["C111"]})
        scopes = {f"scope{n}": [f"{item:03d}" + "s" * 176 for item in range(100)] for n in range(8)}
        measurements["workloads"]["largeScopes"] = workload(directory, "large", scopes)
    Path(args.output).write_text(json.dumps(measurements, indent=2) + "\n")


if __name__ == "__main__":
    main()
