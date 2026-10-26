"""C3 SQL/model experiment, never imported by Cairn's scheduler.

Only synthetic SQLite data; no network, agent homes, persistent index or state.
"""

import argparse
import hashlib
import itertools
import json
import multiprocessing
import os
import platform
import shutil
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PENDING = "SELECT DISTINCT chat_id FROM chat_messages WHERE json_extract(data,'$.status') IN ('queued','sending')"
CHATS = "SELECT data FROM records WHERE kind='chats' ORDER BY updated_at DESC"
MESSAGES = "SELECT data FROM chat_messages WHERE chat_id=? ORDER BY created_at,rowid"
SCHEMA = """
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE TABLE runs(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,project_id TEXT NOT NULL,status TEXT NOT NULL,created_at INTEGER NOT NULL,data TEXT NOT NULL,dedupe TEXT UNIQUE);
CREATE TABLE chat_messages(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,data TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id,created_at);
CREATE TABLE kv(key TEXT PRIMARY KEY,data TEXT NOT NULL,expires INTEGER);
"""


def encoded(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


def connect(file):
    db = sqlite3.connect(file, isolation_level=None, timeout=5)
    db.execute("PRAGMA synchronous=FULL")
    return db


def active(chat):
    lifecycle = chat.get("lifecycle")
    return not isinstance(lifecycle, str) or lifecycle == "active"


def attention(chat, run):
    return (run is not None and run.get("status") != "succeeded"
            and chat.get("cancelledByDeletion") is not True
            and chat.get("sessionRestartRequested") is not True)


def required(chat, run, pending):
    """Conservative snapshot filter; no admission/launch policy is changed."""
    return (chat["id"] in pending
            or isinstance(chat.get("wakeup"), dict)
            or (run is not None and (run.get("status") in ("queued", "running")
                                    or run.get("recoveryPending") is True))
            or attention(chat, run))


def actions(chat, run, messages, busy=False, now=100):
    """Snapshot action model, NOT Service::chat_tick or its transaction rechecks.

    Action tuples preserve order and message identity. Conditional readiness is a
    supplied observation, not a reimplementation of GitHub/run condition probes.
    No admission, permissions, run preparation, resume or movement is executed.
    """
    if not active(chat):
        return []
    pending = [m for m in messages if m.get("status") in ("queued", "sending")]
    if run and run.get("status") in ("queued", "running"):
        steering = [m["id"] for m in pending if m.get("mode") == "steer"
                    and (chat.get("paused") is not True or isinstance(m.get("questionId"), str))
                    and m["id"] != run.get("chatExecution", {}).get("messageId")]
        # Publishing an empty steering list clears stale input too.
        return [("publish_steering", steering, chat.get("withdrawals", []))]
    if busy or (run and run.get("recoveryPending") is True):
        return []
    if attention(chat, run):
        result = [("cancel_wakeup",)] if isinstance(chat.get("wakeup"), dict) else []
        if chat.get("paused") is not True:
            result.extend(("requeue", m["id"]) for m in pending if m["status"] == "sending")
            result.append(("pause",))
        return result
    if chat.get("paused") is True:
        return []
    wakeup = chat.get("wakeup")
    resumable = (isinstance(wakeup, dict) and run is not None
                 and run.get("status") == "succeeded"
                 and chat.get("sessionRestartRequested") is not True
                 and run.get("cancelRequestedAt") is None
                 and run.get("workspaceCleanedAt") is None
                 and run.get("chatExecution", {}).get("messageId") == wakeup.get("messageId"))
    unhandled = any(m.get("status") == "sending" or m.get("mode") == "steer" for m in pending)
    holds = resumable and not unhandled
    if not holds and pending:
        return ([('requeue', m['id']) for m in pending if m['status'] == 'sending']
                + [("prepare_message", pending[0]["id"])])
    if holds:
        condition = isinstance(wakeup.get("condition"), dict)
        ready = wakeup.get("observed", False) if condition else wakeup.get("wakeAt", now + 1) <= now
        if ready:
            return [("prepare_wakeup", wakeup["id"])]
    return []


def tick(db, policy):
    """Time SQL + decoding + model, retaining per-chat run reads in both arms.

    The reference exercises the no-op IMMEDIATE next_queued transaction on idle
    unpaused chats. It does not emulate the async Store actor or launch writes.
    Both arms are read-only except for those empty transactions.
    """
    chats = [json.loads(row[0]) for row in db.execute(CHATS)]
    pending = {row[0] for row in db.execute(PENDING)} if policy == "grouped" else set()
    trace = []
    histories = 0
    transactions = 0
    transaction_ns = 0
    for chat in chats:
        if not active(chat):
            continue
        row = (db.execute("SELECT data FROM runs WHERE id=?", (chat["runId"],)).fetchone()
               if isinstance(chat.get("runId"), str) else None)
        run = json.loads(row[0]) if row else None
        if policy == "grouped" and not required(chat, run, pending):
            continue
        # The benchmark corpus only contains idle succeeded/no-run chats. The
        # behavior matrix separately exercises other snapshot branches.
        start = time.perf_counter_ns()
        db.execute("BEGIN IMMEDIATE")
        messages = [json.loads(row[0]) for row in db.execute(MESSAGES, (chat["id"],))]
        db.execute("COMMIT")
        transaction_ns += time.perf_counter_ns() - start
        transactions += 1
        histories += len(messages)
        trace.extend((chat["id"], action) for action in actions(chat, run, messages))
    return trace, {"decoded_messages": histories, "immediate_transactions": transactions,
                   "immediate_section_ns": transaction_ns}


def fixture(file, chats, messages, retention, plugins):
    db = connect(file)
    db.execute("PRAGMA journal_mode=WAL")
    db.executescript(SCHEMA)
    db.execute("BEGIN")
    db.execute("INSERT INTO kv VALUES ('conversation-expiration',?,NULL)",
               (encoded({"inactivityDays": retention}),))
    if plugins:
        # Shape/size interference proxy, not a running Luau/external plugin.
        db.execute("INSERT INTO records VALUES ('plugin','plugin-instances',?,0)",
                   (encoded({"id": "plugin", "enabled": True, "state": "x" * 1024}),))
    encoded_bytes = 0
    for i in range(chats):
        chat_id = f"chat-{i:05}"
        chat = {"id": chat_id, "updatedAt": i, "lastActivityAt": i,
                "title": "Synthetic C3 conversation", "paused": False}
        if i % 2:
            chat["runId"] = f"run-{i}"
            run = {"id": chat["runId"], "status": "succeeded", "createdAt": i}
            data = encoded(run)
            encoded_bytes += len(data.encode())
            db.execute("INSERT INTO runs VALUES (?,?,?,'succeeded',?,?,NULL)",
                       (run["id"], chat_id, "synthetic", i, data))
        data = encoded(chat)
        encoded_bytes += len(data.encode())
        db.execute("INSERT INTO records VALUES (?,'chats',?,?)", (chat_id, data, i))
        for j in range(messages):
            message = {"id": f"{chat_id}-{j}", "chatId": chat_id, "createdAt": j,
                       "status": "delivered", "mode": "queue", "text": "Synthetic history. " * 8,
                       "attachments": []}
            data = encoded(message)
            encoded_bytes += len(data.encode())
            db.execute("INSERT INTO chat_messages VALUES (?,?,?,?)", (message["id"], chat_id, data, j))
    db.execute("COMMIT")
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.close()
    return {"encoded_chat_run_message_bytes": encoded_bytes, "database_bytes": file.stat().st_size}


def writer(file, plugins, channel):
    db = connect(file)
    samples = []
    channel.send("ready")
    cpu_start = time.process_time_ns()
    try:
        while True:
            start = time.perf_counter_ns()
            db.execute("BEGIN IMMEDIATE")
            if plugins:
                db.execute("UPDATE records SET updated_at=updated_at+1 WHERE id='plugin'")
            else:
                db.execute("INSERT INTO kv VALUES ('synthetic-writer',?,NULL) ON CONFLICT(key) DO UPDATE SET data=excluded.data",
                           (str(len(samples)),))
            db.execute("COMMIT")
            samples.append(time.perf_counter_ns() - start)
            # Experimental offered load, not a product budget. Pipes also work
            # in VMs without /dev/shm; no host semaphore/mount changes needed.
            if channel.poll(0.01):
                assert channel.recv() == "stop"
                break
        channel.send({"latencies_ns": samples, "cpu_ns": time.process_time_ns() - cpu_start})
    finally:
        db.close()
        channel.close()


def timed(operation):
    cpu = time.process_time_ns()
    start = time.perf_counter_ns()
    value = operation()
    return value, {"wall_ns": time.perf_counter_ns() - start, "cpu_ns": time.process_time_ns() - cpu}


def measure_case(directory, config, repeats):
    file = directory / "study.db"
    sizes = fixture(file, config["chats"], config["messages"], config["retention_days"], config["plugins_proxy"])
    samples = []
    db = connect(file)
    process = None
    interference = None
    failure = None
    write_samples = []
    if config["writer"]:
        ctx = multiprocessing.get_context("spawn")
        receiver, sender = ctx.Pipe()
        process = ctx.Process(target=writer, args=(file, config["plugins_proxy"], sender))
        process.start()
        sender.close()
        if not receiver.poll(10):
            process.terminate()
            process.join()
            raise RuntimeError("synthetic writer did not initialize")
        assert receiver.recv() == "ready"
    try:
        for connection in ("warm", "fresh_connection"):
            for repetition in range(-1, repeats):  # One warmup per arm/mode.
                policies = ("reference", "grouped") if repetition % 2 == 0 else ("grouped", "reference")
                for order, policy in enumerate(policies):
                    current = connect(file) if connection == "fresh_connection" else db
                    try:
                        (trace, counts), timing = timed(lambda: tick(current, policy))
                        assert trace == [], "idle synthetic corpus unexpectedly performed an action"
                        if repetition >= 0:
                            samples.append(dict(policy=policy, connection=connection,
                                                repetition=repetition, order=order, **timing, **counts))
                    finally:
                        if current is not db:
                            current.close()
        # Stable unchanged-chat upserts, same representation for both policies.
        for _ in range(repeats):
            _, timing = timed(lambda: db.execute("UPDATE records SET data=data WHERE id='chat-00000'"))
            write_samples.append(timing)
    except sqlite3.Error as error:
        # Preserve completed samples and the exact failure; never retry a case
        # or raise a busy timeout merely to obtain a green matrix.
        failure = {"name": error.sqlite_errorname, "code": error.sqlite_errorcode, "message": str(error)}
    finally:
        db.close()
        if process:
            receiver.send("stop")
            # Drain before join: a full pipe must not deadlock writer shutdown.
            if receiver.poll(10):
                interference = receiver.recv()
            process.join(10)
            if process.is_alive():
                process.terminate()
                process.join()
                raise RuntimeError("synthetic writer exceeded shutdown guard")
            if process.exitcode != 0 or interference is None:
                raise RuntimeError("synthetic writer failed; no valid sample")
            receiver.close()
    return dict(config=config, sizes=sizes, samples=samples, unchanged_chat_writes=write_samples,
                interference=interference, error=failure)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True, help="owned persistent fixture parent")
    parser.add_argument("--output", type=Path, required=True, help="append-only raw JSONL, must not exist")
    parser.add_argument("--repeats", type=int, default=6)
    parser.add_argument("--chats", type=int, nargs="+", default=[100, 1000, 5000])
    args = parser.parse_args()
    if args.repeats < 2 or any(n < 1 or n > 5000 for n in args.chats):
        parser.error("experimental resource guards: repeats >=2; chats 1..5000")
    if not (args.directory / ".owner").is_file():
        parser.error("fixture parent requires an explicit .owner marker")
    if shutil.disk_usage(args.directory).free < 12 * 1024**3:
        parser.error("experimental free-disk guard: 10 GiB production reserve + 2 GiB margin")
    # Ensure the subset schema is still identical to shipped source, without
    # opening any user database. Other production tables/indexes are omitted.
    source = (ROOT / "crates/installation/src/store.rs").read_text()
    for statement in SCHEMA.strip().splitlines():
        assert statement.rstrip(";") in source.replace(" IF NOT EXISTS", ""), f"schema drift: {statement}"
    identity = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    header = {"type": "conditions", "head": identity,
              "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "budget": "No accepted user latency or writer-occupancy budget supplied; no production acceptance decision.",
              "host": {"platform": platform.platform(), "python": platform.python_version(),
                       "sqlite": sqlite3.sqlite_version, "cpus": os.cpu_count(),
                       "meminfo": Path('/proc/meminfo').read_text().splitlines()[:3],
                       "filesystem": subprocess.check_output(["df", "-T", str(args.directory)], text=True)},
              "repeats": args.repeats, "warmups": 1,
              "fresh_connection": "open/close excluded; no OS cache eviction; earlier setup/reads warm the host",
              "scope": "Python SQL/snapshot model, not native Store actors, full scheduler, retention sweeps or plugin execution; local only"}
    with args.output.open("x") as output:
        output.write(encoded(header) + "\n")
        output.flush()
        for chats, messages, retention, interference, plugins in itertools.product(
                args.chats, [20, 200], [0, 7], [False, True], [False, True]):
            if shutil.disk_usage(args.directory).free < 12 * 1024**3:
                raise RuntimeError("free-disk guard reached; partial JSONL retained")
            config = dict(chats=chats, messages=messages, retention_days=retention,
                          writer=interference, plugins_proxy=plugins)
            with tempfile.TemporaryDirectory(prefix="c3-", dir=args.directory) as directory:
                result = measure_case(Path(directory), config, args.repeats)
                output.write(encoded(dict(type="case_error" if result["error"] else "case", **result)) + "\n")
                output.flush()
            print(encoded(config), flush=True)


if __name__ == "__main__":
    main()
