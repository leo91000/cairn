"""Integrity and accounting checks for the storage-format experiment.

uv run --with cryptography==46.0.3 --with fastcdc==1.7.0 --with zstandard==0.25.0 \
    --python 3.11 python scripts/tests/storage_formats_test.py
"""

import importlib.util
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

from cryptography.exceptions import InvalidTag

spec = importlib.util.spec_from_file_location(
    "storage_efficiency",
    Path(__file__).resolve().parents[1] / "benchmark-storage-formats.py",
)
bench = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = bench
spec.loader.exec_module(bench)


class StorageEfficiencyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def image(self, name, data):
        path = self.root / name
        path.write_bytes(data)
        return path

    def store(self, policy):
        return bench.Store(self.root / policy.name, policy)

    def test_cli_rejects_unsafe_duplicate_or_unknown_discard_owners(self):
        source = self.image("safe.bin", b"x" * 8192)
        entry = {"disk": "safe", "path": str(source)}
        victim = self.root / "escaped.metadata"
        victim.write_bytes(b"preserve-me")
        invalid = [
            [{**entry, "disk": "../../../escaped"}],
            [{**entry, "disk": "index"}],
            [{**entry, "disk": "unknown-owner"}],
            [entry, entry],
        ]
        for number, discard in enumerate(invalid):
            with self.subTest(discard=discard):
                descriptor = self.root / f"corpus-{number}.json"
                descriptor.write_text(
                    json.dumps(
                        {
                            "label": "safe",
                            "generations": [[entry]],
                            "trim_generations": discard,
                        }
                    )
                )
                output = self.root / f"results-{number}"
                argv = [
                    "benchmark-storage-formats.py",
                    "--output",
                    str(output),
                    "--corpus",
                    str(descriptor),
                    "--policies",
                    "current-v2",
                ]
                with (
                    patch.object(sys, "argv", argv),
                    redirect_stderr(io.StringIO()),
                    redirect_stdout(io.StringIO()),
                    self.assertRaises(SystemExit) as stopped,
                ):
                    bench.main()
                self.assertEqual(stopped.exception.code, 2)
                self.assertEqual(victim.read_bytes(), b"preserve-me")
                self.assertFalse((output / "results.json").exists())

    def test_cli_discard_subset_keeps_shared_bytes_owned_by_other_disks(self):
        shared = self.image("shared.bin", bench.noise("shared-cli", 8192))
        trimmed = self.image("trimmed.bin", bytes(8192))
        descriptor = self.root / "valid-corpus.json"
        descriptor.write_text(
            json.dumps(
                {
                    "label": "subset",
                    "generations": [
                        [{"disk": disk, "path": str(shared)} for disk in ("a", "b")]
                    ],
                    "trim_generations": [{"disk": "a", "path": str(trimmed)}],
                }
            )
        )
        output = self.root / "valid-results"
        argv = [
            "benchmark-storage-formats.py",
            "--output",
            str(output),
            "--corpus",
            str(descriptor),
            "--policies",
            "current-v2",
        ]
        with patch.object(sys, "argv", argv), redirect_stdout(io.StringIO()):
            bench.main()
        result = json.loads((output / "results.json").read_text())["corpora"][0][
            "results"
        ][0]
        self.assertEqual(result["trim"]["data_bytes"], result["data_bytes"])
        self.assertEqual(result["trim"]["unique_chunks"], 1)
        self.assertEqual(len(result["owner_deletion_bytes"]), 2)

    def test_current_v2_data_overhead_and_identical_probe_workload(self):
        data = bench.noise("raw-overhead", 128 * bench.KIB + 317)
        path = self.image("raw.bin", data)
        fingerprints = []
        for size in (bench.REGION, 64 * bench.KIB):
            policy = bench.Policy(f"probe-{size}", size, packed=False)
            store = self.store(policy)
            store.publish("same-source", path)
            for record in store.records.values():
                self.assertEqual(record.codec, "raw")
                self.assertEqual(len(record.payload), record.size + 41)
            probes = store.probes({"same-source": path}, count=8)
            fingerprints.append(probes["probe_positions_fingerprint"])
            self.assertEqual(probes["count"], 8)
            self.assertGreater(probes["process_cpu_ms_p50"], 0)
            self.assertGreater(probes["warm_local_ms_p50"], 0)
        self.assertEqual(fingerprints[0], fingerprints[1])

    def test_all_formats_restore_zeros_duplicates_and_short_tail(self):
        data = bytes(bench.REGION) + bench.noise("mixed", 128 * bench.KIB + 317)
        path = self.image("mixed.bin", data)
        for policy in bench.POLICIES:
            with self.subTest(policy=policy.name):
                store = self.store(policy)
                store.publish("a", path)
                count = len(store.records)
                uploaded = store.uploaded_bytes
                store.publish("b", path)
                self.assertEqual(len(store.records), count)
                self.assertEqual(store.uploaded_bytes, uploaded)
                store.verify("a", path)
                store.verify("b", path)
                store.probes({"a": path, "b": path})

    def test_dictionary_delta_handles_shift_and_bounds_dependencies(self):
        policy = bench.Policy("shift-delta", 64 * bench.KIB, delta=True)
        store = self.store(policy)
        base = bench.noise("shift", policy.chunk_bytes)
        insertion = b"inserted-byte-range"
        changed = base[:4096] + insertion + base[4096 : -len(insertion)]
        self.assertEqual(len(changed), len(base))
        first = self.image("base.bin", base)
        second = self.image("changed.bin", changed)
        store.publish("a", first)
        store.publish("b", second)
        record = store.records[bench.digest(changed)]
        self.assertEqual(record.codec, "dict-zstd")
        self.assertEqual(record.base, bench.digest(base))
        self.assertIsNone(store.records[record.base].base)
        store.verify("b", second)
        counters = {"gets": 0, "bytes": 0, "decoded": 0}
        self.assertEqual(store.decode(record.hash, counters=counters), changed)
        self.assertEqual(counters["gets"], 2)

    def test_orphan_base_is_counted_then_materialized_when_it_costs_more(self):
        policy = bench.Policy("orphan-delta", 64 * bench.KIB, delta=True)
        store = self.store(policy)
        base = bench.noise("orphan", policy.chunk_bytes)
        changed = base[:8192] + b"different" + base[8201:]
        first = self.image("base.bin", base)
        second = self.image("changed.bin", changed)
        store.publish("a", first)
        store.publish("b", second)
        self.assertIsNotNone(store.records[bench.digest(changed)].base)
        del store.manifests["a"]
        store.collect()
        self.assertEqual(len(store.records), 1)
        self.assertIsNone(store.records[bench.digest(changed)].base)
        self.assertEqual(store.delta_materialized, 1)
        store.verify("b", second)
        stats = store.stats()
        self.assertEqual(stats["data_bytes"], stats["live_record_bytes"])
        self.assertEqual(stats["dependency_only_bytes"], 0)
        self.assertFalse((store.directory / "a.metadata").exists())
        del store.manifests["b"]
        store.collect()
        self.assertFalse(list(store.directory.glob("*.pack")))

    def test_dictionary_delta_supports_different_target_length(self):
        policy = bench.Policy("different-length", 64 * bench.KIB, delta=True)
        store = self.store(policy)
        base = bench.noise("different-length", policy.chunk_bytes)
        changed = base[:32768] + b"inserted range" + base[32768:60000]
        first = self.image("base.bin", base)
        second = self.image("changed.bin", changed)
        store.publish("a", first)
        store.publish("b", second)
        record = store.records[bench.digest(changed)]
        self.assertEqual(record.codec, "dict-zstd")
        self.assertNotEqual(record.size, store.records[record.base].size)
        store.verify("b", second)

    def test_shared_orphan_base_survives_owner_deletion_until_uneconomical(self):
        policy = bench.Policy("shared-delta", 64 * bench.KIB, delta=True)
        store = self.store(policy)
        base = bench.noise("shared-orphan", policy.chunk_bytes)
        first = self.image("base.bin", base)
        store.publish("a", first)
        paths = {}
        for number in range(3):
            changed = bytearray(base)
            changed[8192 + number * 4096] ^= 0xFF
            path = self.image(f"changed-{number}.bin", changed)
            paths[str(number)] = path
            store.publish(str(number), path)
        del store.manifests["a"]
        store.collect()
        stats = store.stats()
        self.assertGreater(stats["dependency_only_bytes"], 0)
        self.assertEqual(stats["delta_chunks"], 3)
        self.assertEqual(stats["data_bytes"], stats["live_record_bytes"])
        for disk, path in paths.items():
            store.verify(disk, path)
        del store.manifests["0"]
        del store.manifests["1"]
        store.collect()
        self.assertEqual(store.stats()["dependency_only_bytes"], 0)
        store.verify("2", paths["2"])

    def test_delta_metadata_and_ciphertext_tampering_fail_authentication(self):
        policy = bench.Policy("authenticated", 64 * bench.KIB, delta=True)
        store = self.store(policy)
        base = bench.noise("authentication", policy.chunk_bytes)
        changed = base[:9000] + b"!" + base[9001:]
        store.publish("a", self.image("base.bin", base))
        store.publish("b", self.image("changed.bin", changed))
        record = store.records[bench.digest(changed)]
        actual = record.base
        record.base = "0" * 64
        with self.assertRaises(InvalidTag):
            store.decode(record.hash)
        record.base = actual
        pack = store.directory / record.pack
        with pack.open("r+b") as stream:
            position = record.offset + 25
            stream.seek(position)
            byte = stream.read(1)
            stream.seek(position)
            stream.write(bytes([byte[0] ^ 1]))
        with self.assertRaises(InvalidTag):
            store.decode(record.hash)

    def test_unrelated_chunks_use_full_encoding(self):
        policy = bench.Policy("unrelated", 64 * bench.KIB, delta=True)
        store = self.store(policy)
        first = bench.noise("first", policy.chunk_bytes)
        second = bench.noise("second", policy.chunk_bytes)
        store.publish("a", self.image("first.bin", first))
        store.publish("a", self.image("second.bin", second))
        self.assertIsNone(store.records[bench.digest(second)].base)
        store.collect()
        self.assertEqual(len(store.records), 1)

    def test_cdc_reuses_shifted_content_and_last_owner_frees_packs(self):
        base = bench.noise("cdc-shift", bench.MIB)
        changed = b"!" + base[:-1]
        path = self.image("base.bin", base)
        changed_path = self.image("changed.bin", changed)
        counts = []
        for cdc in (False, True):
            policy = bench.Policy(f"cdc-{cdc}", 64 * bench.KIB, cdc=cdc)
            store = self.store(policy)
            store.publish("a", path)
            before = len(store.records)
            store.publish("b", changed_path)
            counts.append(len(store.records) - before)
            store.verify("b", changed_path)
            del store.manifests["a"]
            store.collect()
            store.verify("b", changed_path)
            del store.manifests["b"]
            store.collect()
            self.assertEqual(store.records, {})
            self.assertFalse(list(store.directory.glob("*.pack")))
        self.assertLess(counts[1], counts[0])


if __name__ == "__main__":
    unittest.main()
