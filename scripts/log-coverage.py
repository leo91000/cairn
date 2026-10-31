#!/usr/bin/env python3
"""Bounded source-host log counters. Never emit or persist source messages."""
import argparse
import datetime as dt
import json
import os
import re
import selectors
import subprocess
import time

NANO = 1_000_000_000
STAMP = re.compile(rb"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?Z$")
LEVEL = re.compile(r"^\d{4}-\d\d-\d\dT\S+\s+(TRACE|DEBUG|INFO|WARN|ERROR)\s+")
COUNTERS = ("records", "bytes", "errors", "panics", "leasesExpired", "fencing", "journalOpen", "kernelIO", "hungTask", "unclassified", "stdout", "stderr", "trace", "debug", "info", "warnings")


def timestamp(value):
    match = STAMP.fullmatch(value.encode("ascii"))
    if not match:
        raise ValueError("timestamp")
    seconds = int(dt.datetime.fromisoformat(match[1].decode() + "+00:00").timestamp())
    return seconds * NANO + int((match[2] or b"").ljust(9, b"0"))


def iso(value):
    seconds, nanos = divmod(value, NANO)
    return dt.datetime.fromtimestamp(seconds, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S") + f".{nanos:09d}Z"


def empty_counts():
    return dict.fromkeys(COUNTERS, 0)


def merge_counts(target, addition):
    for key in COUNTERS:
        target[key] += addition[key]


def record(line, stream, kind):
    if kind == "docker":
        stamp, message = line.split(b" ", 1)
        at = timestamp(stamp.decode("ascii"))
        return at, stamp.decode() + ":" + stream, message.decode("utf8"), stream
    item = json.loads(line)
    at = int(item["__REALTIME_TIMESTAMP"]) * 1000
    message = item.get("MESSAGE")
    if not isinstance(message, str):
        raise ValueError("message")
    cursor = item.get("__CURSOR")
    if not isinstance(cursor, str) or not 0 < len(cursor) <= 256:
        raise ValueError("cursor")
    return at, cursor, message, "stdout"


def classify(counts, message, kind):
    level = LEVEL.match(message)
    panic = re.match(r"^(?:thread [^\n]{0,240}panicked(?: at|:)|panicked at)", message)
    if level:
        severity = {"TRACE": "trace", "DEBUG": "debug", "INFO": "info", "WARN": "warnings", "ERROR": "errors"}[level[1]]
        counts[severity] += 1
        counts["leasesExpired"] += bool(re.search(r"\blease_expired\b", message))
    elif not panic and kind != "kernel":
        counts["unclassified"] += 1
    counts["panics"] += bool(panic)
    counts["fencing"] += "fencing must be retried" in message
    counts["journalOpen"] += "unable to open database file" in message
    if kind == "kernel":
        counts["kernelIO"] += bool(re.search(r"I/O error|EXT4-fs error|nvme.*timeout", message, re.I))
        counts["hungTask"] += bool(re.search(r"blocked(?: in I/O wait)? for more than\s*\d+\s*seconds", message, re.I))


def read_page(command, kind, lower, upper, anchor, deadline, byte_limit, line_limit=16384):
    counts = empty_counts()
    result = {"counts": counts, "workBytes": 0, "eof": False, "exitCode": None,
              "reason": None, "anchorFound": anchor is None, "anchor": None}
    child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL)
    selector = selectors.DefaultSelector()
    buffers = {}
    for name, pipe in (("stdout", child.stdout), ("stderr", child.stderr)):
        os.set_blocking(pipe.fileno(), False)
        selector.register(pipe, selectors.EVENT_READ, name)
        buffers[name] = bytearray()
    try:
        while selector.get_map() and not result["reason"]:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                result["reason"] = "time_budget"
                break
            for key, _ in selector.select(min(remaining, 0.1)):
                chunk = os.read(key.fd, min(16384, byte_limit - result["workBytes"] + 1))
                if not chunk:
                    selector.unregister(key.fileobj)
                    if buffers[key.data]:
                        result["reason"] = "incomplete_record"
                    continue
                result["workBytes"] += len(chunk)
                if result["workBytes"] > byte_limit:
                    result["reason"] = "byte_budget"
                    break
                buffer = buffers[key.data]
                buffer.extend(chunk)
                while b"\n" in buffer:
                    end = buffer.index(b"\n")
                    if end > line_limit:
                        result["reason"] = "record_limit"
                        break
                    line = bytes(buffer[:end])
                    del buffer[:end + 1]
                    try:
                        at, key_id, message, stream = record(line, key.data, "journal" if kind == "kernel" else kind)
                    except (ValueError, KeyError, TypeError, UnicodeError, OverflowError, RecursionError):
                        result["reason"] = "invalid_record"
                        break
                    if anchor and key_id == anchor["key"]:
                        result["anchorFound"] = True
                    if not lower <= at < upper:
                        continue
                    counts["records"] += 1
                    counts["bytes"] += end + 1
                    counts[stream] += 1
                    classify(counts, message, kind)
                    if result["anchor"] is None or at >= int(result["anchor"]["at"]):
                        result["anchor"] = {"at": str(at), "key": key_id}
                if len(buffer) > line_limit:
                    result["reason"] = "record_limit"
                if result["reason"]:
                    break
        if not result["reason"]:
            result["eof"] = True
            try:
                result["exitCode"] = child.wait(timeout=max(0.001, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                result["reason"] = "time_budget"
            if result["exitCode"] != 0 and not result["reason"]:
                result["reason"] = "source_exit"
    finally:
        selector.close()
        if child.poll() is None:
            child.kill()
        try:
            child.wait(timeout=0.2)
        except subprocess.TimeoutExpired:
            result["reason"] = "cleanup_timeout"
            result["eof"] = False
        child.stdout.close()
        child.stderr.close()
    result["ok"] = result["eof"] and result["exitCode"] == 0 and result["reason"] is None
    return result


def metadata(request, deadline):
    if request["kind"] != "docker":
        with open("/proc/sys/kernel/random/boot_id", encoding="ascii") as source:
            return {"id": "kernel" if request["kind"] == "kernel" else "journal:" + request["unit"], "epoch": source.read(64).strip()}
    command = ["docker", "inspect", "--format", "{{.Id}} {{.State.StartedAt}}", request["container"]]
    child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    try:
        # Inspect returns only two selected metadata fields, never configuration.
        selector = selectors.DefaultSelector()
        selector.register(child.stdout, selectors.EVENT_READ)
        data = bytearray()
        try:
            while True:
                remaining = min(2, deadline - time.monotonic())
                if remaining <= 0 or not selector.select(remaining):
                    raise ValueError("metadata_timeout")
                chunk = os.read(child.stdout.fileno(), 513 - len(data))
                if not chunk:
                    break
                data.extend(chunk)
                if len(data) > 512:
                    raise ValueError("metadata_limit")
        finally:
            selector.close()
        code = child.wait(timeout=max(0.001, min(2, deadline - time.monotonic())))
        parts = data.decode("ascii").strip().split()
        if code != 0 or len(parts) != 2 or not re.fullmatch("[a-f0-9]{64}", parts[0]):
            raise ValueError("source_metadata")
        timestamp(parts[1])
        return {"id": parts[0], "epoch": parts[1]}
    finally:
        if child.poll() is None:
            child.kill()
        try:
            child.wait(timeout=0.2)
        except subprocess.TimeoutExpired:
            raise ValueError("metadata_cleanup") from None
        child.stdout.close()


def command_for(request, identity, lower, upper):
    if request["kind"] == "docker":
        return ["docker", "logs", "--timestamps", "--since", iso(lower), "--until", iso(upper), identity["id"]]
    command = ["journalctl", "--no-pager", "--all", "--output=json", "--output-fields=__REALTIME_TIMESTAMP,__CURSOR,MESSAGE", "--since", "@" + str(lower // NANO) + f".{lower % NANO:09d}",
               "--until", "@" + str(upper // NANO) + f".{upper % NANO:09d}"]
    return command + (["--kernel"] if request["kind"] == "kernel" else ["--unit", request["unit"]])


def collect(request, get_metadata=metadata, reader=read_page, now_ns=None):
    began = time.monotonic()
    budget = min(10_000, max(100, request.get("timeBudgetMs", 10_000)))
    deadline = began + budget / 1000
    read_deadline = deadline - (2.4 if request["kind"] == "docker" else 0.2)
    work_limit = min(2 * 1024 * 1024, max(1024, request.get("workBytes", 2 * 1024 * 1024)))
    page_limit = min(512000, work_limit)
    max_attempts = min(8, max(1, request.get("maxAttempts", 8)))
    lower = timestamp(request["since"])
    requested_upper = timestamp(request["until"])
    upper = min(requested_upper, (time.time_ns() if now_ns is None else now_ns) - 2 * NANO)
    if lower >= upper:
        raise ValueError("window")
    identity = get_metadata(request, deadline)
    prior = request.get("previous") or {}
    changed = prior.get("identity") is not None and prior["identity"] != identity
    anchor = prior.get("anchor") if not changed else None
    if anchor and (not isinstance(anchor.get("key"), str) or len(anchor["key"]) > 300 or not str(anchor.get("at", "")).isdigit()):
        raise ValueError("anchor")
    continuity = "source_changed" if changed else "unproven_bootstrap" if anchor is None else "anchor_pending"
    counts, partial = empty_counts(), empty_counts()
    through, work, pages, attempts = lower, 0, 0, 0
    reasons = set()
    previous_span = str(prior.get("pageSpanNs", 70 * NANO))
    if not previous_span.isdigit() or not 1_000_000 <= int(previous_span) <= 70 * NANO:
        raise ValueError("page_span")
    span = 70 * NANO if changed else int(previous_span)
    last = None
    while through < upper and attempts < max_attempts and work < work_limit and time.monotonic() < read_deadline:
        end = min(through + span, upper)
        anchor_due = anchor is not None and int(anchor["at"]) < end
        check_anchor = anchor if anchor_due else None
        read_since = min(through, int(anchor["at"])) if anchor_due else through
        allowance = min(work_limit - work, page_limit)
        last = reader(command_for(request, identity, read_since, end), request["kind"], through, end,
                      check_anchor, read_deadline, allowance)
        attempts += 1
        work += last["workBytes"]
        if not last["ok"]:
            merge_counts(partial, last["counts"])
            reasons.add(last["reason"] or "source_exit")
            if last["reason"] not in ("byte_budget", "time_budget") or span <= 1_000_000:
                break
            if last["reason"] == "byte_budget" and allowance < page_limit:
                # A shortened final allowance says nothing about page density.
                # Preserve the learned width for the next fresh work budget.
                break
            span = max(1_000_000, span // 2)
            continue
        if anchor_due:
            if not last["anchorFound"]:
                continuity = "anchor_missing"
            elif continuity == "anchor_pending":
                continuity = "anchor_confirmed"
        merge_counts(counts, last["counts"])
        # Keep an existing future anchor across narrow overlap pages/ticks.
        # It can only be checked once a completed page includes its timestamp.
        if anchor is None or anchor_due:
            anchor = last["anchor"] or anchor
        through, pages = end, pages + 1
        # Relearn available capacity during backlog too: waiting for catch-up
        # before widening would make a formerly busy source lag forever.
        if last["workBytes"] <= page_limit // 4:
            span = min(70 * NANO, span * 2)
    if through < upper:
        if time.monotonic() >= read_deadline:
            reasons.add("time_budget")
        if work >= work_limit:
            reasons.add("work_budget")
        if attempts >= max_attempts:
            reasons.add("attempt_budget")
    after = get_metadata(request, deadline)
    if after != identity:
        continuity = "source_changed"
        through = lower
    caught_up = through == requested_upper
    available = through == upper
    return {"version": 1, "identity": identity, "window": {"since": iso(lower), "until": iso(requested_upper), "sourceUntil": iso(upper)},
            "coveredThrough": iso(through), "anchor": anchor, "pageSpanNs": str(span), "continuity": continuity,
            "continuityUnknown": continuity != "anchor_confirmed", "sourceChanged": continuity == "source_changed",
            "complete": caught_up and available and continuity not in ("source_changed", "anchor_pending") and bool(last and last["ok"]), "caughtUp": caught_up,
            "pages": pages, "attempts": attempts, "counts": counts, "partialCounts": partial,
            "workBytes": work, "limits": sorted(reasons), "lastExitCode": last["exitCode"] if last else None,
            "eof": bool(last and last["ok"]), "elapsedMs": round((time.monotonic() - began) * 1000)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request", required=True)
    args = parser.parse_args()
    try:
        if len(args.request) > 4096:
            raise ValueError("request_limit")
        request = json.loads(args.request)
        if request.get("kind") not in ("docker", "journal", "kernel"):
            raise ValueError("source_kind")
        target = request.get("container") if request["kind"] == "docker" else request.get("unit", "kernel")
        if not isinstance(target, str) or not re.fullmatch(r"[A-Za-z0-9_.:@-]{1,200}", target) or target.startswith("-"):
            raise ValueError("source_target")
        result = {"ok": True, "data": collect(request)}
    except (OSError, ValueError, KeyError, TypeError, OverflowError, subprocess.SubprocessError):
        result = {"ok": False, "reason": "source_or_request_failure"}
    print(json.dumps(result, separators=(",", ":")))


if __name__ == "__main__":
    main()
