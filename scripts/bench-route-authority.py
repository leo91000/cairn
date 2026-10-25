#!/usr/bin/env python3
"""Measure JSON/SQLite record overhead, not Cairn/Rust or network latency.

The fixture uses Store's records/lookup/upsert shapes and synthetic identities.
No credentials, installation files or provider services are read. Cold means a
new SQLite connection; OS caches are left intact. Run on a quiet host for useful
latency comparisons; the output records the observed host conditions.
"""

import argparse
import copy
import json
import os
import pathlib
import platform
import sqlite3
import statistics
import tempfile
import time
import uuid


def identity(name):
    return str(uuid.uuid5(uuid.NAMESPACE_URL, "cairn-route-benchmark/" + name))


def encode(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


def sample(times):
    ordered = sorted(times)
    return {
        "samples": len(times),
        "p50_ms": statistics.median(times) / 1e6,
        "p95_ms": ordered[min(len(times) - 1, int(len(times) * 0.95))] / 1e6,
    }


def records(generations):
    result = []
    people = ["U%06d" % i for i in range(100)]
    for i in range(256):
        route = {
            "id": identity("route/%d" % i),
            "connectionId": identity("instance"),
            "resourceId": identity("resource"),
            "name": "Support %d" % i,
            "enabled": True,
            "match": {"channel": "C%06d" % i},
            "agentId": identity("agent"),
            "projectId": None,
            "grantedAccess": {
                "projects": [], "skills": [], "mcps": [], "mcpTools": {},
                "installation": False, "github": False, "sandbox": "read-only",
                "nodes": ["local"],
            },
            "ownerAccessId": identity("owner-access"),
            "accessMode": "requests",
            "userIds": people,
            "people": [{"id": person, "name": "Person " + person} for person in people],
            "maxMessagesPerHour": 20,
            "maxPerPersonPerHour": 5,
            "revision": identity("route-revision/%d" % i),
            "digest": "0" * 64,
            "updatedAt": 1800000000000,
        }
        if generations:
            route["personRevisions"] = {
                person: identity("grant/%d/%s" % (i, person)) for person in people
            }
        result.append(("plugin-routes", route))
    for i in range(70):
        binding = {
            "id": identity("route/0"), "revision": identity("route-revision/0"),
            "resource": identity("resource"),
            "resources": [[identity("resource"), identity("resource-revision")]],
            "digest": "0" * 64, "context": {"channel": "C000000"},
        }
        if generations:
            binding["person"] = {"id": people[0], "revision": identity("grant/0/" + people[0])}
        result.append(("plugin-events" if i < 50 else "plugin-deliveries", {
            "id": identity("work/%d" % i), "connectionId": identity("instance"),
            "revision": identity("connection-revision"), "routing": binding,
            "payload": {"text": "Synthetic admitted message"},
            "createdAt": 1800000000000,
        }))
    return result


UPSERT = ("INSERT INTO records VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE "
          "SET data=excluded.data,updated_at=excluded.updated_at")


def measure(path, values):
    db = sqlite3.connect(path)
    db.executescript("""
        PRAGMA journal_mode=WAL;
        PRAGMA foreign_keys=ON;
        CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
        CREATE INDEX records_kind ON records(kind,updated_at DESC);
        CREATE TABLE audit(id INTEGER PRIMARY KEY AUTOINCREMENT,created_at INTEGER NOT NULL,action TEXT NOT NULL,detail TEXT NOT NULL);
    """)
    encoded = [(kind, value, encode(value)) for kind, value in values]
    db.executemany(UPSERT, [(value["id"], kind, raw, 1800000000000) for kind, value, raw in encoded])
    db.commit()
    pragmas = {name: db.execute("PRAGMA " + name).fetchone()[0]
               for name in ["page_size", "synchronous", "journal_mode"]}
    warm = []
    cpu = time.process_time_ns()
    for i in range(1000):
        before = time.perf_counter_ns()
        json.loads(db.execute("SELECT data FROM records WHERE kind=? AND id=?",
                              ("plugin-routes", values[i % 256][1]["id"])).fetchone()[0])
        warm.append(time.perf_counter_ns() - before)
    warm_cpu = time.process_time_ns() - cpu
    writes = []
    updates = []
    for i in range(100):
        value = copy.deepcopy(values[i % 256][1])
        value["updatedAt"] += i + 1
        updates.append(value)
    cpu = time.process_time_ns()
    for value in updates:
        before = time.perf_counter_ns()
        db.execute("BEGIN IMMEDIATE")
        db.execute(UPSERT, (value["id"], "plugin-routes", encode(value), value["updatedAt"]))
        db.execute("INSERT INTO audit(created_at,action,detail) VALUES(?,?,?)",
                   (value["updatedAt"], "plugin.route.people", encode({"id": value["id"], "action": "approve"})))
        db.commit()
        writes.append(time.perf_counter_ns() - before)
    write_cpu = time.process_time_ns() - cpu
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.close()
    cold = []
    cpu = time.process_time_ns()
    for i in range(50):
        before = time.perf_counter_ns()
        with sqlite3.connect(path) as reader:
            json.loads(reader.execute("SELECT data FROM records WHERE kind=? AND id=?",
                                      ("plugin-routes", values[i][1]["id"])).fetchone()[0])
        reader.close()
        cold.append(time.perf_counter_ns() - before)
    cold_cpu = time.process_time_ns() - cpu
    return {
        "encoded_bytes": sum(len(raw.encode()) for _, _, raw in encoded),
        "route_bytes": len(encoded[0][2].encode()),
        "event_bytes": len(encoded[256][2].encode()),
        "database_bytes_after_checkpoint": path.stat().st_size,
        "new_connection_read": {**sample(cold), "cpu_ms": cold_cpu / 1e6},
        "warm_read": {**sample(warm), "cpu_ms": warm_cpu / 1e6},
        "write_and_audit_commit": {**sample(writes), "cpu_ms": write_cpu / 1e6},
        "pragmas": pragmas,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", default="Unspecified concurrent host work")
    args = parser.parse_args()
    memory = {}
    meminfo = pathlib.Path("/proc/meminfo")
    if meminfo.exists():
        memory = {line.split(":")[0]: line.split(":")[1].strip()
                  for line in meminfo.read_text().splitlines()
                  if line.startswith(("MemTotal:", "MemAvailable:"))}
    result = {
        "workload": "256 routes, 100 listed grants/route, 50 events and 20 deliveries; synthetic data",
        "scope": "Python JSON/SQLite shape microbenchmark, not native Cairn/Rust or production network",
        "cold_definition": "new connection, OS caches retained",
        "concurrent_work": args.context,
        "host": {"kernel": platform.release(), "python": platform.python_version(),
                 "sqlite": sqlite3.sqlite_version, "load_average": os.getloadavg(),
                 "memory": memory},
    }
    with tempfile.TemporaryDirectory(prefix="cairn-route-benchmark-") as directory:
        for name, generations in [("reference_without_person_generations", False),
                                  ("candidate_with_person_generations", True)]:
            result[name] = measure(pathlib.Path(directory) / (name + ".db"), records(generations))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
