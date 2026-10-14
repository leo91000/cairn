"""Copy bounded immutable published cache regions; never open live raw disks for writing.

Private output includes workload bytes and hash inventories. Do not publish it.
Verify every copied object against its content identity before using the sample.
"""

import argparse
import hashlib
import json
import math
import os
import sqlite3
import stat
import struct
import time
from collections import OrderedDict
from pathlib import Path

REGION = 4 * 1024 * 1024


class SampleBudgetReached(ValueError):
    pass


def manifest(directory):
    path = directory / "journal-v2.sqlite"
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=1) as db:
        row = db.execute("SELECT manifest,generation FROM state WHERE id=1").fetchone()
    return json.loads(row[0]), row[1]


class Reader:
    def __init__(self, directory, value, output, budget):
        self.directory, self.value, self.output, self.budget = (
            directory,
            value,
            output,
            budget,
        )
        self.records = {}
        self.cache = OrderedDict()
        if len(self.budget) == 2:
            self.budget.append(0)

    def region(self, index):
        if index < 0 or index >= len(self.value["blocks"]):
            raise ValueError("Region outside published image")
        block = self.value["blocks"][index]
        size = min(REGION, self.value["size"] - index * REGION)
        identity = block.get("hash")
        if not identity:
            self.records[str(index)] = {"hash": None, "size": size}
            return bytes(size)
        if not (
            len(identity) == 64
            and all(c in "0123456789abcdef" for c in identity)
            or identity.startswith("b3-")
            and len(identity) == 67
            and all(c in "0123456789abcdef" for c in identity[3:])
        ):
            raise ValueError("Invalid content identity")
        if identity in self.cache:
            if len(self.cache[identity]) != size:
                raise ValueError("Cached content reused with a different extent size")
            self.cache.move_to_end(identity)
            if str(index) not in self.records:
                self.records[str(index)] = {
                    "hash": identity,
                    "size": size,
                    "sha256": hashlib.sha256(self.cache[identity]).hexdigest(),
                }
            return self.cache[identity]
        path = self.directory / "cache" / identity
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as stream:
            metadata = os.fstat(stream.fileno())
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_size != size:
                raise ValueError("Cache extent differs from manifest")
            if self.budget[2] + size > self.budget[1] * 2:
                raise SampleBudgetReached("Private sample read budget reached")
            data = stream.read(REGION + 1)
            self.budget[2] += len(data)
        if len(data) != size:
            raise ValueError("Cache extent changed")
        target = self.output / "objects" / identity
        if not target.exists():
            if self.budget[0] + size > self.budget[1]:
                raise SampleBudgetReached("Private sample byte budget reached")
            with target.open("xb") as stream:
                stream.write(data)
            self.budget[0] += size
        self.records[str(index)] = {
            "hash": identity,
            "size": size,
            "sha256": hashlib.sha256(data).hexdigest(),
        }
        self.cache[identity] = data
        while len(self.cache) > 4:
            self.cache.popitem(last=False)
        return data

    def read(self, offset, size):
        result = bytearray()
        while len(result) < size:
            position = offset + len(result)
            region = self.region(position // REGION)
            inner = position % REGION
            result.extend(
                region[inner : inner + min(size - len(result), len(region) - inner)]
            )
        return bytes(result)


def ext4_metadata(reader):
    superblock = reader.read(1024, 1024)

    def u16(offset):
        return struct.unpack_from("<H", superblock, offset)[0]

    def u32(offset):
        return struct.unpack_from("<I", superblock, offset)[0]

    if u16(56) != 0xEF53 or u32(96) & 0x10 or u32(100) & 0x200:
        raise ValueError("Unsupported ext4 metadata layout")
    block_size = 1024 << u32(24)
    if block_size != 4096:
        raise ValueError("Study requires 4 KiB ext4 allocation blocks")
    count = u32(4) | (u32(336) << 32 if u32(96) & 0x80 else 0)
    groups = math.ceil((count - u32(20)) / u32(32))
    descriptor_size = max(32, u16(254)) if u32(96) & 0x80 else 32
    if groups > 8192 or descriptor_size not in (32, 64):
        raise ValueError("Unsupported descriptor table size")
    descriptors = reader.read(block_size, groups * descriptor_size)
    bitmaps = []
    for group in range(groups):
        descriptor = descriptors[
            group * descriptor_size : (group + 1) * descriptor_size
        ]
        bitmap = struct.unpack_from("<I", descriptor, 0)[0]
        if descriptor_size == 64:
            bitmap |= struct.unpack_from("<I", descriptor, 32)[0] << 32
        if struct.unpack_from("<H", descriptor, 18)[0] & 2:
            continue
        try:
            reader.read(bitmap * block_size, block_size)
            bitmaps.append(group)
        except SampleBudgetReached:
            raise
        except (OSError, ValueError):
            pass
    first = descriptors[:descriptor_size]
    inode_table = struct.unpack_from("<I", first, 8)[0]
    if descriptor_size == 64:
        inode_table |= struct.unpack_from("<I", first, 40)[0] << 32
    inode = reader.read(inode_table * block_size + 7 * u16(88), u16(88))
    journal_blocks = []

    def extents(node, remaining_depth=5):
        magic, entries, maximum, depth = struct.unpack_from("<HHHH", node)
        if magic != 0xF30A or entries > maximum or remaining_depth == 0 or depth > 4:
            raise ValueError("Invalid or unsupported journal extent tree")
        for number in range(entries):
            entry = node[12 + number * 12 : 24 + number * 12]
            if depth:
                _, low, high = struct.unpack_from("<IIH", entry)
                extents(
                    reader.read((low | high << 32) * block_size, block_size),
                    remaining_depth - 1,
                )
            else:
                _, length, high, low = struct.unpack_from("<IHHI", entry)
                if not length or length > 32768:
                    raise ValueError("Unwritten journal extent")
                start = low | high << 32
                if sum(length for _, length in journal_blocks) + length > 65536:
                    raise ValueError("Journal exceeds 256 MiB bound")
                journal_blocks.append((start, length))

    extents(inode[40:100])
    for start, length in journal_blocks:
        for block in range(start, start + length):
            reader.read(block * block_size, block_size)
    return {
        "blockSize": block_size,
        "blocksPerGroup": u32(32),
        "firstDataBlock": u32(20),
        "groupCount": groups,
        "verifiedBitmapGroups": bitmaps,
        "journalComplete": True,
        "needsRecovery": bool(u32(96) & 4),
        "filesystemState": u16(58),
        "journalBlocks": sum(n for _, n in journal_blocks),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--phase", choices=["first", "second"], required=True)
    parser.add_argument("--disks", type=int, default=9)
    parser.add_argument("--regions", type=int, default=6)
    parser.add_argument("--metadata-disks", type=int, default=3)
    parser.add_argument("--max-mib", type=int, default=1024)
    args = parser.parse_args()
    os.umask(0o077)
    if (
        not 1 <= args.disks <= 16
        or not 1 <= args.regions <= 32
        or not 1 <= args.max_mib <= 2048
    ):
        parser.error("Sample limits exceeded")
    if not 0 <= args.metadata_disks <= args.disks:
        parser.error("Metadata disk limit must fit the selected disk count")
    args.root = args.root.resolve()
    args.output.mkdir(mode=0o700, parents=True, exist_ok=True)
    (args.output / "objects").mkdir(mode=0o700, exist_ok=True)
    destination = args.output / (args.phase + ".private.json")
    if destination.exists():
        parser.error("Phase already captured; choose a new private output")
    budget = [
        sum(p.stat().st_size for p in (args.output / "objects").iterdir()),
        args.max_mib * 1024 * 1024,
    ]
    if args.phase == "first":
        eligible = []
        for category in ("disks", "environments"):
            for directory in sorted((args.root / category).glob("*/lazy")):
                if (
                    directory.resolve() != directory
                    or (directory / "cache").is_symlink()
                ):
                    continue
                try:
                    value, _ = manifest(directory)
                    cached = [
                        i
                        for i, b in enumerate(value["blocks"])
                        if b.get("hash") and (directory / "cache" / b["hash"]).is_file()
                    ]
                    if len(cached) >= args.regions:
                        eligible.append(
                            (
                                directory,
                                cached,
                                (directory / "journal-v2.sqlite").stat().st_mtime_ns,
                            )
                        )
                except (OSError, sqlite3.Error, ValueError, KeyError):
                    pass
        eligible.sort(key=lambda item: item[2], reverse=True)
        selected = []
        for number, (directory, cached, _) in enumerate(eligible[: args.disks]):
            positions = (
                [
                    cached[round(i * (len(cached) - 1) / (args.regions - 1))]
                    for i in range(args.regions)
                ]
                if args.regions > 1
                else cached[:1]
            )
            selected.append(
                {
                    "label": f"disk-{number:02}",
                    "directory": str(directory),
                    "sampleIndices": positions,
                }
            )
        (args.output / "selection.private.json").write_text(
            json.dumps(selected, indent=2)
        )
    else:
        selected = json.loads((args.output / "selection.private.json").read_text())
    captured = []
    if not selected:
        raise ValueError("No eligible published cache regions")
    for number, selection in enumerate(selected):
        directory = Path(selection["directory"])
        if (
            not directory.is_relative_to(args.root)
            or directory.resolve() != directory
            or (directory / "cache").is_symlink()
        ):
            raise ValueError("Selection escaped source root")
        try:
            value, generation = manifest(directory)
        except (OSError, sqlite3.Error):
            continue
        reader = Reader(directory, value, args.output, budget)
        sampled = []
        for index in selection["sampleIndices"]:
            try:
                reader.region(index)
                sampled.append(index)
            except SampleBudgetReached:
                break
            except (OSError, ValueError):
                pass
        if not sampled:
            continue
        metadata = None
        if number < args.metadata_disks:
            try:
                metadata = ext4_metadata(reader)
            except (OSError, ValueError, struct.error):
                metadata = {"journalComplete": False}
        captured.append(
            {
                "label": selection["label"],
                "capturedAt": time.time(),
                "size": value["size"],
                "generation": generation,
                "sampleIndices": sampled,
                "blocks": reader.records,
                "ext4": metadata,
            }
        )
    if not captured:
        parser.error("No selected published cache regions remain available")
    destination.write_text(
        json.dumps(
            {"schema": 1, "phase": args.phase, "private": True, "disks": captured},
            indent=2,
        )
    )
    print(
        json.dumps(
            {
                "phase": args.phase,
                "disks": len(captured),
                "skippedDisks": len(selected) - len(captured),
                "sampledRegions": sum(len(d["sampleIndices"]) for d in captured),
                "journalCompleteDisks": sum(
                    bool(d["ext4"] and d["ext4"].get("journalComplete"))
                    for d in captured
                ),
                "copiedMiB": budget[0] // (1024 * 1024),
                "readMiB": budget[2] // (1024 * 1024),
            }
        )
    )


if __name__ == "__main__":
    main()
