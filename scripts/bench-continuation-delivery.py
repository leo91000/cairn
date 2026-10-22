#!/usr/bin/env python3
"""Local JSON/SQLite shape costs of reserving native continuation delivery.

Synthetic source bindings, no credentials or production data. Does not benchmark
Rust, authorization, database actors, models, VM/S3 restoration or network I/O.
"""
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


def identifier(name):
    return str(uuid.uuid5(uuid.NAMESPACE_URL, 'cairn-continuation/' + name))


def encode(value):
    return json.dumps(value, separators=(',', ':'), sort_keys=True)


def sample(values, cpu):
    values.sort()
    return {'samples': len(values), 'p50_ms': statistics.median(values) / 1e6,
            'p95_ms': values[min(len(values) - 1, int(len(values) * .95))] / 1e6,
            'cpu_ms': cpu / 1e6}


def fixture():
    result = []
    for i in range(10):
        result.append({
            'id': identifier(f'input/{i}'), 'connectionId': identifier('instance'),
            'revision': identifier('instance-revision'), 'chatId': identifier(f'chat/{i}'),
            'routing': {
                'id': identifier('route'), 'revision': identifier('route-revision'),
                'resource': identifier('workspace'),
                'resources': [[identifier('workspace'), identifier('workspace-revision')]],
                'digest': 'a' * 64,
                'context': {'team': 'TTEST', 'channel': 'CTEST', 'thread': f'1800000000.{i:06}'},
                'person': {'id': 'UALICE', 'revision': identifier('person-revision')},
            },
            'thread': f'TTEST:CTEST:1800000000.{i:06}', 'userId': 'UALICE',
            'reply': {'workspace': 'TTEST', 'channel': 'CTEST',
                      'resource': identifier('workspace'), 'thread_ts': f'1800000000.{i:06}'},
            'asked': [], 'attempts': 0, 'createdAt': 1800000000000, 'nextAttemptAt': 1800000000000,
        })
    return result


UPSERT = ('INSERT INTO records VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE '
          'SET data=excluded.data,updated_at=excluded.updated_at')


def put(db, kind, value):
    db.execute(UPSERT, (value['id'], kind, encode(value), 1800000000000))


def measure(path, reserved):
    db = sqlite3.connect(path)
    db.executescript('''
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=FULL;
        CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
        CREATE INDEX records_kind ON records(kind,updated_at DESC);
        CREATE TABLE events(id INTEGER PRIMARY KEY,run_id TEXT,type TEXT,payload TEXT);
    ''')
    sources = fixture()
    for value in sources:
        put(db, 'plugin-deliveries', value)
        put(db, 'chats', {'id': value['chatId'], 'wakeup': None})
        put(db, 'plugin-activity', {'id': identifier(value['chatId'] + '/activity'),
                                  'deliveryId': value['id'], 'status': 'working'})
    db.commit()
    writes = []
    previous = {}
    cpu = time.process_time_ns()
    for i in range(100):
        source = sources[i % len(sources)]
        wakeup = {'id': identifier(f'wakeup/{i}'), 'messageId': source['id'], 'seconds': 420,
                  'reason': 'Await the original request result', 'createdAt': 1800000000000,
                  'wakeAt': 1800000420000}
        started = time.perf_counter_ns()
        db.execute('BEGIN IMMEDIATE')
        if reserved:
            if source['id'] in previous:
                db.execute('DELETE FROM records WHERE id=?', (previous[source['id']],))
            child = copy.deepcopy(source)
            child.update(id=wakeup['id'], continuationOf=source['id'])
            put(db, 'plugin-deliveries', child)
            put(db, 'plugin-activity', {'id': identifier(source['chatId'] + '/activity'),
                                      'deliveryId': child['id'], 'status': 'working'})
            previous[source['id']] = child['id']
        put(db, 'chats', {'id': source['chatId'], 'wakeup': wakeup})
        db.execute('INSERT INTO events(run_id,type,payload) VALUES(?,?,?)',
                   (identifier(source['chatId'] + '/run'), 'chat.sleep', encode(wakeup)))
        db.commit()
        writes.append(time.perf_counter_ns() - started)
    write_cpu = time.process_time_ns() - cpu
    query = "SELECT data FROM records WHERE kind='plugin-deliveries' ORDER BY updated_at DESC"
    encoded = db.execute('SELECT kind,data FROM records').fetchall()
    pending = db.execute(query).fetchall()
    warm = []
    cpu = time.process_time_ns()
    for _ in range(1000):
        started = time.perf_counter_ns()
        [json.loads(row[0]) for row in db.execute(query)]
        warm.append(time.perf_counter_ns() - started)
    warm_cpu = time.process_time_ns() - cpu
    pragmas = {k: db.execute('PRAGMA ' + k).fetchone()[0]
               for k in ['page_size', 'journal_mode', 'synchronous']}
    db.execute('PRAGMA wal_checkpoint(TRUNCATE)')
    db.close()
    cold = []
    cpu = time.process_time_ns()
    for _ in range(50):
        started = time.perf_counter_ns()
        with sqlite3.connect(path) as reader:
            [json.loads(row[0]) for row in reader.execute(query)]
        reader.close()
        cold.append(time.perf_counter_ns() - started)
    return {'pending_deliveries': len(pending),
            'acknowledgement_payload_bytes': len(encode({'messageId': identifier('wakeup')})) if reserved else 0,
            'encoded_record_bytes': sum(len(raw.encode()) for _, raw in encoded),
            'encoded_delivery_bytes': sum(len(row[0].encode()) for row in pending),
            'database_bytes_after_checkpoint': path.stat().st_size,
            'schedule_replacement_write': sample(writes, write_cpu),
            'warm_pending_list': sample(warm, warm_cpu),
            'new_connection_pending_list': sample(cold, time.process_time_ns() - cpu),
            'pragmas': pragmas}


def main():
    result = {'scope': __doc__, 'workload': '10 accepted sources, 100 wait registrations/replacements, 1000 warm lists, 50 fresh-connection lists; cap20 with10 reservations',
              'cold_definition': 'new SQLite connection including open/close; OS caches retained',
              'concurrent_work': 'No local compiler, tests, emulator or Postgres during sampling',
              'host': {'kernel': platform.release(), 'python': platform.python_version(),
                       'sqlite': sqlite3.sqlite_version, 'load_average': os.getloadavg(),
                       'memory': {line.split(':')[0]: line.split(':')[1].strip()
                                  for line in pathlib.Path('/proc/meminfo').read_text().splitlines()
                                  if line.startswith(('MemTotal:', 'MemAvailable:'))}}}
    with tempfile.TemporaryDirectory(prefix='cairn-continuation-benchmark-') as root:
        for name, reserved in [('reference_without_reservation', False),
                               ('candidate_with_reservation', True)]:
            result[name] = measure(pathlib.Path(root) / (name + '.db'), reserved)
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
