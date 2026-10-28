"""Guard the benchmark against stale binaries and misleading speedup reports."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "benchmark_blaze", Path(__file__).resolve().parents[1] / "benchmark-blaze.py"
)
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


class BlazeBenchmarkTests(unittest.TestCase):
    def test_edits_change_behavior_without_changing_the_function_signature(self):
        original = f"fn version() {{ {benchmark.VERSION_LINE} }}\n"
        changed = benchmark.edited_source(original, "-blaze-probe-2")
        self.assertIn('println!("cairn {}-blaze-probe-2",', changed)
        self.assertTrue(changed.startswith("fn version() {"))
        with self.assertRaises(ValueError):
            benchmark.edited_source(original * 2, "-probe")

    def test_fast_path_requires_the_finished_marker(self):
        self.assertTrue(benchmark.served_by_blaze("Finished `dev` profile [ra-jit] in 0.1s"))
        self.assertFalse(benchmark.served_by_blaze("[ra-jit] failed; building with cargo\nFinished `dev` profile in 1s"))

    def test_workspace_can_become_ready_after_three_minutes(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            status = f"daemon: running\n    {directory}: ready"
            with patch.object(benchmark.time, "monotonic", side_effect=[0, 181, 182]):
                with patch.object(benchmark, "command_output", return_value=status):
                    elapsed = benchmark.wait_ready({}, directory, directory)
            self.assertEqual(elapsed, 182)

    def test_oracle_rejects_a_successful_build_with_stale_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "binary"
            binary.write_text("#!/bin/sh\necho 'cairn 1.0'\n")
            binary.chmod(0o700)
            with self.assertRaisesRegex(ValueError, "Stale or incorrect binary"):
                benchmark.verify_binary(binary, "cairn 1.0-blaze-probe-1", os.environ)

    @unittest.skipUnless(Path("/usr/bin/time").is_file(), "GNU time is required")
    def test_measurement_keeps_failure_status_and_times_out_processes(self):
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "failure.log"
            failed = benchmark.measured(["sh", "-c", "echo compiler-error; exit 7"], os.environ, log)
            self.assertEqual(failed["exit_code"], 7)
            self.assertGreater(failed["wall_seconds"], 0)
            self.assertIn("compiler-error", log.read_text())
            self.assertIn("client_user_seconds", failed)
            timed_out = benchmark.measured(["sleep", "10"], os.environ, Path(temporary) / "timeout.log", timeout=0.1)
            self.assertEqual(timed_out["exit_code"], 124)

    def test_failure_restores_source_and_preserves_diagnostics(self):
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary)
            source = repo / benchmark.SOURCE
            source.parent.mkdir(parents=True)
            original = benchmark.VERSION_LINE + "\n"
            source.write_text(original)
            (source.parent.parent / "Cargo.toml").write_text('[package]\nversion = "1.0"\n')
            output = repo / "results"
            output.mkdir()

            def failing_build(command, env, log):
                # Simulate a failed build after the source has been edited.
                source.write_text("temporary edit")
                log.write_text("compiler diagnostics\n")
                return {"exit_code": 1, "wall_seconds": 1, "log": log.name}

            with patch.object(benchmark, "command_output", return_value="rustc test"):
                with patch.object(benchmark, "measured", side_effect=failing_build):
                    result = benchmark.run_engine(repo, output, "cargo-ci", "1.97.1", False)
            self.assertEqual(source.read_text(), original)
            self.assertEqual(result["status"], "failed")
            self.assertTrue((output / "cargo-ci/result.json").is_file())
            self.assertIn("compiler diagnostics", (output / "cargo-ci/cold.log").read_text())

    def test_speedup_requires_three_verified_fast_path_edits(self):
        rows = [
            {"phase": f"edit-{i}", "wall_seconds": i, "blaze_fast_path": True, "verified_output": "changed"}
            for i in range(1, 4)
        ]
        cargo = {"name": "cargo-matched", "status": "completed", "rows": rows}
        blaze = {"name": "blaze", "status": "completed", "rows": [dict(row, wall_seconds=0.5) for row in rows]}
        self.assertIn("4.00× speedup", benchmark.summary([cargo, blaze], {"commit": "test"}))
        blaze["rows"][1]["blaze_fast_path"] = False
        self.assertIn("No demonstrated Blaze speedup", benchmark.summary([cargo, blaze], {"commit": "test"}))
        blaze["rows"][1]["blaze_fast_path"] = True
        del blaze["rows"][1]["verified_output"]
        self.assertIn("No demonstrated Blaze speedup", benchmark.summary([cargo, blaze], {"commit": "test"}))


if __name__ == "__main__":
    unittest.main()
