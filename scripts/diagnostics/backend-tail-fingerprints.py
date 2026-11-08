"""Throwaway diagnostic collector. Does not invoke Cargo or execute test binaries."""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import time


NAME = re.compile(r"[A-Za-z0-9_+.-]{1,180}\Z")
FEATURE = re.compile(r"[A-Za-z0-9_+./?=-]{1,180}\Z")
MARKER = re.compile(rb"[0-9a-f]{16}\Z")
MAX_BYTES = 65536


def read_regular(location):
    descriptor = os.open(location, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_BYTES:
            raise ValueError("unsupported_fingerprint_file")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            data = source.read(MAX_BYTES + 1)
        if len(data) > MAX_BYTES:
            raise ValueError("oversized_fingerprint_file")
        return data
    finally:
        os.close(descriptor)


def strings(value):
    # Synthetic run-build-script fingerprints use an empty string.
    parsed = [] if value == "" else json.loads(value) if isinstance(value, str) else value
    if not isinstance(parsed, list) or len(parsed) > 1024:
        raise ValueError("invalid_feature_list")
    if not all(isinstance(item, str) and FEATURE.fullmatch(item) for item in parsed):
        raise ValueError("invalid_feature_name")
    return sorted(parsed)


def project(data):
    source = json.loads(data)
    result = {"jsonSha256": hashlib.sha256(data).hexdigest()}
    for name in ("rustc", "target", "profile", "path", "config", "compile_kind"):
        value = source.get(name)
        if type(value) is not int or not 0 <= value < 2**64:
            raise ValueError("invalid_fingerprint_number")
        # Preserve Cargo's complete u64 values, including values above 2**53.
        result[name] = str(value)
    result["features"] = strings(source["features"])
    result["declaredFeatures"] = strings(source["declared_features"])
    dependencies = source["deps"]
    if not isinstance(dependencies, list) or len(dependencies) > 2048:
        raise ValueError("invalid_dependency_list")
    result["deps"] = []
    for item in dependencies:
        if (not isinstance(item, list) or len(item) != 4
                or type(item[0]) is not int or not 0 <= item[0] < 2**64
                or not isinstance(item[1], str) or not NAME.fullmatch(item[1])
                or type(item[2]) is not bool
                or type(item[3]) is not int or not 0 <= item[3] < 2**64):
            raise ValueError("invalid_dependency")
        result["deps"].append([str(item[0]), item[1], item[2], str(item[3])])
    flags = source.get("rustflags")
    if not isinstance(flags, list) or not all(isinstance(item, str) for item in flags):
        raise ValueError("invalid_rustflags")
    # Flags and unknown fields may contain arbitrary strings. Never emit them.
    result["rustflagsEmpty"] = not flags
    result["rustflagsSha256"] = hashlib.sha256(json.dumps(flags).encode()).hexdigest()
    return result


def snapshot(root):
    started = time.monotonic()
    root = Path(root)
    if root.is_symlink() or not root.is_dir():
        raise ValueError("invalid_fingerprint_root")
    records = {}
    for index, directory in enumerate(root.iterdir()):
        if index >= 20000 or time.monotonic() - started > 15:
            raise ValueError("observation_budget_exhausted")
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError("invalid_fingerprint_directory")
        if not NAME.fullmatch(directory.name):
            raise ValueError("invalid_fingerprint_name")
        for file_index, location in enumerate(directory.iterdir()):
            if file_index >= 1000 or time.monotonic() - started > 15:
                raise ValueError("observation_budget_exhausted")
            if location.suffix != ".json":
                continue
            if not NAME.fullmatch(location.name):
                raise ValueError("invalid_fingerprint_name")
            record = project(read_regular(location))
            marker = location.with_suffix("")
            try:
                data = read_regular(marker)
                if not MARKER.fullmatch(data):
                    raise ValueError("invalid_fingerprint_marker")
                record["cargoFingerprint"] = data.decode("ascii")
            except FileNotFoundError:
                record["cargoFingerprint"] = None
            records[f"{directory.name}/{location.name}"] = record
    return {"schemaVersion": 1, "qualification": False, "fingerprints": records}


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("Usage: backend-tail-fingerprints.py FINGERPRINT_ROOT OUTPUT.json")
    try:
        observation = snapshot(sys.argv[1])
        with open(sys.argv[2], "x", encoding="utf8") as destination:
            os.chmod(sys.argv[2], 0o600)
            json.dump(observation, destination, sort_keys=True)
            destination.write("\n")
    except (OSError, ValueError, KeyError, TypeError):
        # No exception text: paths and malformed fields are deliberately private.
        raise SystemExit("BACKEND_TAIL_FINGERPRINT_OBSERVATION_FAILED") from None
