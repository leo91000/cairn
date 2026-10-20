#!/usr/bin/env python3
"""Synthetic SQLite question-retirement shape, not complete host startup latency."""
import argparse
import json
import os
from pathlib import Path
import platform
import sqlite3
import statistics
import tempfile
import time

MARKER = 'migration:question-turn-attribution'
NOTICE = ('Questions saved before this update could not be matched to their original turn. '
          'They were withdrawn without answering. Ask the agent to repeat them in this conversation.')
UPSERT = 'INSERT INTO kv VALUES(?,?,NULL) ON CONFLICT(key) DO UPDATE SET data=excluded.data,expires=excluded.expires'


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'))


def seed(path):
    db = sqlite3.connect(path)
    db.executescript('''PRAGMA journal_mode=WAL;
        CREATE TABLE kv(key TEXT PRIMARY KEY,data TEXT NOT NULL,expires INTEGER);
        CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL,created_at INTEGER NOT NULL,type TEXT NOT NULL,text TEXT NOT NULL,payload TEXT);
        CREATE TABLE audit(id INTEGER PRIMARY KEY AUTOINCREMENT,created_at INTEGER NOT NULL,action TEXT NOT NULL,detail TEXT NOT NULL);''')
    rows = []
    for chat in range(8):
        for number in range(16):
            question = {'id': f'{chat * 16 + number:064x}', 'chatId': f'chat-{chat}',
                        'runId': f'run-{chat}', 'status': 'pending', 'blocking': True,
                        'fields': [{'id': '0', 'title': 'Synthetic question ' + 'x' * 100,
                                    'secret': True, 'options': []}], 'createdAt': 1800000000000}
            rows.append((f"chat-question:chat-{chat}:{question['id']}", encoded(question)))
    db.executemany(UPSERT, rows)
    db.commit()
    db.execute('PRAGMA wal_checkpoint(TRUNCATE)')
    db.close()


def operation(db, candidate):
    db.execute('BEGIN IMMEDIATE')
    if db.execute('SELECT data FROM kv WHERE key=?', (MARKER,)).fetchone():
        db.commit()
        return
    affected = {}
    for key, raw in db.execute("SELECT key,data FROM kv WHERE key>=? AND key<?", ('chat-question:', 'chat-question;')).fetchall():
        question = json.loads(raw)
        if question.get('turnId') or question['status'] not in ('pending', 'answering'):
            continue
        affected[question['chatId']] = affected.get(question['chatId'], 0) + 1
        if candidate:
            question.update(status='cancelled', blocking=False, retiredReason='missing-turn-attribution')
            db.execute(UPSERT, (key, encoded(question)))
    if candidate:
        for chat, count in affected.items():
            db.execute('INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,?,?,?,?)',
                       (chat.replace('chat-', 'run-'), 1800000000000, 'error', NOTICE, None))
            db.execute('INSERT INTO audit(created_at,action,detail) VALUES(?,?,?)',
                       (1800000000000, 'chat.questions.retired', encoded({'chatId': chat, 'count': count, 'reason': 'missing-turn-attribution'})))
        db.execute(UPSERT, (MARKER, 'true'))
    db.commit()


def summary(times, cpu):
    return {'samples': len(times), 'p50_ms': statistics.median(times) / 1e6,
            'p95_ms': sorted(times)[int((len(times) - 1) * .95)] / 1e6, 'cpu_total_ms': cpu / 1e6}


def measure(root, candidate):
    trials, cpu = [], 0
    for trial in range(30):
        path = root / f'{candidate}-{trial}.db'
        seed(path)
        db = sqlite3.connect(path)
        start_cpu, start = time.process_time_ns(), time.perf_counter_ns()
        operation(db, candidate)
        trials.append(time.perf_counter_ns() - start)
        cpu += time.process_time_ns() - start_cpu
        if trial == 29:
            bytes_kv = db.execute('SELECT SUM(length(CAST(data AS BLOB))) FROM kv').fetchone()[0]
            bytes_events = db.execute('SELECT COALESCE(SUM(length(CAST(text AS BLOB))),0) FROM events').fetchone()[0]
            bytes_audit = db.execute('SELECT COALESCE(SUM(length(CAST(detail AS BLOB))),0) FROM audit').fetchone()[0]
            settings = {key: db.execute('PRAGMA ' + key).fetchone()[0] for key in ['synchronous', 'journal_mode', 'page_size']}
            warm, warm_cpu = [], time.process_time_ns()
            for _ in range(1000):
                began = time.perf_counter_ns()
                db.execute('SELECT data FROM kv WHERE key=?', (MARKER,)).fetchone()
                warm.append(time.perf_counter_ns() - began)
            warm_cpu = time.process_time_ns() - warm_cpu
            cold, cold_cpu = [], time.process_time_ns()
            for _ in range(50):
                began = time.perf_counter_ns()
                reader = sqlite3.connect(path)
                reader.execute('SELECT data FROM kv WHERE key=?', (MARKER,)).fetchone()
                reader.close()
                cold.append(time.perf_counter_ns() - began)
            cold_cpu = time.process_time_ns() - cold_cpu
            db.execute('PRAGMA wal_checkpoint(TRUNCATE)')
        db.close()
    return {'transaction': summary(trials, cpu), 'warm_marker_read': summary(warm, warm_cpu),
            'new_connection_marker_read': summary(cold, cold_cpu),
            'encoded_kv_json_bytes': bytes_kv, 'event_text_bytes': bytes_events,
            'audit_json_bytes': bytes_audit, 'database_bytes': path.stat().st_size, 'pragmas': settings}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--context', required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='question-retirement-') as directory:
        root = Path(directory)
        print(json.dumps({'scope': 'Python/SQLite shape only: marker/prefix reads, question rewrites, event and audit writes. Excludes Rust/actor startup, chat/run lookup, queued-answer retirement, relay, provider and network.',
                          'reference': 'Read existing 128 pending questions across 8 chats; leave unchanged, no marker/event/audit',
                          'candidate': 'Same records retired, one event and audit per chat, one marker per store',
                          'cold_definition': 'New SQLite connection with OS caches retained; transaction excludes connection and seed creation',
                          'host': {'kernel': platform.release(), 'python': platform.python_version(), 'sqlite': sqlite3.sqlite_version,
                                   'cpus': os.cpu_count(), 'load': os.getloadavg(),
                                   'memory': [line for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith(('MemTotal:', 'MemAvailable:'))],
                                   'context': args.context},
                          'before': measure(root, False), 'after': measure(root, True)}, indent=2))


if __name__ == '__main__':
    main()
