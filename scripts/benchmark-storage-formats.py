# /// script
# requires-python = ">=3.11"
# dependencies = ["cryptography==46.0.3", "fastcdc==1.7.0", "zstandard==0.25.0"]
# ///
"""Storage-format experiment, never a publisher or migration tool.

uv run --python 3.11 scripts/benchmark-storage-formats.py --output /path/to/results
External corpora: --corpus /path/to/corpus.json (schema in the accompanying doc).
Only aggregate metrics are written to results.json; source bytes remain local.
"""

import argparse
import bisect
import hashlib
import importlib.metadata
import json
import math
import os
import platform
import random
import re
import shutil
import statistics
import struct
import subprocess
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path

import zstandard as zstd
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from fastcdc import fastcdc

KIB = 1024
MIB = 1024 * KIB
REGION = 4 * MIB
PACK_BYTES = 8 * MIB
HEADER = b"CAIRNEXP"
KEY = hashlib.sha256(b"public synthetic benchmark key, never production").digest()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def json_bytes(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode()


def noise(label, size):
    return random.Random(label).randbytes(size)


def percentile(values, fraction):
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)]


@dataclass(frozen=True)
class Policy:
    name: str
    chunk_bytes: int
    compressed: bool = True
    cdc: bool = False
    delta: bool = False
    packed: bool = True


POLICIES = [
    Policy("current-4m", REGION, compressed=False, packed=False),
    Policy("current-v2", REGION, packed=False),
    Policy("fixed-4k", 4 * KIB),
    Policy("fixed-16k", 16 * KIB),
    Policy("fixed-64k", 64 * KIB),
    Policy("fixed-256k", 256 * KIB),
    Policy("cdc-16k", 16 * KIB, cdc=True),
    Policy("cdc-64k", 64 * KIB, cdc=True),
    Policy("delta-4m", REGION, delta=True),
    Policy("delta-64k", 64 * KIB, delta=True),
    Policy("cdc-delta-64k", 64 * KIB, cdc=True, delta=True),
]


def chunks(data, policy):
    # Bound CDC propagation and random-read reconstruction to a 4 MiB region.
    for start in range(0, len(data), REGION):
        region = data[start : start + REGION]
        if policy.cdc:
            for chunk in fastcdc(
                region,
                policy.chunk_bytes // 4,
                policy.chunk_bytes,
                policy.chunk_bytes * 4,
            ):
                yield (
                    start + chunk.offset,
                    region[chunk.offset : chunk.offset + chunk.length],
                )
        else:
            for offset in range(0, len(region), policy.chunk_bytes):
                yield start + offset, region[offset : offset + policy.chunk_bytes]


def sketch(data):
    # A deliberately simple bounded similarity index, not a SOTA reference search.
    features = {
        hashlib.sha256(part).digest()[:8]
        for offset in range(0, len(data), 4 * KIB)
        if (part := data[offset : offset + 4 * KIB]).strip(b"\0")
    }
    return sorted(features)[:4]


def xor_bytes(a, b):
    if len(a) != len(b):
        raise ValueError("XOR base length differs")
    return (int.from_bytes(a, "little") ^ int.from_bytes(b, "little")).to_bytes(
        len(a), "little"
    )


@dataclass
class Record:
    hash: str
    size: int
    codec: str
    base: str | None
    payload: bytes
    pack: str | None = None
    offset: int = 0

    def aad(self):
        return json_bytes([self.hash, self.size, self.codec, self.base])


class Store:
    """Local immutable packs with authenticated, independently decodable records."""

    def __init__(self, directory, policy):
        self.directory = Path(directory)
        self.directory.mkdir(parents=True)
        self.policy = policy
        self.records = {}
        self.manifests = {}
        self.similar = {}
        self.aes = AESGCM(KEY)
        self.compressor = zstd.ZstdCompressor(level=3)
        self.uploaded_bytes = 0
        self.compaction_bytes = 0
        self.delta_attempts = 0
        self.delta_materialized = 0

    def seal(self, record, encoded):
        nonce = os.urandom(12)
        # Match the current v2 data envelope's five encrypted codec/length bytes.
        codec = {"raw": 0, "zstd": 1, "xor-zstd": 2, "dict-zstd": 3}[record.codec]
        encoded = bytes([codec]) + struct.pack("<I", record.size) + encoded
        record.payload = HEADER + nonce + self.aes.encrypt(nonce, encoded, record.aad())
        return record

    def full_encoding(self, data):
        if self.policy.compressed:
            encoded = self.compressor.compress(data)
            if len(encoded) < len(data):
                return "zstd", encoded
        return "raw", data

    def decode(self, hash_value, cache=None, counters=None):
        cache = {} if cache is None else cache
        if hash_value in cache:
            return cache[hash_value]
        record = self.records[hash_value]
        if record.pack:
            with (self.directory / record.pack).open("rb") as stream:
                stream.seek(record.offset)
                payload = stream.read(len(record.payload))
        else:
            payload = record.payload
        if counters is not None:
            counters["gets"] += 1
            counters["bytes"] += len(payload)
            counters["decoded"] += record.size
        if payload[:8] != HEADER:
            raise ValueError("Invalid envelope")
        encoded = self.aes.decrypt(payload[8:20], payload[20:], record.aad())
        expected_codec = {"raw": 0, "zstd": 1, "xor-zstd": 2, "dict-zstd": 3}[
            record.codec
        ]
        if (
            len(encoded) < 5
            or encoded[0] != expected_codec
            or struct.unpack_from("<I", encoded, 1)[0] != record.size
        ):
            raise ValueError("Invalid authenticated codec/length prefix")
        encoded = encoded[5:]
        if record.codec == "raw":
            data = encoded
        elif record.codec == "zstd":
            data = zstd.ZstdDecompressor().decompress(
                encoded, max_output_size=record.size
            )
        else:
            base = self.decode(record.base, cache, counters)
            if record.codec == "xor-zstd":
                patch = zstd.ZstdDecompressor().decompress(
                    encoded, max_output_size=record.size
                )
                data = xor_bytes(base, patch)
            elif record.codec == "dict-zstd":
                dictionary = zstd.ZstdCompressionDict(
                    base, dict_type=zstd.DICT_TYPE_RAWCONTENT
                )
                data = zstd.ZstdDecompressor(dict_data=dictionary).decompress(
                    encoded, max_output_size=record.size
                )
            else:
                raise ValueError("Unknown codec")
        if len(data) != record.size or digest(data) != hash_value:
            raise ValueError("Plaintext integrity failure")
        cache[hash_value] = data
        return data

    def encode(self, data, candidates):
        hash_value = digest(data)
        codec, encoded = self.full_encoding(data)
        selected_base = None
        if self.policy.delta:
            features = sketch(data)
            for feature in features:
                candidates.extend(reversed(self.similar.get(feature, [])[-2:]))
            tried = set()
            for base_hash in candidates:
                if base_hash in tried or base_hash not in self.records:
                    continue
                base_record = self.records[base_hash]
                if base_record.base:
                    continue
                tried.add(base_hash)
                base = self.decode(base_hash)
                self.delta_attempts += 1
                dictionary = zstd.ZstdCompressionDict(
                    base, dict_type=zstd.DICT_TYPE_RAWCONTENT
                )
                params = zstd.ZstdCompressionParameters.from_level(
                    3,
                    window_log=max(
                        17, math.ceil(math.log2(max(len(data), len(base)))) + 1
                    ),
                )
                options = [
                    (
                        "dict-zstd",
                        zstd.ZstdCompressor(
                            dict_data=dictionary, compression_params=params
                        ).compress(data),
                    ),
                ]
                if len(base) == len(data):
                    options.insert(
                        0, ("xor-zstd", self.compressor.compress(xor_bytes(base, data)))
                    )
                candidate_codec, candidate = min(options, key=lambda item: len(item[1]))
                # Include the extra 32-byte dependency hash; reject marginal savings.
                if len(candidate) + 32 < len(encoded) * 0.90:
                    codec, encoded, selected_base = (
                        candidate_codec,
                        candidate,
                        base_hash,
                    )
                if len(tried) == 4:
                    break
            if selected_base is None:
                for feature in features:
                    self.similar.setdefault(feature, []).append(hash_value)
        record = Record(hash_value, len(data), codec, selected_base, b"")
        self.records[hash_value] = self.seal(record, encoded)

    def publish(self, disk, path):
        previous = {
            offset: hash_value for offset, _, hash_value in self.manifests.get(disk, [])
        }
        anchor_spans = next(
            (spans for name, spans in self.manifests.items() if name != disk), []
        )
        anchor = {offset: hash_value for offset, _, hash_value in anchor_spans}
        manifest = []
        with Path(path).open("rb") as stream:
            position = 0
            while region := stream.read(REGION):
                for offset, data in chunks(region, self.policy):
                    offset += position
                    hash_value = digest(data) if data.strip(b"\0") else None
                    if hash_value and hash_value not in self.records:
                        candidates = [previous[offset]] if previous.get(offset) else []
                        if anchor.get(offset):
                            candidates.append(anchor[offset])
                        self.encode(data, candidates)
                    manifest.append((offset, len(data), hash_value))
                position += len(region)
        self.manifests[disk] = manifest
        self.flush()

    def flush(self):
        pending = [record for record in self.records.values() if record.pack is None]
        while pending:
            name = f"{uuid.uuid4()}.pack"
            selected = []
            length = 0
            while pending:
                record = pending[-1]
                if selected and (
                    not self.policy.packed or length + len(record.payload) > PACK_BYTES
                ):
                    break
                pending.pop()
                record.pack, record.offset = name, length
                length += len(record.payload)
                selected.append(record)
            with (self.directory / name).open("wb") as stream:
                for record in selected:
                    stream.write(record.payload)
            self.uploaded_bytes += length

    def roots(self):
        return {
            hash_value
            for spans in self.manifests.values()
            for _, _, hash_value in spans
            if hash_value
        }

    def collect(self):
        old_packs = {record.pack for record in self.records.values() if record.pack}
        rewritten_packs = set()
        roots = self.roots()
        children = {}
        for hash_value in roots:
            record = self.records[hash_value]
            if record.base:
                children.setdefault(record.base, []).append(record)
        for base_hash, records in children.items():
            if base_hash in roots:
                continue
            # Count retained bases. Drop uneconomical dependencies as a group.
            alternatives = []
            for record in records:
                data = self.decode(record.hash)
                codec, encoded = self.full_encoding(data)
                alternatives.append((record, codec, encoded))
            full_bytes = sum(len(encoded) + 36 for _, _, encoded in alternatives)
            delta_bytes = len(self.records[base_hash].payload) + sum(
                len(record.payload) + 32 for record in records
            )
            if full_bytes <= delta_bytes:
                for record, codec, encoded in alternatives:
                    if record.pack:
                        rewritten_packs.add(record.pack)
                    record.codec, record.base, record.pack = codec, None, None
                    self.seal(record, encoded)
                    self.delta_materialized += 1
        reachable = roots | {
            self.records[hash_value].base
            for hash_value in roots
            if self.records[hash_value].base
        }
        dirty_packs = rewritten_packs | {
            record.pack
            for hash_value, record in self.records.items()
            if hash_value not in reachable
        }
        # Re-encoding a record also leaves its old pack obsolete. Repack all survivors
        # of affected packs; this is ideal compaction, not an immediate S3 operation.
        self.records = {
            hash_value: record
            for hash_value, record in self.records.items()
            if hash_value in reachable
        }
        for record in self.records.values():
            if record.pack in dirty_packs:
                record.pack = None
                self.compaction_bytes += len(record.payload)
        self.flush()
        live_packs = {record.pack for record in self.records.values()}
        for name in old_packs - live_packs:
            (self.directory / name).unlink()
        self.similar = {
            feature: [
                h for h in hashes if h in self.records and not self.records[h].base
            ]
            for feature, hashes in self.similar.items()
        }

    def metadata(self):
        index = (
            [
                [r.hash, r.size, r.codec, r.base, r.pack, r.offset, len(r.payload)]
                for r in self.records.values()
            ]
            if self.policy.packed
            else []
        )
        documents = {"index": index, **self.manifests}
        for path in self.directory.glob("*.metadata"):
            if path.stem not in documents:
                path.unlink()
        total = 0
        for name, document in documents.items():
            encoded = json_bytes(document)
            if self.policy.compressed:
                encoded = self.compressor.compress(encoded)
            nonce = os.urandom(12)
            payload = nonce + self.aes.encrypt(nonce, encoded, name.encode())
            (self.directory / f"{name}.metadata").write_bytes(payload)
            total += len(payload)
        return total

    def stats(self):
        roots = self.roots()
        metadata = self.metadata()
        physical = sum(path.stat().st_size for path in self.directory.glob("*.pack"))
        return {
            "data_bytes": physical,
            "metadata_bytes": metadata,
            "stored_bytes": physical + metadata,
            "live_record_bytes": sum(len(r.payload) for r in self.records.values()),
            "objects": len(list(self.directory.glob("*.pack"))),
            "unique_chunks": len(self.records),
            "delta_chunks": sum(r.base is not None for r in self.records.values()),
            "delta_codecs": {
                codec: sum(r.codec == codec for r in self.records.values())
                for codec in ("xor-zstd", "dict-zstd")
            },
            "dependency_only_bytes": sum(
                len(r.payload) for h, r in self.records.items() if h not in roots
            ),
            "delta_attempts": self.delta_attempts,
            "materialized_deltas": self.delta_materialized,
            "uploaded_record_bytes": self.uploaded_bytes,
            "compaction_record_bytes": self.compaction_bytes,
        }

    def verify(self, disk, path):
        started = time.perf_counter()
        cache = {}
        with Path(path).open("rb") as stream:
            for _, size, hash_value in self.manifests[disk]:
                restored = self.decode(hash_value, cache) if hash_value else bytes(size)
                if restored != stream.read(size):
                    raise AssertionError("Disk reconstruction differs")
            if stream.read(1):
                raise AssertionError("Incomplete disk reconstruction")
        return time.perf_counter() - started

    def probes(self, paths, count=32):
        positions_used = []
        times, cpu_times, warm_times, warm_cpu, requests, fetched, expanded = (
            [],
            [],
            [],
            [],
            [],
            [],
            [],
        )
        for disk, path in paths.items():
            spans = self.manifests[disk]
            offsets = [span[0] for span in spans]
            # Identical source offsets across policies; choosing stored chunks
            # would give the fine-chunk formats a different read workload.
            pages = range(Path(path).stat().st_size // 4096)
            selected = random.Random(disk).sample(pages, min(count, len(pages)))
            with Path(path).open("rb") as source:
                for page in selected:
                    positions_used.append((disk, page))
                    position = page * 4096
                    source.seek(position)
                    expected = source.read(4096)
                    counters = {"gets": 0, "bytes": 0, "decoded": 0}
                    index = bisect.bisect_right(offsets, position) - 1
                    output = bytearray()
                    cache = {}
                    cpu_started = time.process_time_ns()
                    started = time.perf_counter()
                    while len(output) < 4096:
                        offset, length, hash_value = spans[index]
                        data = (
                            self.decode(hash_value, cache, counters)
                            if hash_value
                            else bytes(length)
                        )
                        inner = position + len(output) - offset
                        output.extend(
                            data[
                                inner : inner + min(length - inner, 4096 - len(output))
                            ]
                        )
                        index += 1
                    times.append((time.perf_counter() - started) * 1000)
                    cpu_times.append((time.process_time_ns() - cpu_started) / 1_000_000)
                    if output != expected:
                        raise AssertionError("Random read differs")
                    requests.append(counters["gets"])
                    fetched.append(counters["bytes"])
                    expanded.append(counters["decoded"])
                    warm_started = time.perf_counter_ns()
                    warm_cpu_started = time.process_time_ns()
                    restored = bytearray()
                    index = bisect.bisect_right(offsets, position) - 1
                    while len(restored) < 4096:
                        offset, length, hash_value = spans[index]
                        data = (
                            self.decode(hash_value, cache)
                            if hash_value
                            else bytes(length)
                        )
                        inner = position + len(restored) - offset
                        restored.extend(
                            data[
                                inner : inner
                                + min(length - inner, 4096 - len(restored))
                            ]
                        )
                        index += 1
                    warm_cpu.append(
                        (time.process_time_ns() - warm_cpu_started) / 1_000_000
                    )
                    warm_times.append(
                        (time.perf_counter_ns() - warm_started) / 1_000_000
                    )
                    if restored != expected:
                        raise AssertionError("Warm read differs")
        if not times:
            return {"count": 0}
        return {
            "count": len(times),
            "probe_positions_fingerprint": digest(json_bytes(positions_used)),
            "local_ms_p50": statistics.median(times),
            "local_ms_p95": percentile(times, 0.95),
            "process_cpu_ms_p50": statistics.median(cpu_times),
            "process_cpu_ms_p95": percentile(cpu_times, 0.95),
            "warm_local_ms_p50": statistics.median(warm_times),
            "warm_local_ms_p95": percentile(warm_times, 0.95),
            "warm_process_cpu_ms_p50": statistics.median(warm_cpu),
            "range_gets_mean": statistics.mean(requests),
            "range_bytes_mean": statistics.mean(fetched),
            "decoded_bytes_mean": statistics.mean(expanded),
        }


def debugfs(image, command, write=False):
    result = subprocess.run(
        [
            shutil.which("debugfs") or "/usr/sbin/debugfs",
            *(["-w"] if write else []),
            "-R",
            command,
            str(image),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    # debugfs sometimes exits zero after a failed command.
    if any(
        text in result.stderr
        for text in ("not found", "Could not", "File not found", "No such")
    ):
        raise RuntimeError(result.stderr)
    return result.stdout


def file_blocks(image, file):
    return [int(value) for value in debugfs(image, f"blocks {file}").split()]


def fixtures(directory, repo, disks):
    directory.mkdir(parents=True)
    source_files = sorted((repo / "crates").glob("*/src/**/*.rs"))
    source_files = [path for path in source_files if path.is_file()]
    source_files = source_files[:100]
    corpus = {
        "label": "independent-ext4",
        "generations": [],
        "trim_generations": [],
        "notes": [],
    }
    for generation in range(3):
        corpus["generations"].append([])
    source_bytes = 0
    for disk in range(disks):
        with tempfile.TemporaryDirectory(prefix="cairn-ext4-source-") as temporary:
            tree = Path(temporary)
            directories = [
                "00-placement",
                "10-dependencies",
                "20-sources",
                "30-build",
                "40-cache",
            ]
            for name in directories:
                (tree / name).mkdir()
            (tree / "00-placement/padding.bin").write_bytes(
                noise(f"pad-{disk}", disk * 4096)
            )
            (tree / "10-dependencies/toolchain.bin").write_bytes(
                noise("shared-toolchain", 8 * MIB)
            )
            artifact = noise("shared-shifted-artifact", REGION)
            if disk:
                insertion = noise(f"insert-{disk}", 19)
                artifact = artifact[:MIB] + insertion + artifact[MIB : -len(insertion)]
            (tree / "10-dependencies/artifact.bin").write_bytes(artifact)
            for index, path in enumerate(source_files):
                data = path.read_bytes()
                source_bytes += len(data) if disk == 0 else 0
                (tree / f"20-sources/source-{index:03}.rs").write_bytes(data)
            (tree / "30-build/private.bin").write_bytes(
                noise(f"unique-build-{disk}", 2 * MIB)
            )
            (tree / "40-cache/deleted.bin").write_bytes(
                noise(f"discarded-cache-{disk}", REGION)
            )
            image = directory / f"disk-{disk}-g0.ext4"
            with image.open("wb") as stream:
                stream.truncate(64 * MIB)
            subprocess.run(
                [
                    shutil.which("mkfs.ext4") or "/usr/sbin/mkfs.ext4",
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
            deleted_blocks = file_blocks(image, "/40-cache/deleted.bin")
            modified_blocks = file_blocks(image, "/10-dependencies/toolchain.bin")
            for generation in range(3):
                if generation:
                    previous = image
                    image = directory / f"disk-{disk}-g{generation}.ext4"
                    shutil.copyfile(previous, image)
                    with image.open("r+b") as stream:
                        stream.seek(modified_blocks[32 * generation + disk] * 4096)
                        stream.write(noise(f"mutation-{disk}-{generation}", 4096))
                if generation == 2:
                    debugfs(image, "rm /40-cache/deleted.bin", write=True)
                corpus["generations"][generation].append(
                    {"disk": f"disk-{disk}", "path": str(image.resolve())}
                )
            trimmed = directory / f"disk-{disk}-trim.ext4"
            shutil.copyfile(image, trimmed)
            # Controlled discard model: zero ONLY the extents of the known deleted file.
            commands = directory / "freed-block-check.commands"
            commands.write_text("\n".join(f"testb {block}" for block in deleted_blocks))
            checked = subprocess.run(
                [
                    shutil.which("debugfs") or "/usr/sbin/debugfs",
                    "-f",
                    str(commands),
                    str(image),
                ],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            for block in deleted_blocks:
                if f"Block {block} not in use" not in checked:
                    raise AssertionError("Refusing to discard an allocated block")
            with trimmed.open("r+b") as stream:
                for block in deleted_blocks:
                    stream.seek(block * 4096)
                    stream.write(bytes(4096))
            corpus["trim_generations"].append(
                {"disk": f"disk-{disk}", "path": str(trimmed.resolve())}
            )
    corpus["notes"] = [
        f"{disks} independently formatted 64 MiB ext4 images; {source_bytes} bytes of repository Rust sources per disk.",
        "Shared deterministic high-entropy toolchain (8 MiB), shifted binary (4 MiB), private build (2 MiB), deleted cache (4 MiB).",
        "Three generations, two 4 KiB in-place toolchain changes per disk, then cache file deletion.",
        "TRIM is a model on copies, with every freed extent checked using debugfs; no guest or live discard was exercised.",
    ]
    return corpus


def benchmark(corpus, output, selected):
    results = []
    for policy in selected:
        started = time.perf_counter()
        directory = output / corpus["label"] / policy.name
        store = Store(directory, policy)
        paths = {}
        snapshots = []
        encode_seconds = 0
        encode_cpu_ms = 0
        verify_seconds = 0
        for generation, entries in enumerate(corpus["generations"]):
            before_upload = store.uploaded_bytes
            start = time.perf_counter()
            cpu_start = time.process_time_ns()
            for entry in entries:
                store.publish(entry["disk"], entry["path"])
                paths[entry["disk"]] = entry["path"]
            publication_bytes = store.uploaded_bytes - before_upload
            before_gc = store.stats()
            store.collect()
            after_gc = store.stats()
            encode_seconds += time.perf_counter() - start
            encode_cpu_ms += (time.process_time_ns() - cpu_start) / 1_000_000
            for disk, path in paths.items():
                verify_seconds += store.verify(disk, path)
            snapshots.append(
                {
                    "generation": generation,
                    "before_gc_bytes": before_gc["stored_bytes"],
                    "new_record_bytes": store.uploaded_bytes - before_upload,
                    "new_unique_record_bytes": publication_bytes,
                    "gc_rewrite_record_bytes": store.uploaded_bytes
                    - before_upload
                    - publication_bytes,
                    **after_gc,
                }
            )
        probes = store.probes(paths)
        final = store.stats()
        trim = None
        if corpus.get("trim_generations"):
            trim_started = time.perf_counter()
            trim_cpu_started = time.process_time_ns()
            trim_upload_start = store.uploaded_bytes
            for entry in corpus["trim_generations"]:
                store.publish(entry["disk"], entry["path"])
                paths[entry["disk"]] = entry["path"]
            store.collect()
            trim = store.stats()
            trim["write_process_cpu_ms"] = (
                time.process_time_ns() - trim_cpu_started
            ) / 1_000_000
            trim["write_wall_ms"] = (time.perf_counter() - trim_started) * 1000
            trim["new_upload_record_bytes"] = store.uploaded_bytes - trim_upload_start
            for disk, path in paths.items():
                store.verify(disk, path)
            trim["cold_4k_reads"] = store.probes(paths)
        # Verify dependencies survive owner deletion, and last-owner purge empties packs.
        delete_bytes = []
        for disk in list(paths):
            del store.manifests[disk]
            del paths[disk]
            store.collect()
            for survivor, path in paths.items():
                store.verify(survivor, path)
            delete_bytes.append(store.stats()["stored_bytes"])
        if store.records or list(directory.glob("*.pack")):
            raise AssertionError("Last-owner collection left payloads")
        row = {
            "policy": policy.name,
            **final,
            "snapshots": snapshots,
            "trim": trim,
            "encode_and_gc_seconds": encode_seconds,
            "encode_and_gc_process_cpu_ms": encode_cpu_ms,
            "verify_seconds": verify_seconds,
            "cold_4k_reads": probes,
            "owner_deletion_bytes": delete_bytes,
            "verification": "all generations, trim copies, cold reads and surviving owners matched exact source bytes; last owner freed all packs",
            "elapsed_seconds": time.perf_counter() - started,
        }
        results.append(row)
        print(
            f"{corpus['label']} {policy.name}: {final['stored_bytes'] / MIB:.2f} MiB; delta={final['delta_chunks']}; {row['elapsed_seconds']:.2f}s",
            flush=True,
        )
    return {
        "label": corpus["label"],
        "notes": corpus.get("notes", []),
        "results": results,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, action="append", default=[])
    parser.add_argument("--disks", type=int, default=6)
    parser.add_argument("--policies", nargs="+", choices=[p.name for p in POLICIES])
    args = parser.parse_args()
    if args.output.exists():
        parser.error(
            "Output must be a new directory; existing results are never overwritten"
        )
    if args.disks < 2:
        parser.error("At least two disks are required")
    args.output.mkdir(parents=True)
    selected = [p for p in POLICIES if not args.policies or p.name in args.policies]
    corpora = [json.loads(path.read_text()) for path in args.corpus]
    if not corpora:
        corpus = fixtures(
            args.output / "fixtures", Path(__file__).resolve().parents[1], args.disks
        )
        (args.output / "corpus.private.json").write_text(json.dumps(corpus, indent=2))
        corpora.append(corpus)
    for corpus in corpora:
        if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_-]{0,79}", corpus["label"]):
            parser.error("Corpus label must be a short filename-safe identifier")
        for generation in corpus["generations"]:
            seen = set()
            for entry in generation:
                disk = entry["disk"]
                if disk == "index" or not re.fullmatch(
                    r"[a-zA-Z0-9][a-zA-Z0-9_-]{0,79}", disk
                ):
                    parser.error(
                        "Disk name must be filename-safe and cannot be 'index'"
                    )
                if disk in seen:
                    parser.error("A generation must contain only one state per disk")
                seen.add(disk)
    report = {
        "schema": 2,
        "baseline": "current-v2",
        "benchmark_source_sha256": digest(Path(__file__).read_bytes()),
        "platform": platform.platform(),
        "host_cpu_count": os.cpu_count(),
        "host_load_at_start": list(os.getloadavg()),
        "cpu_quota": Path("/sys/fs/cgroup/cpu.max").read_text().strip()
        if Path("/sys/fs/cgroup/cpu.max").is_file()
        else None,
        "python": platform.python_version(),
        "packages": {
            name: importlib.metadata.version(name)
            for name in ("fastcdc", "cryptography", "zstandard")
        },
        "cdc_backend": fastcdc.__module__,
        "zstd_library": zstd.ZSTD_VERSION,
        "pack_bytes": PACK_BYTES,
        "delta_max_depth": 1,
        "limitations": [
            "Experimental metadata, hashes and AAD; current-v2 reproduces the current 4 MiB dedupe/compression policy and its 41-byte data-envelope overhead. Metadata serialization is modeled, not Cairn's exact authenticated manifest.",
            "Real AES-GCM, zstd and local pack files; no S3 network, fsync durability, process-death recovery or production migration.",
            "Cold means empty decoded cache; the host page cache is not flushed. Identical source read positions are used across policies. Range bytes are records, assuming precise S3 range GETs; no production network latency is measured.",
            "Storage numbers include measured encrypted metadata and retained delta bases, after ideal immediate compaction; production would have grace-period and dead-pack overhead.",
            "Only latest states are roots. Temporal delta dependencies are retained and materialized when full records are cheaper, rather than silently dropping their cost.",
        ],
        "corpora": [],
    }
    for corpus in corpora:
        report["corpora"].append(benchmark(corpus, args.output, selected))
        (args.output / "results.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
