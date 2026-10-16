"""Isolated tests: no Docker daemon, production files or agent credentials."""

import datetime as dt
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SOURCE = (
    Path(__file__).resolve().parents[2] / "deploy/installations/storage-observer.py"
)
SPEC = importlib.util.spec_from_file_location("storage_observer", SOURCE)
observer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(observer)


def metrics(count=3):
    return {
        "blockCodec": {
            "encode": {
                "count": count,
                "bytes": count * 4096,
                "totalMicros": count * 10,
                "maxMicros": 20,
                "inFlight": 1,
                "errors": 0,
            },
            "encodedBytes": count * 1024,
            "zstdWrites": count,
            "rawWrites": 0,
        },
        "get": {"count": count, "bytes": count * 1024, "errors": 0},
    }


class ObserverTests(unittest.TestCase):
    def test_fresh_counter_window_and_reset_are_explicit(self):
        metadata = {
            "Id": "a" * 64,
            "State": {"StartedAt": "same", "Status": "running"},
            "Config": {"Image": "image"},
        }
        previous = {
            "manager": {"container": "a" * 12, "startedAt": "same"},
            "metrics": metrics(3),
            "metricsAt": "2026-10-16T10:00:00Z",
            "observedAt": "2026-10-16T10:00:00+00:00",
        }
        for count in (4, 1):
            with self.subTest(count=count), tempfile.TemporaryDirectory() as temporary:

                def fake(args, sample_count=count, **kwargs):
                    if args[1] == "inspect":
                        return json.dumps(metadata)
                    if args[1] == "logs":
                        self.assertEqual(args[-1], "a" * 64)
                        return (
                            '2026-10-16T10:05:00Z operation="s3_totals" metrics='
                            + json.dumps(metrics(sample_count))
                        )
                    return ""

                with (
                    patch.object(observer, "command", side_effect=fake),
                    patch.object(observer, "manager_cpu", return_value=None),
                ):
                    result = observer.collect(
                        Path(temporary), "manager", "garage", "bucket", previous
                    )
                self.assertEqual(result["counterResetDetected"], count < 3)
                if count == 4:
                    self.assertEqual(
                        result["metricDelta"]["blockCodec"]["encode"]["count"], 1
                    )
                    self.assertEqual(
                        result["metricDeltaWindow"],
                        {"from": previous["metricsAt"], "to": "2026-10-16T10:05:00Z"},
                    )
                else:
                    self.assertNotIn("metricDelta", result)

    def test_log_recovers_checkpoint_after_crash_between_append_and_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            old = {"schema": 1, "sequence": 1, "observedAt": "2026-10-16T00:00:00Z"}
            new = {"schema": 1, "sequence": 2, "observedAt": "2026-10-16T00:05:00Z"}
            observer.append_sample(root, old)
            with (
                patch.object(observer, "atomic_json", side_effect=OSError("crash")),
                self.assertRaises(OSError),
            ):
                observer.append_sample(root, new)
            self.assertEqual(json.loads((root / "state.json").read_text()), old)
            with next(root.glob("*.jsonl")).open("ab") as stream:
                stream.write(b'{"partial":')
            self.assertEqual(observer.previous_sample(root), new)

    def test_only_allowlisted_numeric_fields_survive(self):
        value = metrics()
        value["credential"] = "must-never-be-stored"
        value["blockCodec"]["encode"].update(
            token="must-never-be-stored", bytes="secret", errors=True
        )
        value["blockCodec"]["rawWrites"] = -1
        self.assertIsNone(observer.clean_metrics(value))
        value["blockCodec"]["encode"]["bytes"] = 12288
        clean = observer.clean_metrics(value)
        self.assertNotIn("must-never-be-stored", json.dumps(clean))
        self.assertNotIn("errors", clean["blockCodec"]["encode"])
        self.assertNotIn("rawWrites", clean["blockCodec"])

    def test_json_and_text_logs_keep_latest_valid_snapshot_only(self):
        first = "2026-10-16T10:00:00Z "
        last = "2026-10-16T10:05:00Z "
        text = (
            first
            + 'INFO operation="s3_totals" id="private-run" metrics='
            + json.dumps(metrics(2))
        )
        structured = last + json.dumps(
            {
                "fields": {
                    "operation": "s3_totals",
                    "id": "private-run",
                    "metrics": json.dumps(metrics(4)),
                }
            }
        )
        result = observer.latest_metrics(
            text
            + "\n"
            + structured
            + "\n"
            + last
            + "operation=s3_totals metrics=broken"
        )
        self.assertEqual(result[0], last.strip())
        self.assertEqual(result[1]["blockCodec"]["encode"]["count"], 4)
        self.assertNotIn("private-run", json.dumps(result))

    def test_counter_deltas_do_not_subtract_gauges_or_maxima(self):
        delta = observer.deltas(metrics(4), metrics(3))
        self.assertEqual(delta["blockCodec"]["encodedBytes"], 1024)
        self.assertEqual(delta["blockCodec"]["encode"]["count"], 1)
        self.assertNotIn("inFlight", delta["blockCodec"]["encode"])
        self.assertNotIn("maxMicros", delta["blockCodec"]["encode"])

    def test_collection_changes_epoch_and_does_not_carry_old_metrics(self):
        metadata = [
            {
                "Id": "a" * 64,
                "State": {"StartedAt": "new", "Status": "running"},
                "Config": {"Image": "image"},
            }
        ]
        old = {
            "manager": {"container": "a" * 12, "startedAt": "old"},
            "metrics": metrics(99),
            "metricsAt": "2026-10-16T00:00:00Z",
        }
        with tempfile.TemporaryDirectory() as temporary:

            def fake(args, **kwargs):
                return json.dumps(metadata) if args[1] == "inspect" else ""

            with (
                patch.object(observer, "command", side_effect=fake),
                patch.object(observer, "manager_cpu", return_value=None),
            ):
                result = observer.collect(
                    Path(temporary), "manager", "garage", "bucket", old
                )
        self.assertTrue(result["counterEpochChanged"])
        self.assertNotIn("metrics", result)
        self.assertNotIn("metricDelta", result)

    def test_collection_keeps_explicitly_stale_metrics_and_cpu_interval(self):
        now = dt.datetime.now(dt.timezone.utc)
        old_time = (now - dt.timedelta(seconds=300)).isoformat()
        previous = {
            "manager": {"container": "a" * 12, "startedAt": "same"},
            "metrics": metrics(),
            "metricsAt": "2020-01-01T00:00:00Z",
            "observedAt": old_time,
            "processCpu": {"startTicks": 1, "seconds": 10},
        }
        metadata = [
            {
                "Id": "a" * 64,
                "State": {"StartedAt": "same", "Status": "running"},
                "Config": {"Image": "image"},
            }
        ]
        with tempfile.TemporaryDirectory() as temporary:

            def fake(args, **kwargs):
                return json.dumps(metadata) if args[1] == "inspect" else ""

            with (
                patch.object(observer, "command", side_effect=fake),
                patch.object(
                    observer,
                    "manager_cpu",
                    return_value={"startTicks": 1, "seconds": 40},
                ),
            ):
                result = observer.collect(
                    Path(temporary), "manager", "garage", "bucket", previous
                )
        self.assertGreater(result["metricsAgeSeconds"], 600)
        self.assertAlmostEqual(result["managerCpuCoresDuringInterval"], 0.1, places=3)
        self.assertNotIn("metricDelta", result)

    def test_failed_or_oversized_command_returns_no_partial_output(self):
        self.assertIsNone(
            observer.command([sys.executable, "-c", 'print("x"*10000)'], limit=100)
        )
        self.assertIsNone(
            observer.command(
                [sys.executable, "-c", "import time;time.sleep(1)"], timeout=0.05
            )
        )
        self.assertIsNone(
            observer.command([sys.executable, "-c", "raise SystemExit(1)"])
        )

    def test_durable_append_recovers_partial_tail_and_updates_checkpoint(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            now = dt.datetime(2026, 10, 16, tzinfo=dt.timezone.utc)
            observer.append_sample(root, {"n": 1}, now=now)
            path = next(root.glob("*.jsonl"))
            with path.open("ab") as stream:
                stream.write(b'{"n":')
            observer.append_sample(root, {"n": 2}, now=now)
            self.assertEqual(
                [json.loads(line) for line in path.read_text().splitlines()],
                [{"n": 1}, {"n": 2}],
            )
            self.assertEqual(json.loads((root / "state.json").read_text()), {"n": 2})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_daily_retention_and_byte_quota_remove_only_own_closed_segments(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            now = dt.datetime(2026, 10, 16, tzinfo=dt.timezone.utc)
            foreign = root / "foreign.jsonl"
            foreign.write_text("preserve")
            for day in range(4):
                for n in range(4):
                    observer.append_sample(
                        root,
                        {"n": n, "padding": "x" * 30},
                        now=now + dt.timedelta(days=day),
                        retention_days=1,
                        max_bytes=250,
                        segment_bytes=125,
                    )
            own = [p for p in root.glob("*.jsonl") if p != foreign]
            self.assertLessEqual(sum(p.stat().st_size for p in own), 250)
            self.assertTrue(all(p.name[:10] >= "2026-10-18" for p in own))
            self.assertEqual(foreign.read_text(), "preserve")

    def test_append_refuses_symlink_destination(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "foreign"
            target.write_text("preserve")
            (root / "2026-10-16.0000.jsonl").symlink_to(target)
            with self.assertRaises(OSError):
                observer.append_sample(
                    root,
                    {"n": 1},
                    now=dt.datetime(2026, 10, 16, tzinfo=dt.timezone.utc),
                )
            self.assertEqual(target.read_text(), "preserve")


if __name__ == "__main__":
    os.umask(0o077)
    unittest.main()
