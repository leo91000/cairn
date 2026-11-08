import importlib.util
import json
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest


spec = importlib.util.spec_from_file_location(
    "collector", Path(__file__).with_name("backend-tail-fingerprints.py"))
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)


def fingerprint():
    return {
        "rustc": 18446744073709551615,
        "target": 3, "profile": 4, "path": 5, "config": 6, "compile_kind": 0,
        "features": '["worker-fixture-hooks"]', "declared_features": "[]",
        "deps": [[18446744073709551614, "axum", False, 18446744073709551613]],
        "rustflags": [], "unknown": "private-sentinel",
    }


class ObservationTests(unittest.TestCase):
    def test_non_object_json_roots_keep_closed_failure_receipts(self):
        for value in [[], None, 'private-sentinel']:
            with self.subTest(value=value), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                directory = root / 'fixture-1234'
                directory.mkdir()
                (directory / 'lib-fixture.json').write_text(json.dumps(value))
                output = root / 'receipt.json'
                result = subprocess.run([sys.executable, str(Path(__file__).with_name('backend-tail-fingerprints.py')),
                                         str(root), str(output)], capture_output=True, text=True)
                self.assertEqual(result.returncode, 1)
                receipt = json.loads(output.read_text())
                self.assertEqual(receipt['error']['reason'], 'invalid_fingerprint_object')
                self.assertEqual(receipt['error']['stage'], 'json_projection')
                self.assertNotIn('private-sentinel', json.dumps(receipt) + result.stderr)
                self.assertNotIn('Traceback', result.stderr)

    def test_failed_snapshot_retains_closed_receipt_and_nonzero_exit(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / 'private-sentinel-1234'
            directory.mkdir()
            target = directory / 'lib-private-sentinel.json'
            target.write_text('private-sentinel malformed json')
            output = root / 'receipt.json'
            result = subprocess.run([sys.executable, str(Path(__file__).with_name('backend-tail-fingerprints.py')),
                                     str(root), str(output)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            receipt = json.loads(output.read_text())
            self.assertFalse(receipt['complete'])
            self.assertEqual(receipt['error']['stage'], 'json_projection')
            self.assertEqual(receipt['error']['reason'], 'invalid_json')
            self.assertEqual(len(receipt['error']['identitySha256']), 64)
            self.assertNotIn('private-sentinel', json.dumps(receipt) + result.stderr)
            self.assertNotIn(str(root), json.dumps(receipt) + result.stderr)

    def test_accepts_synthetic_build_script_features(self):
        source = fingerprint()
        source["features"] = ""
        source["declared_features"] = ""
        result = collector.project(json.dumps(source).encode())
        self.assertEqual(result["features"], [])
        self.assertEqual(result["declaredFeatures"], [])

    def test_preserves_hashes_without_arbitrary_strings(self):
        source = fingerprint()
        source["rustflags"] = ["private-sentinel"]
        result = collector.project(json.dumps(source).encode())
        self.assertEqual(result["rustc"], "18446744073709551615")
        self.assertEqual(result["deps"][0][3], "18446744073709551613")
        self.assertEqual(result["features"], ["worker-fixture-hooks"])
        self.assertFalse(result["rustflagsEmpty"])
        self.assertNotIn("private-sentinel", json.dumps(result))

    def test_snapshots_cargo_marker_and_feature_change(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / "cairn-installation-1234abcd"
            directory.mkdir()
            target = directory / "test-lib-cairn_installation.json"
            source = fingerprint()
            target.write_text(json.dumps(source))
            target.with_suffix("").write_text("0123456789abcdef")
            before = collector.snapshot(root)
            source["features"] = "[]"
            target.write_text(json.dumps(source))
            after = collector.snapshot(root)
            key = f"{directory.name}/{target.name}"
            self.assertNotEqual(before["fingerprints"][key], after["fingerprints"][key])
            self.assertEqual(after["fingerprints"][key]["cargoFingerprint"], "0123456789abcdef")
            self.assertFalse(after["qualification"])

    def test_refuses_symlinks_and_large_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "large"
            target.write_bytes(b"x" * (collector.MAX_BYTES + 1))
            with self.assertRaises(ValueError):
                collector.read_regular(target)
            link = root / "link"
            link.symlink_to(target)
            with self.assertRaises(OSError):
                collector.read_regular(link)

    def test_refuses_arbitrary_feature_and_dependency_strings(self):
        for field, value in (("features", '["private sentinel\\n"]'),
                             ("deps", [[1, "private sentinel", False, 2]])):
            source = fingerprint()
            source[field] = value
            with self.assertRaises(ValueError):
                collector.project(json.dumps(source).encode())


if __name__ == "__main__":
    unittest.main()
