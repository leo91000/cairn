# /// script
# dependencies = ["psycopg[binary]==3.2.10"]
# ///
"""Local alias overhead; never measures production relay/network latency.

Run: CAIRN_SLUG_MEASUREMENT_DATABASE_URL=<disposable Postgres database> \
     uv run scripts/measure-readable-slugs.py
An isolated Postgres schema is created and removed; SQLite files are temporary.
"""

import json
import os
import platform
import random
import sqlite3
import statistics
import tempfile
import time
import uuid
from pathlib import Path

import psycopg

ROWS = 1000
READS = 5000
WRITES = 100
rng = random.Random(42)
ids = [str(uuid.UUID(int=rng.getrandbits(128))) for _ in range(ROWS)]
records = [
    {"id": key, "title": "A representative conversation", "updatedAt": 123}
    for key in ids
]
slugs = [f"quiet-cedar-{i:08x}" for i in range(ROWS)]
encoded = lambda value: json.dumps(value, separators=(",", ":"))


def measure(operation, count, cpu=time.process_time):
    start_cpu = cpu()
    values = []
    for i in range(count):
        start = time.perf_counter_ns()
        operation(i)
        values.append((time.perf_counter_ns() - start) / 1000)
    return {
        "median_us": round(statistics.median(values), 2),
        "cpu_ms": round((cpu() - start_cpu) * 1000, 2),
        "operations": count,
    }


def sqlite(directory, aliases):
    file = Path(directory) / f"sqlite-{aliases}.db"
    db = sqlite3.connect(file)
    db.executescript(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);"
    )
    if aliases:
        db.execute(
            "CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats'"
        )
    values = [
        dict(record, **({"slug": slugs[i]} if aliases else {}))
        for i, record in enumerate(records)
    ]
    db.executemany(
        "INSERT INTO records VALUES (?,'chats',?,123)",
        [(ids[i], encoded(value)) for i, value in enumerate(values)],
    )
    db.commit()
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    result = {
        "encoded_bytes_per_record": len(encoded(values[0]).encode()),
        "database_bytes": file.stat().st_size,
    }
    db.close()

    def fresh(i):
        with sqlite3.connect(file) as connection:
            key = ids[i]
            if aliases:
                key = connection.execute(
                    "SELECT id FROM records WHERE kind='chats' AND json_extract(data,'$.slug')=?",
                    (slugs[i],),
                ).fetchone()[0]
            connection.execute(
                "SELECT data FROM records WHERE kind='chats' AND id=?", (key,)
            ).fetchone()

    result["first_read_new_connection_including_open"] = measure(fresh, 20)
    db = sqlite3.connect(file)

    def read(i):
        key = ids[i % ROWS]
        if aliases:
            key = db.execute(
                "SELECT id FROM records WHERE kind='chats' AND json_extract(data,'$.slug')=?",
                (slugs[i % ROWS],),
            ).fetchone()[0]
        db.execute(
            "SELECT data FROM records WHERE kind='chats' AND id=?", (key,)
        ).fetchone()

    result["warm_read_by_slug" if aliases else "warm_read_by_uuid"] = measure(
        read, READS
    )

    def write(i):
        value = values[i]
        if aliases:
            # The manager preserves the stored slug on every conversation update.
            value = json.loads(
                db.execute(
                    "SELECT data FROM records WHERE kind='chats' AND id=?", (ids[i],)
                ).fetchone()[0]
            )
        db.execute(
            "UPDATE records SET data=?,updated_at=124 WHERE kind='chats' AND id=?",
            (encoded(dict(value, title=f"Updated conversation {i}")), ids[i]),
        )
        db.commit()

    result["write_commit"] = measure(write, WRITES)
    db.close()
    return result


def postgres(aliases):
    connection = psycopg.connect(
        os.environ["CAIRN_SLUG_MEASUREMENT_DATABASE_URL"], autocommit=True
    )
    schema = "slug_measurement_" + uuid.uuid4().hex
    connection.execute(f"CREATE SCHEMA {schema}")
    connection.execute(f"SET search_path TO {schema}")
    # Use real, WAL-logged tables and commit each write.
    generator = (
        Path("crates/beacon/migrations/202610120001_readable_slugs.sql")
        .read_text()
        .split("ALTER TABLE", 1)[0]
    )
    connection.execute(generator)
    extra = (
        ",slug text NOT NULL UNIQUE DEFAULT cairn_readable_slug()" if aliases else ""
    )
    connection.execute(
        f"CREATE TABLE installations(id text PRIMARY KEY,name text NOT NULL{extra})"
    )
    with connection.transaction(), connection.cursor() as cursor:
        cursor.executemany(
            "INSERT INTO installations(id,name) VALUES (%s,%s)",
            [(key, "Example installation") for key in ids],
        )
    pid = connection.execute("SELECT pg_backend_pid()").fetchone()[0]
    ticks = os.sysconf("SC_CLK_TCK")

    def cpu():
        fields = Path(f"/proc/{pid}/stat").read_text().split()
        return (int(fields[13]) + int(fields[14])) / ticks

    column = ",slug" if aliases else ""
    result = {
        "encoded_row_bytes": connection.execute(
            "SELECT pg_column_size(i) FROM installations i LIMIT 1"
        ).fetchone()[0]
    }
    # First lookup in a fresh backend/table; OS buffers are still warm.
    result["first_read"] = measure(
        lambda i: connection.execute(
            f"SELECT id,name{column} FROM installations WHERE id=%s", (ids[i],)
        ).fetchone(),
        1,
        cpu,
    )
    result["warm_read"] = measure(
        lambda i: connection.execute(
            f"SELECT id,name{column} FROM installations WHERE id=%s", (ids[i % ROWS],)
        ).fetchone(),
        READS,
        cpu,
    )
    if aliases:
        host_slugs = [row[0] for row in connection.execute(
            "SELECT slug FROM installations ORDER BY id"
        ).fetchall()]
        result["warm_host_resolution"] = measure(
            lambda i: connection.execute(
                "SELECT id FROM installations WHERE slug=%s", (host_slugs[i % ROWS],)
            ).fetchone(),
            READS,
            cpu,
        )
    result["write_commit"] = measure(
        lambda i: connection.execute(
            "INSERT INTO installations(id,name) VALUES (%s,'Example installation')",
            (str(uuid.uuid4()),),
        ),
        WRITES,
        cpu,
    )
    result["cpu_scope"] = (
        "Postgres backend user+system time, 10 ms clock ticks; TCP loopback included in latency"
    )
    connection.execute(f"DROP SCHEMA {schema} CASCADE")
    connection.close()
    return result


with tempfile.TemporaryDirectory(prefix="cairn-slug-measurements-") as directory:
    result = {
        "host": platform.platform(),
        "cpu_count": os.cpu_count(),
        "sqlite_version": sqlite3.sqlite_version,
        "workload": {
            "rows": ROWS,
            "reads": READS,
            "writes": WRITES,
            "sqlite_journal": "WAL/FULL",
        },
        "cold_read_limits": "First reads after reconnect, OS caches warm; no physical cache eviction",
        "sqlite_reference": sqlite(directory, False),
        "sqlite_slugs": sqlite(directory, True),
        "postgres_reference": postgres(False),
        "postgres_slugs": postgres(True),
    }
    print(json.dumps(result, indent=2))
