#!/usr/bin/env python3
"""Guest maintenance must cancel safely and never infer discarded allocation."""
import importlib.util
from pathlib import Path
import signal
import subprocess
import unittest
from unittest.mock import Mock, patch

SPEC = importlib.util.spec_from_file_location("guest_trim", Path(__file__).resolve().parents[2] / "deploy/microvm/trim.py")
trim = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(trim)


class GuestTrimTests(unittest.TestCase):
    def test_targets_only_exact_mounted_ext4_views(self):
        mounts = "\n".join([
            "1 0 0:1 / / rw - overlay overlay rw",
            "2 1 254:16 / /oldroot/run/data rw - ext4 /dev/vdb rw",
            "3 1 0:2 / /data rw - tmpfs tmpfs rw",
            "4 2 254:16 / /data/subdir rw - ext4 /dev/vdb rw",
            "broken",
        ])
        self.assertEqual(trim.targets(mounts), [trim.TARGETS[0]])
        mounts += "\n5 1 254:32 / /data rw - ext4 /dev/vdc rw"
        self.assertEqual(trim.targets(mounts), list(trim.TARGETS))

    def test_late_workspace_mount_is_trimmed_without_waiting_for_the_system_interval(self):
        due = {"/oldroot/run/data": 1800}
        self.assertEqual(trim.ready_targets(list(trim.TARGETS), due, 20), ["/data"])
        due["/data"] = 1820
        self.assertEqual(trim.ready_targets(list(trim.TARGETS), due, 30), [])
        self.assertEqual(trim.ready_targets([trim.TARGETS[0]], due, 30), [])
        self.assertNotIn("/data", due)
        self.assertEqual(trim.ready_targets(list(trim.TARGETS), due, 40), ["/data"])
        self.assertEqual(trim.ready_targets(list(trim.TARGETS), due, 1800), list(trim.TARGETS))

    def test_completed_trim_uses_fixed_argv_and_a_private_process_group(self):
        process = Mock(pid=123)
        process.poll.return_value = 0
        with patch.object(trim.subprocess, "Popen", return_value=process) as popen:
            result = trim.trim("/data", lambda: False)
        self.assertTrue(result["invocationSucceeded"])
        args, kwargs = popen.call_args
        self.assertEqual(args[0], [trim.BUSYBOX, "fstrim", "-m", "65536", "/data"])
        self.assertTrue(kwargs["start_new_session"])
        self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
        self.assertEqual(set(result), {"operation", "mount", "invocationSucceeded", "elapsedMs"})
        process.wait.assert_not_called()

    def test_cancellation_terminates_and_reaps_only_our_child_group(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        with patch.object(trim.subprocess, "Popen", return_value=process), patch.object(trim.os, "killpg") as kill:
            result = trim.trim("/data", lambda: True)
        self.assertFalse(result["invocationSucceeded"])
        kill.assert_called_once_with(123, signal.SIGTERM)
        process.wait.assert_called_once_with(timeout=2)

    def test_timeout_escalates_and_reaps_a_stuck_child(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired("fixture", 2), 0]
        with patch.object(trim.subprocess, "Popen", return_value=process), patch.object(trim.os, "killpg") as kill, patch.object(trim.time, "monotonic", side_effect=[0, 121, 122]):
            result = trim.trim("/data", lambda: False)
        self.assertFalse(result["invocationSucceeded"])
        self.assertEqual([call.args for call in kill.call_args_list], [(123, signal.SIGTERM), (123, signal.SIGKILL)])
        self.assertEqual(process.wait.call_count, 2)

    def test_missing_tool_is_reported_without_child_diagnostics(self):
        with patch.object(trim.subprocess, "Popen", side_effect=OSError("private diagnostic")):
            result = trim.trim("/data", lambda: False)
        self.assertFalse(result["invocationSucceeded"])
        self.assertNotIn("private", str(result))

    def test_unknown_mount_is_rejected_before_spawning_any_process(self):
        with patch.object(trim.subprocess, "Popen") as popen:
            with self.assertRaises(ValueError):
                trim.trim("/data/user-controlled", lambda: False)
        popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
