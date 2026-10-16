"""Private fixture tests for cache selection and safe discard estimation.

uv run --python 3.11 --with blake3==1.0.8 python scripts/tests/storage_sample_test.py
"""

import hashlib
import importlib.util
import io
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

import blake3

SCRIPTS = Path(__file__).resolve().parents[1]


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


sampler = module("cache_sampler", "sample-storage-cache.py")
prepare = module("sample_prepare", "prepare-storage-sample.py")


class SampleTests(unittest.TestCase):
    def test_second_phase_preserves_available_disks_after_journal_disappears(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            live = source / "environments/live/lazy"
            cache = live / "cache"
            cache.mkdir(parents=True)
            data = b"live" * 1024
            identity = hashlib.sha256(data).hexdigest()
            (cache / identity).write_bytes(data)
            with sqlite3.connect(live / "journal-v2.sqlite") as db:
                db.execute(
                    "CREATE TABLE state (id INTEGER, manifest TEXT, generation INTEGER)"
                )
                db.execute(
                    "INSERT INTO state VALUES (1, ?, 1)",
                    (json.dumps({"size": len(data), "blocks": [{"hash": identity}]}),),
                )
            output = root / "sample"
            output.mkdir()
            selection = [
                {
                    "label": "disk-00",
                    "directory": str(source / "environments/deleted/lazy"),
                    "sampleIndices": [0],
                },
                {"label": "disk-01", "directory": str(live), "sampleIndices": [0]},
            ]
            (output / "selection.private.json").write_text(json.dumps(selection))
            argv = [
                "sample-storage-cache.py",
                "--root",
                str(source),
                "--output",
                str(output),
                "--phase",
                "second",
                "--metadata-disks",
                "0",
            ]
            captured_stdout = io.StringIO()
            with patch.object(sys, "argv", argv), redirect_stdout(captured_stdout):
                sampler.main()
            captured = json.loads((output / "second.private.json").read_text())
            self.assertEqual([disk["label"] for disk in captured["disks"]], ["disk-01"])
            self.assertEqual(captured["disks"][0]["sampleIndices"], [0])
            self.assertEqual(json.loads(captured_stdout.getvalue())["skippedDisks"], 1)

    def test_second_phase_fails_clearly_if_all_selected_disks_disappear(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            output = root / "sample"
            output.mkdir()
            (output / "selection.private.json").write_text(
                json.dumps(
                    [
                        {
                            "label": "disk-00",
                            "directory": str(source / "environments/deleted/lazy"),
                            "sampleIndices": [0],
                        }
                    ]
                )
            )
            argv = [
                "sample-storage-cache.py",
                "--root",
                str(source),
                "--output",
                str(output),
                "--phase",
                "second",
                "--metadata-disks",
                "0",
            ]
            errors = io.StringIO()
            with (
                patch.object(sys, "argv", argv),
                redirect_stderr(errors),
                self.assertRaises(SystemExit) as stopped,
            ):
                sampler.main()
            self.assertEqual(stopped.exception.code, 2)
            self.assertIn(
                "No selected published cache regions remain available",
                errors.getvalue(),
            )
            self.assertFalse((output / "second.private.json").exists())

    def test_discard_cohort_keeps_unknown_owners(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "objects").mkdir()
            disks = []
            for index in range(2):
                data = bytes([index + 1]) * 4096
                identity = hashlib.sha256(data).hexdigest()
                (root / "objects" / identity).write_bytes(data)
                disks.append(
                    {
                        "label": f"disk-{index:02d}",
                        "size": len(data),
                        "sampleIndices": [0],
                        "capturedAt": 1,
                        "blocks": {
                            "0": {
                                "hash": identity,
                                "size": len(data),
                                "sha256": identity,
                            }
                        },
                    }
                )
            for phase in ("first", "second"):
                (root / f"{phase}.private.json").write_text(
                    json.dumps({"disks": disks})
                )
            output = root / "output"
            output.mkdir()
            valid = {"disk": "disk-00", "before": "replayed", "after": "trimmed"}
            with patch.object(
                prepare,
                "discard_sample",
                side_effect=[(valid, None), (None, "unknown")],
            ):
                prepare.prepare(root, output)
            cohort = json.loads((output / "discard-cohort.private.json").read_text())
            self.assertEqual(
                [entry["disk"] for entry in cohort["generations"][0]],
                ["disk-00", "disk-01"],
            )
            self.assertEqual(cohort["generations"][0][0]["path"], "replayed")
            self.assertTrue(
                cohort["generations"][0][1]["path"].endswith("disk-01-g0.private.bin")
            )
            self.assertEqual(
                cohort["trim_generations"], [{"disk": "disk-00", "path": "trimmed"}]
            )

    def test_repeated_cache_hash_keeps_all_manifest_positions_and_respects_budget(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source/cache"
            source.mkdir(parents=True)
            output = root / "output/objects"
            output.mkdir(parents=True)
            data = b"x" * sampler.REGION
            identity = "b3-" + blake3.blake3(data).hexdigest()
            (source / identity).write_bytes(data)
            value = {
                "size": 2 * sampler.REGION,
                "blocks": [{"hash": identity}, {"hash": identity}],
            }
            reader = sampler.Reader(
                source.parent, value, output.parent, [0, sampler.REGION]
            )
            self.assertEqual(reader.region(0), data)
            self.assertEqual(reader.region(1), data)
            self.assertEqual(set(reader.records), {"0", "1"})
            (source / identity).unlink()
            (source / identity).symlink_to(root / "output/objects" / identity)
            reader = sampler.Reader(
                source.parent, value, output.parent, [0, 2 * sampler.REGION]
            )
            with self.assertRaises(OSError):
                reader.region(0)

    def test_verification_rejects_path_escape_and_corrupt_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "objects").mkdir()
            data = b"private-fixture"
            identity = hashlib.sha256(data).hexdigest()
            (root / "objects" / identity).write_bytes(data)
            disk = {
                "label": "disk-00",
                "size": len(data),
                "blocks": {
                    "0": {"hash": identity, "size": len(data), "sha256": identity}
                },
            }
            self.assertEqual(prepare.verify(root, [{"disks": [disk]}]), 1)
            disk["label"] = "../escape"
            with self.assertRaises(ValueError):
                prepare.verify(root, [{"disks": [disk]}])
            disk["label"] = "disk-00"
            (root / "objects" / identity).write_bytes(b"wrong")
            with self.assertRaises(ValueError):
                prepare.verify(root, [{"disks": [disk]}])

    def test_discard_on_ext4_copy_preserves_source_and_allocated_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tree = root / "tree"
            tree.mkdir()
            (tree / "deleted.bin").write_bytes(os.urandom(2 * 1024 * 1024))
            (tree / "kept.bin").write_bytes(os.urandom(1024 * 1024))
            image = root / "source.ext4"
            with image.open("wb") as stream:
                stream.truncate(64 * 1024 * 1024)
            subprocess.run(
                [
                    "/usr/sbin/mkfs.ext4",
                    "-q",
                    "-F",
                    "-b",
                    "4096",
                    "-E",
                    "lazy_itable_init=0,lazy_journal_init=0",
                    "-d",
                    str(tree),
                    str(image),
                ],
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["/usr/sbin/debugfs", "-w", "-R", "rm /deleted.bin", str(image)],
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["/usr/sbin/debugfs", "-w", "-R", "feature needs_recovery", str(image)],
                check=True,
                capture_output=True,
            )
            source_digest = hashlib.sha256(image.read_bytes()).hexdigest()
            cache = root / "lazy/cache"
            cache.mkdir(parents=True)
            corpus = root / "corpus"
            (corpus / "objects").mkdir(parents=True)
            blocks = []
            with image.open("rb") as stream:
                while data := stream.read(sampler.REGION):
                    identity = (
                        "b3-" + blake3.blake3(data).hexdigest() if any(data) else None
                    )
                    blocks.append({"hash": identity})
                    if identity:
                        (cache / identity).write_bytes(data)
            manifest = {"size": image.stat().st_size, "blocks": blocks}
            reader = sampler.Reader(
                cache.parent, manifest, corpus, [0, 128 * 1024 * 1024]
            )
            for index in range(len(blocks)):
                reader.region(index)
            metadata = sampler.ext4_metadata(reader)
            self.assertTrue(metadata["needsRecovery"])
            disk = {
                "label": "disk-00",
                "size": image.stat().st_size,
                "blocks": reader.records,
                "ext4": metadata,
            }
            prepare.verify(corpus, [{"disks": [disk]}])
            output = root / "output"
            output.mkdir()
            result, reason = prepare.discard_sample(
                corpus, disk, list(range(len(blocks))), output
            )
            self.assertIsNone(reason)
            self.assertTrue(result["allocatedPagesUnchanged"])
            self.assertGreater(result["nonzeroBytesCleared"], 1024 * 1024)
            self.assertEqual(
                hashlib.sha256(image.read_bytes()).hexdigest(), source_digest
            )
            before = Path(result["before"]).read_bytes()
            after = Path(result["after"]).read_bytes()
            self.assertEqual(before[:4096], after[:4096])
            restored = subprocess.run(
                ["/usr/sbin/debugfs", "-R", "cat /kept.bin", result["after"]],
                check=True,
                capture_output=True,
            )
            self.assertEqual(restored.stdout, (tree / "kept.bin").read_bytes())
            self.assertFalse(list(output.glob("*.ext4")))


if __name__ == "__main__":
    unittest.main()
