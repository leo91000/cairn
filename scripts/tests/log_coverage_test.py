"""Synthetic records only; exercise real bounded child pipes and collector pages."""
import importlib.util
import json
import pathlib
import shutil
import subprocess
import sys
import time
import unittest

SPEC = importlib.util.spec_from_file_location("coverage", pathlib.Path(__file__).parents[1] / "log-coverage.py")
coverage = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(coverage)
START = coverage.timestamp("2026-10-31T10:24:19Z")
END = START + 70 * coverage.NANO
IDENTITY = {"id": "a" * 64, "epoch": "2026-10-30T00:00:00Z"}


def request(**changes):
    return {"kind": "docker", "container": "fixture", "since": coverage.iso(START), "until": coverage.iso(END), **changes}


def pipe_page(records, lower=START, upper=END, **options):
    # Split both timestamps/UTF-8 and severity across writes. No real logs/files.
    command = [sys.executable, "-c", "import os,sys; data=" + repr(records) + "; [os.write(1,data[i:i+7]) for i in range(0,len(data),7)]"]
    return coverage.read_page(command, options.pop("kind", "docker"), lower, upper, options.pop("anchor", None),
                              time.monotonic() + options.pop("seconds", 2), options.pop("bytes_limit", 512000), **options)


def line(index, message="INFO service: private-sentinel é"):
    return (coverage.iso(START + index * 1000000) + " 2026-10-31T10:24:19Z " + message + "\n").encode()


class CoverageTests(unittest.TestCase):
    def test_fewer_than_tail_cap_can_omit_most_of_a_closed_window(self):
        full = b"".join(line(i, "ERROR service: early") if i == 0 else line(i) for i in range(1009))
        full_result = pipe_page(full)
        tail_result = pipe_page(b"".join(full.splitlines(keepends=True)[-392:]))
        self.assertEqual(full_result["counts"]["records"], 1009)
        self.assertEqual(tail_result["counts"]["records"], 392)
        self.assertTrue(392 < 400)  # The old watcher incorrectly called this complete.
        self.assertEqual(full_result["counts"]["errors"], 1)
        self.assertEqual(tail_result["counts"]["errors"], 0)

    def test_record_counts_do_not_decide_eof_coverage(self):
        for count in (399, 400, 401, 1009):
            with self.subTest(count=count):
                result = pipe_page(b"".join(line(i) for i in range(count)))
                self.assertTrue(result["ok"])
                self.assertEqual(result["counts"]["records"], count)
                self.assertEqual(result["counts"]["info"], count)
                self.assertNotIn("private-sentinel", json.dumps(result))

    def test_half_open_boundaries_and_identical_timestamps(self):
        data = line(0) + line(0) + line(1000)
        result = pipe_page(data, upper=START + coverage.NANO)
        self.assertEqual(result["counts"]["records"], 2)
        self.assertEqual(result["anchor"]["at"], str(START))

    def test_severity_is_a_prefix_not_a_word_in_an_info_message(self):
        data = line(0, "INFO service: ERROR and panicked at are quoted text")
        data += line(1, "ERROR service: lease_expired")
        data += (coverage.iso(START + 2000000) + " thread 'worker' panicked at fixture\n").encode()
        data += (coverage.iso(START + 3000000) + " \x1b[31munknown formatter\n").encode()
        result = pipe_page(data)
        self.assertTrue(result["ok"])
        self.assertEqual([result["counts"][key] for key in ("errors", "panics", "leasesExpired", "unclassified")], [1, 1, 1, 1])

    def test_incomplete_huge_and_invalid_records_never_get_coverage_credit(self):
        for data, reason in ((line(0)[:-1], "incomplete_record"), (line(0) + b"x" * 20000, "record_limit"),
                             (b"private-secret-error\n", "invalid_record"), (line(0), "byte_budget")):
            with self.subTest(reason=reason):
                result = pipe_page(data, bytes_limit=32 if reason == "byte_budget" else 512000)
                self.assertFalse(result["ok"])
                self.assertEqual(result["reason"], reason)
                self.assertNotIn("private-secret", json.dumps(result))

    def test_nonzero_exit_and_silent_pipe_do_not_claim_eof_success(self):
        for code, expected in (("import sys; sys.exit(3)", "source_exit"), ("import time; time.sleep(2)", "time_budget")):
            result = coverage.read_page([sys.executable, "-c", code], "docker", START, END, None, time.monotonic() + .15, 512000)
            self.assertFalse(result["ok"])
            self.assertEqual(result["reason"], expected)

    def test_journal_unit_and_kernel_decode_json_without_exporting_messages(self):
        data = json.dumps({"__REALTIME_TIMESTAMP": str(START // 1000), "__CURSOR": "s=fixture", "MESSAGE": "EXT4-fs error private"}).encode() + b"\n"
        result = pipe_page(data, kind="kernel")
        self.assertTrue(result["ok"])
        self.assertEqual(result["counts"]["kernelIO"], 1)
        hung = json.dumps({"__REALTIME_TIMESTAMP": str(START // 1000), "__CURSOR": "s=second", "MESSAGE": "task jbd2 blocked in I/O wait for more than122seconds"}).encode() + b"\n"
        self.assertEqual(pipe_page(hung, kind="kernel")["counts"]["hungTask"], 1)
        self.assertNotIn("EXT4", json.dumps(result))
        command = coverage.command_for({"kind": "journal", "unit": "fixture.service"}, IDENTITY, START, END)
        self.assertNotIn("-n", command)
        self.assertIn("--all", command)
        self.assertIn("--output-fields=__REALTIME_TIMESTAMP,__CURSOR,MESSAGE", command)
        null_message = json.dumps({"__REALTIME_TIMESTAMP": str(START // 1000), "__CURSOR": "s=null", "MESSAGE": None}).encode() + b"\n"
        self.assertFalse(pipe_page(null_message, kind="journal")["ok"])
        large = json.dumps({"__REALTIME_TIMESTAMP": str(START // 1000), "__CURSOR": "s=large", "MESSAGE": "2026-10-31T10:24:19Z ERROR service: " + "x" * 5000}).encode() + b"\n"
        self.assertEqual(pipe_page(large, kind="journal")["counts"]["errors"], 1)
        self.assertIn("--until", command)
        self.assertNotIn("--tail", coverage.command_for(request(), IDENTITY, START, END))
        kernel_command = coverage.command_for({"kind": "kernel"}, IDENTITY, START, END)
        self.assertIn("-k", kernel_command)
        self.assertNotIn("--kernel", kernel_command)

    def test_journal_diagnostics_preserve_exit_and_never_become_empty_success(self):
        for code, expected in ((1, "source_exit"), (0, "source_diagnostic")):
            program = f"import sys;sys.stderr.write('private-diagnostic');sys.exit({code})"
            result = coverage.read_page([sys.executable, "-c", program], "kernel", START, END, None, time.monotonic() + 2, 512000)
            self.assertFalse(result["ok"])
            self.assertEqual(result["exitCode"], code)
            self.assertEqual(result["reason"], expected)
            self.assertGreater(result["diagnosticBytes"], 0)
            self.assertNotIn("private-diagnostic", json.dumps(result))

    def test_nonstring_journal_messages_fail_closed_instead_of_becoming_zero_errors(self):
        for value in (None, [69, 82, 82, 79, 82], ["first", "second"], {"message": "ERROR"}):
            raw = json.dumps({"__REALTIME_TIMESTAMP": str(START // 1000), "__CURSOR": "s=fixture", "MESSAGE": value}).encode() + b"\n"
            result = pipe_page(raw, kind="journal")
            self.assertFalse(result["ok"])
            self.assertEqual(result["reason"], "invalid_record")

    def test_installed_journalctl_accepts_constructed_kernel_argv_and_bounds_empty_read(self):
        binary = shutil.which("journalctl")
        if binary is None:
            self.skipTest("journalctl CLI unavailable on this test host")
        help_result = subprocess.run([binary, "--help"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=2, check=True)
        self.assertIn(b"--dmesg", help_result.stdout)
        lower = time.time_ns() + 60 * coverage.NANO
        command = coverage.command_for({"kind": "kernel"}, IDENTITY, lower, lower + coverage.NANO)
        self.assertIn("-k", command)
        result = coverage.read_page(command, "kernel", lower, lower + coverage.NANO, None, time.monotonic() + 3, 512000)
        self.assertEqual(result["counts"]["records"], 0)
        # CI hosts need not expose a system journal. Their access diagnostics
        # still must fail closed; an accessible empty journal must succeed.
        if result["exitCode"] == 0 and result["diagnosticBytes"] == 0:
            self.assertTrue(result["ok"])
        else:
            self.assertFalse(result["ok"])
            self.assertIn(result["reason"], ("source_exit", "source_diagnostic"))

    def test_reduce_then_credit_only_oldest_completed_pages(self):
        calls = []
        def reader(command, kind, lower, upper, anchor, deadline, allowance):
            calls.append((lower, upper))
            ok = upper - lower <= 35 * coverage.NANO
            return {"ok": ok, "counts": empty_with_record(), "workBytes": 100, "eof": ok, "exitCode": 0 if ok else None,
                    "reason": None if ok else "byte_budget", "anchorFound": True,
                    "anchor": {"at": str(upper - 1), "key": str(upper)}}
        result = coverage.collect(request(maxAttempts=2), lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)
        self.assertEqual(result["coveredThrough"], coverage.iso(START + 35 * coverage.NANO))
        self.assertFalse(result["caughtUp"])
        self.assertEqual(result["pages"], 1)
        self.assertEqual(result["counts"]["records"], 1)
        self.assertEqual(result["partialCounts"]["records"], 1)
        self.assertEqual(calls[1][0], START)

    def test_repeated_dense_cycles_persist_retry_width_and_progress_past_overlap(self):
        cursor = START + 5 * coverage.NANO
        prior = None
        widths = []
        def dense_reader(command, kind, lower, upper, anchor, deadline, allowance):
            widths.append(upper - lower)
            needed = (upper - lower) * 120000 // coverage.NANO
            ok = needed <= allowance
            return {"ok": ok, "counts": empty_with_record(), "workBytes": min(needed, allowance + 1),
                    "eof": ok, "exitCode": 0 if ok else None, "reason": None if ok else "byte_budget",
                    "anchorFound": True, "anchor": {"at": str(upper - 1), "key": str(upper)}}
        for cycle in range(10):
            result = coverage.collect(request(since=coverage.iso(cursor - 5 * coverage.NANO), previous=prior),
                                      lambda *_: IDENTITY, dense_reader, now_ns=END + 3 * coverage.NANO)
            if cycle == 0:
                self.assertEqual(result["pages"], 0)
                self.assertLess(int(result["pageSpanNs"]), 70 * coverage.NANO)
            prior = {"identity": result["identity"], "anchor": result["anchor"], "pageSpanNs": result["pageSpanNs"]}
            cursor = max(cursor, coverage.timestamp(result["coveredThrough"]))
            if result["caughtUp"]:
                break
        self.assertEqual(cursor, END)
        self.assertLess(cycle, 9)
        self.assertEqual(widths.count(70 * coverage.NANO), 1)

    def test_dense_real_child_pipes_progress_on_next_tick_with_five_second_overlap(self):
        def dense_pipe(command, kind, lower, upper, anchor, deadline, allowance):
            read_since = min(lower, int(anchor["at"])) if anchor else lower
            start_index = max(0, (read_since - START + 9999999) // 10000000)
            program = "import os\n" + f"start={START};end={upper};index={start_index}\n" + """try:
 while start+index*10000000<end:
  at=start+index*10000000;sec,nano=divmod(at,1000000000)
  import datetime as d
  stamp=d.datetime.fromtimestamp(sec,d.timezone.utc).strftime('%Y-%m-%dT%H:%M:%S')+f'.{nano:09d}Z'
  os.write(1,(stamp+' 2026-10-31T10:24:19Z INFO fixture: '+'x'*1140+'\\n').encode());index+=1
except BrokenPipeError:pass
"""
            return coverage.read_page([sys.executable, "-c", program], kind, lower, upper, anchor, deadline, allowance)
        cursor = START + 5 * coverage.NANO
        prior = None
        for _ in range(2):
            result = coverage.collect(request(since=coverage.iso(cursor - 5 * coverage.NANO), previous=prior),
                                      lambda *_: IDENTITY, dense_pipe, now_ns=END + 3 * coverage.NANO)
            prior = {"identity": result["identity"], "anchor": result["anchor"], "pageSpanNs": result["pageSpanNs"]}
            cursor = max(cursor, coverage.timestamp(result["coveredThrough"]))
        self.assertGreater(cursor, START + 6 * coverage.NANO)

    def test_time_limited_page_reduction_survives_the_next_invocation(self):
        def reader(command, kind, lower, upper, anchor, deadline, allowance):
            ok = upper - lower <= 35 * coverage.NANO
            return {"ok": ok, "counts": coverage.empty_counts(), "workBytes": 0, "eof": ok, "exitCode": 0 if ok else None,
                    "reason": None if ok else "time_budget", "anchorFound": True, "anchor": None}
        first = coverage.collect(request(maxAttempts=1), lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)
        self.assertEqual(first["pages"], 0)
        second = coverage.collect(request(previous={"identity": IDENTITY, "pageSpanNs": first["pageSpanNs"]}),
                                  lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)
        self.assertTrue(second["caughtUp"])

    def test_reduction_at_the_minimum_always_returns_valid_persisted_state(self):
        def reader(*args):
            return {"ok": False, "counts": coverage.empty_counts(), "workBytes": 0, "eof": False,
                    "exitCode": None, "reason": "time_budget", "anchorFound": True, "anchor": None}
        prior = {"identity": IDENTITY, "pageSpanNs": "1068115"}
        first = coverage.collect(request(previous=prior), lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)
        self.assertEqual(first["pageSpanNs"], "1000000")
        coverage.collect(request(previous={"identity": IDENTITY, "pageSpanNs": first["pageSpanNs"]}),
                         lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)

    def test_future_overlap_anchor_is_preserved_until_a_completed_page_can_include_it(self):
        proof_at = START + 4 * coverage.NANO
        prior = {"identity": IDENTITY, "anchor": {"at": str(proof_at), "key": coverage.iso(proof_at) + ":stdout"}, "pageSpanNs": str(coverage.NANO)}
        def reader(command, kind, lower, upper, anchor, deadline, allowance):
            read_since = min(lower, int(anchor["at"])) if anchor else lower
            data = b"".join((coverage.iso(at) + " 2026-10-31T10:24:19Z INFO fixture: retained\n").encode()
                            for at in range(START, START + 7 * coverage.NANO, coverage.NANO) if read_since <= at <= upper)
            return pipe_page(data, lower=lower, upper=upper, anchor=anchor)
        first = coverage.collect(request(until=coverage.iso(START + 7 * coverage.NANO), previous=prior, maxAttempts=2),
                                 lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)
        self.assertEqual(first["continuity"], "anchor_pending")
        self.assertEqual(first["anchor"], prior["anchor"])
        second = coverage.collect(request(since=first["coveredThrough"], until=coverage.iso(START + 7 * coverage.NANO),
                                          previous={"identity": IDENTITY, "anchor": first["anchor"], "pageSpanNs": first["pageSpanNs"]}),
                                  lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)
        self.assertEqual(second["continuity"], "anchor_confirmed")
        self.assertTrue(second["complete"])

    def test_high_to_low_volume_recovers_backlog_against_a_moving_upper_bound(self):
        density = 120000
        def reader(command, kind, lower, upper, anchor, deadline, allowance):
            needed = (upper - lower) * density // coverage.NANO
            ok = needed <= allowance
            return {"ok": ok, "counts": coverage.empty_counts(), "workBytes": min(needed, allowance + 1),
                    "eof": ok, "exitCode": 0 if ok else None, "reason": None if ok else "byte_budget", "anchorFound": True,
                    "anchor": {"at": str(upper - 1), "key": str(upper)}}
        first = coverage.collect(request(), lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)
        prior = {"identity": IDENTITY, "anchor": first["anchor"], "pageSpanNs": first["pageSpanNs"]}
        cursor = START
        density = 1000
        lags = []
        for tick in range(5):
            end = START + (600 + tick * 60) * coverage.NANO
            result = coverage.collect(request(since=coverage.iso(cursor), until=coverage.iso(end), previous=prior),
                                      lambda *_: IDENTITY, reader, now_ns=end + 3 * coverage.NANO)
            cursor = coverage.timestamp(result["coveredThrough"])
            prior = {"identity": IDENTITY, "anchor": result["anchor"], "pageSpanNs": result["pageSpanNs"]}
            lags.append((end - cursor) // coverage.NANO)
            if result["caughtUp"]:
                break
        self.assertEqual(lags[-1], 0)
        self.assertLess(lags[1], lags[0])
        self.assertGreater(int(result["pageSpanNs"]), int(first["pageSpanNs"]))

    def test_source_clock_bootstrap_and_missing_anchor_are_explicit(self):
        def reader(*_):
            return {"ok": True, "counts": coverage.empty_counts(), "workBytes": 0, "eof": True,
                    "exitCode": 0, "reason": None, "anchorFound": False, "anchor": None}
        result = coverage.collect(request(), lambda *_: IDENTITY, reader, now_ns=END)
        self.assertFalse(result["caughtUp"])
        self.assertEqual(result["coveredThrough"], coverage.iso(END - 2 * coverage.NANO))
        prior = {"identity": IDENTITY, "anchor": {"at": str(START), "key": "old"}}
        result = coverage.collect(request(previous=prior), lambda *_: IDENTITY, reader, now_ns=END + 3 * coverage.NANO)
        self.assertEqual(result["continuity"], "anchor_missing")
        self.assertTrue(result["continuityUnknown"])

    def test_source_change_between_pages_discards_watermark(self):
        values = iter((IDENTITY, {**IDENTITY, "epoch": "2026-10-31T00:00:00Z"}))
        def reader(*_):
            return {"ok": True, "counts": coverage.empty_counts(), "workBytes": 0, "eof": True,
                    "exitCode": 0, "reason": None, "anchorFound": True, "anchor": None}
        result = coverage.collect(request(), lambda *_: next(values), reader, now_ns=END + 3 * coverage.NANO)
        self.assertTrue(result["sourceChanged"])
        self.assertEqual(result["coveredThrough"], coverage.iso(START))
        self.assertFalse(result["complete"])

    def test_cli_error_does_not_echo_request_or_exception_content(self):
        result = subprocess.run([sys.executable, str(SPEC.origin), "--request", '{"kind":"private-secret"}'], capture_output=True, check=True)
        self.assertEqual(json.loads(result.stdout), {"ok": False, "reason": "source_or_request_failure"})
        self.assertEqual(result.stderr, b"")


def empty_with_record():
    return {**coverage.empty_counts(), "records": 1}


if __name__ == "__main__":
    unittest.main()
