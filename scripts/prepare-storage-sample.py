# /// script
# requires-python = ">=3.11"
# dependencies = ["blake3==1.0.8"]
# ///
"""Verify private cache exports and build offline delta/discard experiment inputs.

Never run filesystem tools on an installation path. The only writable images
are newly created sparse files in the required private output directory.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
from pathlib import Path

import blake3

REGION = 4 * 1024 * 1024


def verify(corpus, snapshots):
    checked = {}
    for snapshot in snapshots:
        for disk in snapshot["disks"]:
            if (
                not re.fullmatch(r"disk-[0-9]{2}", disk["label"])
                or not 0 < disk["size"] <= 1024**4
            ):
                raise ValueError("Invalid private disk descriptor")
            for index, block in disk["blocks"].items():
                position = int(index) * REGION
                if (
                    position < 0
                    or position >= disk["size"]
                    or block["size"] != min(REGION, disk["size"] - position)
                ):
                    raise ValueError("Sample region outside source disk")
            for block in disk["blocks"].values():
                identity = block["hash"]
                if not identity:
                    continue
                if not re.fullmatch(r"(b3-)?[0-9a-f]{64}", identity):
                    raise ValueError("Invalid sample content identity")
                if identity not in checked:
                    path = corpus / "objects" / identity
                    if path.is_symlink():
                        raise ValueError("Sample object must not be a symlink")
                    data = path.read_bytes()
                    if len(data) > REGION:
                        raise ValueError("Oversized sample object")
                    actual = (
                        blake3.blake3(data).hexdigest()
                        if identity.startswith("b3-")
                        else hashlib.sha256(data).hexdigest()
                    )
                    expected = identity.removeprefix("b3-")
                    if actual != expected:
                        raise ValueError("Sample content identity verification failed")
                    checked[identity] = (len(data), hashlib.sha256(data).hexdigest())
                if checked[identity] != (block["size"], block["sha256"]):
                    raise ValueError("Sample size or transfer checksum differs")
    return len(checked)


def region(corpus, record):
    return (
        (corpus / "objects" / record["hash"]).read_bytes()
        if record["hash"]
        else bytes(record["size"])
    )


def filesystem_command(args, **kwargs):
    executable = shutil.which(args[0]) or "/usr/sbin/" + args[0]
    return subprocess.run(
        [executable, *args[1:]],
        capture_output=True,
        check=False,
        text=True,
        timeout=120,
        **kwargs,
    )


def discard_sample(corpus, disk, indices, output):
    metadata = disk.get("ext4")
    if not metadata or not metadata.get("journalComplete"):
        return None, "journal_not_available"
    if metadata.get("filesystemState", 0) & 2:
        return None, "filesystem_error_flag"
    image = output / (disk["label"] + ".private.ext4")
    with image.open("xb") as stream:
        stream.truncate(disk["size"])
        for index, record in disk["blocks"].items():
            stream.seek(int(index) * REGION)
            stream.write(region(corpus, record))
    try:
        replay = filesystem_command(["e2fsck", "-p", "-E", "journal_only", str(image)])
        if replay.returncode not in (0, 1):
            return None, "journal_replay_failed"
        with image.open("rb") as stream:
            stream.seek(1024)
            superblock = stream.read(1024)
        if (
            struct.unpack_from("<I", superblock, 96)[0] & 4
            or struct.unpack_from("<H", superblock, 58)[0] & 2
        ):
            return None, "filesystem_not_recovered"
        block_size = metadata["blockSize"]
        eligible_groups = set(metadata["verifiedBitmapGroups"])
        positions = []
        for index in indices:
            for offset in range(
                index * REGION, min((index + 1) * REGION, disk["size"]), block_size
            ):
                page = offset // block_size
                # Block zero contains the ext4 superblock/boot area and is
                # always retained. debugfs deliberately rejects testb 0.
                if page == 0:
                    continue
                group = (page - metadata["firstDataBlock"]) // metadata[
                    "blocksPerGroup"
                ]
                if group in eligible_groups:
                    positions.append(page)
        commands = output / (disk["label"] + ".private.commands")
        commands.write_text("".join(f"testb {page}\n" for page in positions))
        bitmap = filesystem_command(["debugfs", "-f", str(commands), str(image)])
        commands.unlink()
        # debugfs verifies metadata checksums by default. Never pass -n: that
        # disables verification. Missing bitmap pages make this disk unknown.
        if bitmap.returncode or any(
            word in bitmap.stderr.lower()
            for word in (
                "checksum",
                "while",
                "could not",
                "error",
                "short read",
                "invalid",
            )
        ):
            return None, "allocation_bitmap_validation_failed"
        free = {int(v) for v in re.findall(r"Block (\d+) not in use", bitmap.stdout)}
        allocated = {
            int(v) for v in re.findall(r"Block (\d+) marked in use", bitmap.stdout)
        }
        if set(positions) != free | allocated or free & allocated:
            return None, "incomplete_allocation_answers"
        before = output / (disk["label"] + "-replayed.private.bin")
        after = output / (disk["label"] + "-discard.private.bin")
        changed_bytes = 0
        with (
            image.open("rb") as stream,
            before.open("xb") as baseline,
            after.open("xb") as trimmed,
        ):
            for index in indices:
                stream.seek(index * REGION)
                data = stream.read(min(REGION, disk["size"] - index * REGION))
                result = bytearray(data)
                for inner in range(0, len(data), block_size):
                    page = (index * REGION + inner) // block_size
                    if page in free:
                        changed_bytes += sum(
                            v != 0 for v in result[inner : inner + block_size]
                        )
                        result[inner : inner + block_size] = bytes(
                            min(block_size, len(data) - inner)
                        )
                    elif (
                        result[inner : inner + block_size]
                        != data[inner : inner + block_size]
                    ):
                        raise AssertionError("Allocated bytes changed")
                baseline.write(data)
                trimmed.write(result)
        return {
            "label": disk["label"],
            "before": str(before.resolve()),
            "after": str(after.resolve()),
            "sampleBytes": before.stat().st_size,
            "bitmapKnownPages": len(positions),
            "freePages": len(free),
            "nonzeroBytesCleared": changed_bytes,
            "journalReplayedOnCopy": metadata.get("needsRecovery", False),
            "allocatedPagesUnchanged": True,
        }, None
    finally:
        image.unlink(missing_ok=True)


def prepare(corpus, output):
    first = json.loads((corpus / "first.private.json").read_text())
    second = json.loads((corpus / "second.private.json").read_text())
    verified = verify(corpus, [first, second])
    later = {disk["label"]: disk for disk in second["disks"]}
    generations = [[], []]
    trim = {
        "label": "production-discard",
        "generations": [[]],
        "trim_generations": [],
        "notes": [
            "Allocation bitmaps verified with debugfs after journal-only recovery on private sparse copies.",
            "Only sampled pages in exported bitmap groups are eligible; missing groups remain unknown.",
            "Production filesystems were never mounted, trimmed or modified.",
        ],
    }
    summary = {
        "verifiedObjects": verified,
        "disks": 0,
        "regions": 0,
        "changedRegions": 0,
        "discardDisks": [],
        "discardUnavailable": [],
        "phaseIntervalsSeconds": [],
    }
    for disk in first["disks"]:
        current = later.get(disk["label"])
        if not current or disk["size"] != current["size"]:
            continue
        indices = sorted(set(disk["sampleIndices"]) & set(current["sampleIndices"]))
        if not indices:
            continue
        summary["disks"] += 1
        summary["regions"] += len(indices)
        summary["phaseIntervalsSeconds"].append(
            current["capturedAt"] - disk["capturedAt"]
        )
        summary["changedRegions"] += sum(
            disk["blocks"][str(i)]["hash"] != current["blocks"][str(i)]["hash"]
            for i in indices
        )
        for phase, snapshot in enumerate((disk, current)):
            path = output / f"{disk['label']}-g{phase}.private.bin"
            with path.open("xb") as stream:
                for index in indices:
                    stream.write(region(corpus, snapshot["blocks"][str(index)]))
            generations[phase].append(
                {"disk": disk["label"], "path": str(path.resolve())}
            )
        discarded, reason = discard_sample(corpus, disk, indices, output)
        if discarded:
            trim["generations"][0].append(
                {"disk": disk["label"], "path": discarded["before"]}
            )
            trim["trim_generations"].append(
                {"disk": disk["label"], "path": discarded["after"]}
            )
            summary["discardDisks"].append(
                {k: v for k, v in discarded.items() if k not in ("before", "after")}
            )
        else:
            summary["discardUnavailable"].append(
                {"label": disk["label"], "reason": reason}
            )
    regular = {
        "label": "production-cache",
        "generations": generations,
        "notes": [
            "Systematic regions from nine disks with available clean published-cache data; cache-biased, excludes uncached and unpublished dirty regions.",
            "Same physical region positions across both phases. Concatenation keeps 4 MiB boundaries but changes cross-disk offset hints; similarity search is a bounded heuristic.",
            "All copied bytes validated against manifest SHA-256/BLAKE3 identity; no raw workload or hash inventory is published.",
        ],
    }
    (output / "production.private.json").write_text(json.dumps(regular, indent=2))
    if trim["generations"][0]:
        (output / "discard.private.json").write_text(json.dumps(trim, indent=2))
        replayed = {entry["disk"]: entry for entry in trim["generations"][0]}
        cohort = {
            "label": "production-discard-cohort",
            "generations": [
                [replayed.get(entry["disk"], entry) for entry in generations[0]]
            ],
            "trim_generations": trim["trim_generations"],
            "notes": [
                "All sampled owners remain live; only owners with validated allocation are trimmed.",
                "Unknown owners retain their original bytes, including any shared bases.",
            ],
        }
        (output / "discard-cohort.private.json").write_text(
            json.dumps(cohort, indent=2)
        )
    (output / "sample-summary.json").write_text(json.dumps(summary, indent=2))
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        parser.error("Output must be a new private directory")
    os.umask(0o077)
    args.output.mkdir(mode=0o700, parents=True)
    summary = prepare(args.corpus, args.output)
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
