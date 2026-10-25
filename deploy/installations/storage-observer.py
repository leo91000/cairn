#!/usr/bin/env python3
"""Bounded, private storage observations that survive manager/container restarts.

Only numeric performance fields and aggregate inventory are retained. No log text,
object keys, content hashes, run IDs, environment values or credentials are saved.
"""

import argparse
import datetime as dt
import fcntl
import json
import os
import re
import selectors
import sqlite3
import stat
import subprocess
import tempfile
import time
from pathlib import Path

UTC = dt.timezone.utc
COUNTER_FIELDS = {
    "count",
    "bytes",
    "totalMicros",
    "maxMicros",
    "errors",
    "over10Ms",
    "over100Ms",
    "over1s",
    "inFlight",
    "maxInFlight",
    "concurrentStarts",
}
CODEC_FIELDS = {
    "encodedBytes",
    "decodedInputBytes",
    "rawWrites",
    "zstdWrites",
    "rawReads",
    "zstdReads",
}
INCREMENT_FIELDS = {
    "count",
    "bytes",
    "totalMicros",
    "errors",
    "over10Ms",
    "over100Ms",
    "over1s",
    "concurrentStarts",
} | CODEC_FIELDS
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def command(args, timeout=8, limit=8 * 1024 * 1024, merge_stderr=False):
    """Bound command time and captured bytes; diagnostics never contain raw output."""
    process = subprocess.Popen(
        args,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT if merge_stderr else subprocess.DEVNULL,
    )
    result = bytearray()
    deadline = time.monotonic() + timeout
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return None
                for key, _ in selector.select(min(remaining, 0.5)):
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                    else:
                        result.extend(data)
                        if len(result) > limit:
                            return None
            if process.wait(timeout=max(0.01, deadline - time.monotonic())) != 0:
                return None
        return result.decode("utf-8", errors="replace")
    except (OSError, subprocess.TimeoutExpired):
        return None
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        process.stdout.close()


def number(value):
    return isinstance(value, int) and not isinstance(value, bool) and 0 <= value < 2**64


def clean_metrics(value):
    if not isinstance(value, dict):
        return None
    result = {}
    for name in ("get", "put", "purgeKey"):
        counter = value.get(name)
        if isinstance(counter, dict):
            result[name] = {
                k: v for k, v in counter.items() if k in COUNTER_FIELDS and number(v)
            }
    codec = value.get("blockCodec")
    if not isinstance(codec, dict):
        return None
    result["blockCodec"] = {
        k: v for k, v in codec.items() if k in CODEC_FIELDS and number(v)
    }
    for name in ("encode", "decode", "admission", "readAdmission"):
        if isinstance(codec.get(name), dict):
            result["blockCodec"][name] = {
                k: v
                for k, v in codec[name].items()
                if k in COUNTER_FIELDS and number(v)
            }
    if not {"count", "bytes"} <= result["blockCodec"].get("encode", {}).keys():
        return None
    return result


def parse_timestamp(value):
    if not isinstance(value, str):
        raise ValueError("Timestamp must be a string")
    parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("Timestamp must include its timezone")
    return parsed


def latest_metrics(text, not_before=None):
    latest = None
    for raw in (text or "").splitlines():
        line = ANSI.sub("", raw).strip()
        if "s3_totals" not in line:
            continue
        timestamp, _, payload = line.partition(" ")
        try:
            observed = parse_timestamp(timestamp)
            if not_before is not None and observed < not_before:
                continue
            if payload.startswith("{"):
                fields = json.loads(payload).get("fields", {})
                if fields.get("operation") != "s3_totals":
                    continue
                value = fields.get("metrics")
                value = json.loads(value) if isinstance(value, str) else value
            else:
                match = re.search(r'\boperation="?s3_totals"?\b', payload)
                if not match:
                    continue
                match = re.search(r"\bmetrics=(\{.*)", payload)
                value, _ = json.JSONDecoder().raw_decode(match.group(1))
            metrics = clean_metrics(value)
            if metrics is not None:
                latest = (timestamp, metrics)
        except (ValueError, AttributeError, TypeError):
            continue
    return latest


def deltas(current, previous):
    """Cumulative maxima and concurrency gauges are never differenced."""
    result = {}
    for key, value in current.items():
        old = previous.get(key)
        if isinstance(value, dict) and isinstance(old, dict):
            result[key] = deltas(value, old)
        elif key in INCREMENT_FIELDS and number(value) and number(old) and value >= old:
            result[key] = value - old
    return result


def manager_cpu(manager):
    top = command(["docker", "top", manager, "-eo", "pid,comm"])
    for line in (top or "").splitlines()[1:]:
        fields = line.split()
        if len(fields) != 2 or fields[1] != "cairn":
            continue
        try:
            root = Path("/proc") / fields[0]
            if b"serve" not in (root / "cmdline").read_bytes().split(b"\0")[1:]:
                continue
            data = (root / "stat").read_text().rsplit(")", 1)[1].split()
            hz = os.sysconf("SC_CLK_TCK")
            return {
                "startTicks": int(data[19]),
                "seconds": (int(data[11]) + int(data[12])) / hz,
            }
        except (OSError, ValueError, IndexError):
            continue
    return None


def collect(root, manager, garage, bucket, previous):
    now = dt.datetime.now(UTC)
    sample = {
        "schema": 1,
        "collectorVersion": 3,
        "observedAt": now.isoformat(),
        "errors": [],
        "hostLoad": list(os.getloadavg()),
        "hostCpuCount": os.cpu_count(),
    }
    template = (
        '{"Id":{{json .Id}},"State":{"StartedAt":{{json .State.StartedAt}},'
        '"Status":{{json .State.Status}},"Health":{"Status":'
        "{{if .State.Health}}{{json .State.Health.Status}}{{else}}null{{end}}}},"
        '"Config":{"Image":{{json .Config.Image}}}}'
    )
    metadata = command(
        ["docker", "inspect", "--format", template, manager], limit=65536
    )
    container_ref = manager
    epoch_started = None
    if metadata:
        try:
            value = json.loads(metadata)
            value = value[0] if isinstance(value, list) else value
            container_ref = value["Id"]
            epoch_started = parse_timestamp(value["State"]["StartedAt"])
            sample["manager"] = {
                "container": value["Id"][:12],
                "startedAt": value["State"]["StartedAt"],
                "status": value["State"]["Status"],
                "health": value["State"].get("Health", {}).get("Status"),
                "image": value["Config"]["Image"],
            }
            sample["processCpu"] = manager_cpu(container_ref)
        except (ValueError, KeyError, IndexError, TypeError):
            sample["errors"].append("manager_metadata")
    else:
        sample["errors"].append("manager_unavailable")
    same_epoch = sample.get("manager") is not None and all(
        sample["manager"].get(k) == previous.get("manager", {}).get(k)
        for k in ("container", "startedAt")
    )
    valid_baseline = False
    if same_epoch and previous.get("metrics"):
        try:
            valid_baseline = parse_timestamp(previous["metricsAt"]) >= epoch_started
        except (KeyError, ValueError):
            pass
    logs = command(
        [
            "docker",
            "logs",
            "--since",
            "15m",
            "--tail",
            "2000",
            "--timestamps",
            container_ref,
        ],
        merge_stderr=True,
    )
    # Docker retains a container's logs across restarts. Neither a log snapshot
    # nor a checkpoint written by an older collector may cross this epoch.
    latest = latest_metrics(logs, not_before=epoch_started) if epoch_started else None
    if latest:
        sample["metricsAt"], sample["metrics"] = latest
    elif valid_baseline:
        sample["metricsAt"], sample["metrics"] = (
            previous["metricsAt"],
            previous["metrics"],
        )
    else:
        sample["errors"].append("metrics_unavailable")
    if logs is None:
        sample["errors"].append("log_read_limit_or_failure")
    if sample.get("metricsAt"):
        age = (
            now - dt.datetime.fromisoformat(sample["metricsAt"].replace("Z", "+00:00"))
        ).total_seconds()
        sample["metricsAgeSeconds"] = max(0, round(age, 3))
    sample["counterEpochChanged"] = bool(previous) and not same_epoch
    if (
        valid_baseline
        and sample.get("metrics")
        and sample.get("metricsAt") != previous.get("metricsAt")
    ):
        current = sample["metrics"]["blockCodec"]["encode"]["count"]
        old = previous["metrics"]["blockCodec"]["encode"]["count"]
        sample["counterResetDetected"] = current < old
        if current >= old:
            sample["metricDelta"] = deltas(sample["metrics"], previous["metrics"])
            sample["metricDeltaWindow"] = {
                "from": previous["metricsAt"],
                "to": sample["metricsAt"],
            }
    if same_epoch and previous.get("processCpu") and sample.get("processCpu"):
        cpu, old = sample["processCpu"], previous["processCpu"]
        elapsed = (
            now - dt.datetime.fromisoformat(previous["observedAt"])
        ).total_seconds()
        if (
            cpu["startTicks"] == old["startTicks"]
            and cpu["seconds"] >= old["seconds"]
            and elapsed >= 30
        ):
            sample["managerCpuCoresDuringInterval"] = (
                cpu["seconds"] - old["seconds"]
            ) / elapsed
            sample["intervalSeconds"] = elapsed
    try:
        database = root / "data/manager.db"
        with sqlite3.connect(
            database.resolve().as_uri() + "?mode=ro", uri=True, timeout=1
        ) as db:
            deadline = time.monotonic() + 3
            db.set_progress_handler(lambda: time.monotonic() > deadline, 10000)
            sample["databaseVersion"] = db.execute("PRAGMA user_version").fetchone()[0]
            sample["sharedObjects"] = [
                {"state": r[0], "count": r[1], "plaintextBytes": r[2]}
                for r in db.execute(
                    "SELECT state,count(*),sum(size) FROM shared_objects GROUP BY state"
                )
                if r[0] in ("ready", "pending", "invalid", "deleting")
            ]
            sample["unownedSharedObjects"] = db.execute(
                "SELECT count(*) FROM shared_objects WHERE unused_at IS NOT NULL"
            ).fetchone()[0]
            sample["references"] = db.execute(
                "SELECT count(*) FROM shared_references"
            ).fetchone()[0]
            sample["publications"] = db.execute(
                "SELECT count(*) FROM shared_publications"
            ).fetchone()[0]
            sample["remoteDeletionQueue"] = db.execute(
                "SELECT count(*) FROM remote_deletions"
            ).fetchone()[0]
    except (OSError, sqlite3.Error):
        sample["errors"].append("inventory_unavailable")
    info = command(
        ["docker", "exec", garage, "/garage", "bucket", "info", bucket], limit=65536
    )
    sample["bucket"] = {}
    for line in (info or "").splitlines():
        match = re.match(r"^\s*(Size|Objects):\s*([0-9][0-9., ()A-Za-z/-]*)$", line)
        if match:
            sample["bucket"][match[1]] = match[2]
    if not sample["bucket"]:
        sample["errors"].append("bucket_unavailable")
    return sample


def atomic_json(path, value):
    with tempfile.NamedTemporaryFile(
        "w", dir=path.parent, delete=False, encoding="utf-8"
    ) as file:
        temporary = Path(file.name)
        try:
            json.dump(value, file, separators=(",", ":"))
            file.flush()
            os.fsync(file.fileno())
        except BaseException:
            temporary.unlink(missing_ok=True)
            raise
    try:
        os.replace(temporary, path)
        fd = os.open(path.parent, os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        temporary.unlink(missing_ok=True)


def previous_sample(directory):
    candidates = []
    state = directory / "state.json"
    if state.exists() and state.stat().st_size <= 65536:
        try:
            candidates.append(json.loads(state.read_text()))
        except (OSError, ValueError):
            pass
    # The append is authoritative if a crash happened before state.json was
    # replaced. Two maximum-sized records cover a valid line plus a torn tail.
    for path in directory.glob("*.jsonl"):
        if (
            not re.fullmatch(r"\d{4}-\d{2}-\d{2}\.\d{4}\.jsonl", path.name)
            or path.is_symlink()
        ):
            continue
        try:
            with path.open("rb") as stream:
                stream.seek(max(0, path.stat().st_size - 131072))
                lines = stream.read(131072).splitlines()
            for line in reversed(lines):
                try:
                    value = json.loads(line)
                    if isinstance(value, dict) and value.get("schema") == 1:
                        candidates.append(value)
                        break
                except (ValueError, UnicodeError):
                    continue
        except OSError:
            continue
    valid = [
        v
        for v in candidates
        if isinstance(v, dict)
        and v.get("schema") == 1
        and number(v.get("sequence", 0))
        and isinstance(v.get("observedAt"), str)
    ]
    return max(valid, key=lambda v: (v.get("sequence", 0), v["observedAt"]), default={})


def repair_segment_tail(path):
    fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        metadata = os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError("Observation segment must be a regular file")
        size = metadata.st_size
        if size and os.pread(fd, 1, size - 1) != b"\n":
            tail = os.pread(fd, min(size, 65536), max(0, size - 65536))
            boundary = tail.rfind(b"\n")
            os.ftruncate(fd, max(0, size - len(tail) + boundary + 1))
            os.fsync(fd)
    finally:
        os.close(fd)


def append_sample(
    directory,
    sample,
    *,
    now=None,
    retention_days=30,
    max_bytes=128 * 1024 * 1024,
    segment_bytes=8 * 1024 * 1024,
):
    now = now or dt.datetime.now(UTC)
    encoded = json.dumps(sample, separators=(",", ":")).encode() + b"\n"
    if (
        len(encoded) > 65536
        or len(encoded) > segment_bytes
        or max_bytes < segment_bytes
    ):
        raise ValueError("Observation exceeds configured size bounds")
    segments = sorted(
        p
        for p in directory.glob("*.jsonl")
        if re.fullmatch(r"\d{4}-\d{2}-\d{2}\.\d{4}\.jsonl", p.name)
    )
    # Repair before choosing a new date/size segment. Also recover closed tails
    # left by earlier collectors, rather than keeping malformed JSONL history.
    for segment in segments:
        repair_segment_tail(segment)
    prefix = now.strftime("%Y-%m-%d")
    today = [p for p in segments if p.name.startswith(prefix + ".")]
    path = today[-1] if today else directory / (prefix + ".0000.jsonl")
    if path.exists() and path.stat().st_size + len(encoded) > segment_bytes:
        number = int(path.name.split(".")[1]) + 1
        if number > 9999:
            raise ValueError("Too many daily segments")
        path = directory / f"{prefix}.{number:04}.jsonl"
    fd = os.open(
        path,
        os.O_RDWR | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW | os.O_NONBLOCK,
        0o600,
    )
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise ValueError("Observation destination must be a regular file")
        remaining = memoryview(encoded)
        while remaining:
            remaining = remaining[os.write(fd, remaining) :]
        os.fsync(fd)
    finally:
        os.close(fd)
    atomic_json(directory / "state.json", sample)
    segments = sorted(
        p
        for p in directory.glob("*.jsonl")
        if re.fullmatch(r"\d{4}-\d{2}-\d{2}\.\d{4}\.jsonl", p.name)
    )
    cutoff = (now - dt.timedelta(days=retention_days)).strftime("%Y-%m-%d")
    total = sum(p.stat().st_size for p in segments)
    for segment in segments:
        if segment != path and (segment.name[:10] < cutoff or total > max_bytes):
            total -= segment.stat().st_size
            segment.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path("/var/lib/cairn-installation")
    )
    parser.add_argument("--output", type=Path)
    parser.add_argument("--manager", default="cairn-installation-manager-1")
    parser.add_argument("--garage", default="cairn-installation-garage-1")
    parser.add_argument("--bucket", default="cairn-disks")
    args = parser.parse_args()
    os.umask(0o077)
    directory = args.output or args.root / "observations/storage"
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if directory.is_symlink():
        parser.error("Observation directory must not be a symlink")
    if directory.stat().st_mode & 0o077:
        parser.error("Observation directory must have private permissions (0700)")
    with (directory / "observer.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        previous = previous_sample(directory)
        sample = collect(args.root, args.manager, args.garage, args.bucket, previous)
        sample["sequence"] = previous.get("sequence", 0) + 1
        if previous:
            sample["deltaFrom"] = previous["observedAt"]
        append_sample(directory, sample)
        print(
            json.dumps(
                {
                    "observedAt": sample["observedAt"],
                    "errors": sample["errors"],
                    "counterEpochChanged": sample["counterEpochChanged"],
                }
            )
        )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, sqlite3.Error, subprocess.SubprocessError):
        raise SystemExit(
            "Storage observation failed; check Docker access, private output permissions and disk space."
        ) from None
