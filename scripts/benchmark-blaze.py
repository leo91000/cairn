#!/usr/bin/env python3
"""Compare the current CI compiler, a matched Cargo compiler, and pinned Blaze."""

import json
import os
from pathlib import Path
import re
import shutil
import signal
import statistics
import subprocess
import tempfile
import threading
import time
import tomllib


SOURCE = Path("crates/installation/src/main.rs")
VERSION_LINE = 'println!("cairn {}", env!("CARGO_PKG_VERSION"));'
ENGINES = (
    ("cargo-ci", "1.97.1", False),
    ("cargo-matched", "1.99.0", False),
    ("blaze", "1.99.0", True),
)
RELEASE_SHA = "8b15e4b406f010f98eb456ec867e16e0a9ebb0d5"
RELEASE_DIGEST = "c5202bfb3c29920831149db9b7060c58e9876972aba3cbcd73e9e4ccaf818330"


def edited_source(original, marker):
    if original.count(VERSION_LINE) != 1:
        raise ValueError("The version probe must match exactly once; update the benchmark")
    replacement = VERSION_LINE.replace("cairn {}", f"cairn {{}}{marker}")
    return original.replace(VERSION_LINE, replacement)


def served_by_blaze(log):
    return bool(re.search(r"Finished[^\n]*\[ra-jit\]", log))


def command_output(command, env=None):
    return subprocess.check_output(command, env=env, text=True, timeout=30).strip()


def measured(command, env, log, timeout=1200):
    """GNU time covers the client and its children, not an already-running daemon."""
    metrics = log.with_suffix(".time")
    wrapped = ["/usr/bin/time", "-o", str(metrics), "-f", "%U %S %M", *command]
    started = time.monotonic()
    with log.open("w") as output:
        process = subprocess.Popen(
            wrapped, env=env, stdout=output, stderr=subprocess.STDOUT, start_new_session=True
        )
        expired = threading.Event()

        def terminate():
            expired.set()
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

        # wait(timeout=...) polls in 50 ms increments on POSIX, biasing fast builds.
        timer = threading.Timer(timeout, terminate)
        timer.daemon = True
        timer.start()
        try:
            code = process.wait()
        finally:
            timer.cancel()
        if expired.is_set():
            code = 124

    result = {
        "command": command,
        "wall_seconds": time.monotonic() - started,
        "exit_code": code,
        "log": log.name,
    }
    # GNU time adds a diagnostic before these numbers on failure.
    metric_lines = metrics.read_text().splitlines() if metrics.exists() else []
    if metric_lines:
        values = metric_lines[-1].split()
        if len(values) == 3:
            result.update(
                client_user_seconds=float(values[0]),
                client_system_seconds=float(values[1]),
                client_max_rss_kib=int(values[2]),
            )
    return result


def verify_binary(binary, expected, env):
    actual = command_output([str(binary), "--version"], env)
    if actual != expected:
        raise ValueError(f"Stale or incorrect binary: expected {expected!r}, got {actual!r}")
    return actual


def stop_daemon(env):
    subprocess.run(
        ["cargo", "ra-jit", "stop"], env=env, capture_output=True, text=True, timeout=30,
        check=True,
    )


def wait_ready(env, directory, repo):
    started = time.monotonic()
    deadline = started + 180
    while time.monotonic() < deadline:
        status = command_output(["cargo", "ra-jit", "status"], env)
        (directory / "status-ready.log").write_text(status)
        if f"    {repo}: ready" in status.splitlines():
            return time.monotonic() - started
        if "failed" in status.lower():
            raise RuntimeError(f"Blaze failed to load the workspace: {status}")
        time.sleep(1)
    raise TimeoutError("Blaze did not become ready in 180 seconds; see status-ready.log")


def run_engine(repo, output, name, toolchain, blaze):
    directory = output / name
    directory.mkdir()
    target = directory / "target"
    env = {
        **os.environ,
        "RUSTUP_TOOLCHAIN": toolchain,
        "CARGO_TARGET_DIR": str(target),
        "CARGO_PROFILE_DEV_DEBUG": "0",
        "CARGO_INCREMENTAL": "0",
        "CARGO_TERM_COLOR": "never",
    }
    daemon_home = None
    if blaze:
        # A Unix socket path must fit in 108 bytes, even in a deeply nested checkout.
        cache_root = Path.home() / ".cache"
        cache_root.mkdir(exist_ok=True)
        daemon_home = Path(tempfile.mkdtemp(prefix="cairn-blaze-", dir=cache_root))
        env["RA_JIT_HOME"] = str(daemon_home)
    command = ["cargo", *(["ra-jit"] if blaze else []), "build", "--locked", "--offline", "--bin", "cairn"]
    source = repo / SOURCE
    original_bytes = source.read_bytes()
    original = original_bytes.decode()
    version = tomllib.loads((repo / "crates/installation/Cargo.toml").read_text())["package"]["version"]
    result = {"name": name, "toolchain": toolchain, "rows": [], "rustc": command_output(["rustc", "-vV"], env)}
    phases = [("cold", ""), ("warm-noop", "")]
    phases.extend((f"edit-{sample}", f"-blaze-probe-{sample}") for sample in range(1, 4))
    phases.append(("restart-edit", "-blaze-probe-4"))
    try:
        for phase, marker in phases:
            if phase == "restart-edit" and blaze:
                stop_daemon(env)
            if marker:
                source.write_text(edited_source(original, marker))

            print(f"{name}: {phase}", flush=True)
            row = measured(command, env, directory / f"{phase}.log")
            row["phase"] = phase
            row["blaze_fast_path"] = blaze and served_by_blaze((directory / row["log"]).read_text())
            result["rows"].append(row)
            if row["exit_code"] != 0:
                raise RuntimeError(f"{phase} failed with exit {row['exit_code']}; see {row['log']}")
            row["verified_output"] = verify_binary(target / "debug/cairn", f"cairn {version}{marker}", env)
            if blaze:
                status = command_output(["cargo", "ra-jit", "status"], env)
                (directory / f"{phase}-status.log").write_text(status)
                if phase == "cold":
                    result["daemon_warmup_seconds"] = wait_ready(env, directory, repo)
        result["status"] = "completed"
    except Exception as error:
        result["status"] = "failed"
        result["error"] = str(error)
        print(f"{name}: {error}", flush=True)
    finally:
        source.write_bytes(original_bytes)
        if blaze:
            try:
                stop_daemon(env)
            except Exception as error:
                result["cleanup_error"] = str(error)
                result["status"] = "failed"
            if (daemon_home / "daemon.log").exists():
                shutil.copyfile(daemon_home / "daemon.log", directory / "daemon.log")
            for worker_log in daemon_home.glob("spares/*/log"):
                shutil.copyfile(worker_log, directory / f"worker-{worker_log.parent.name}.log")
            if "cleanup_error" not in result:
                shutil.rmtree(daemon_home)
        if target.exists():
            result["target_allocated_bytes"] = int(command_output(["du", "-s", "--block-size=1", str(target)]).split()[0])
            shutil.rmtree(target)
        (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def summary(results, metadata):
    lines = [
        "# Blaze build experiment", "",
        f"Commit: `{metadata['commit']}`. Sequential builds on the same host; targets start empty for each engine.", "",
        "| Engine | Phase | Wall (s) | Correct output | Build path |",
        "| --- | --- | ---: | --- | --- |",
    ]
    for result in results:
        for row in result["rows"]:
            path = "Blaze" if row["blaze_fast_path"] else "Cargo"
            verified = "yes" if "verified_output" in row else "no"
            lines.append(f"| {result['name']} | {row['phase']} | {row['wall_seconds']:.3f} | {verified} | {path} |")
        if "daemon_warmup_seconds" in result:
            lines.extend(["", f"Blaze load after cold build: {result['daemon_warmup_seconds']:.3f} s (additional wall time).", ""])
        if result["status"] != "completed":
            lines.extend(["", f"**{result['name']} failed:** {result.get('error', result.get('cleanup_error'))}", ""])

    eligible = {}
    for result in results:
        edits = [row for row in result["rows"] if row["phase"].startswith("edit-")]
        verified = len(edits) == 3 and all(row.get("verified_output") for row in edits)
        fast = result["name"] != "blaze" or all(row["blaze_fast_path"] for row in edits)
        if verified and fast and result["status"] == "completed":
            eligible[result["name"]] = statistics.median(row["wall_seconds"] for row in edits)

    lines.append("")
    if "blaze" in eligible:
        for baseline in ("cargo-ci", "cargo-matched"):
            if baseline in eligible:
                ratio = eligible[baseline] / eligible["blaze"]
                lines.append(f"Median of three verified body edits: {ratio:.2f}× speedup versus {baseline} (baseline / Blaze wall time).")
    else:
        lines.append("No demonstrated Blaze speedup: all three edits must use Blaze and produce the changed output. Inspect logs and fallback counters.")
    lines.extend([
        "", "These are local build times, not a measured reduction in the complete CI pipeline.",
        "Cold excludes install/fetch; registry and OS caches are warm. Warm edits retain a live daemon.",
        "Restart retains disk state but stops the daemon; cache transfer costs are excluded.",
        "GNU time CPU/RSS measures the client process tree and excludes the persistent daemon; do not compare it as total engine CPU/RAM.",
        "Fixed order and three samples are exploratory. Repeat runs before drawing conclusions.",
        "Existing stable Clippy/tests remain authoritative; these binaries are never deployed.", "",
    ])
    return "\n".join(lines)


def main():
    repo = Path(__file__).resolve().parents[1]
    os.chdir(repo)
    output = repo / ".data/blaze-benchmark"
    output.mkdir(parents=True, exist_ok=False)
    metadata = {
        "commit": command_output(["git", "rev-parse", "HEAD"]),
        "host": command_output(["uname", "-a"]),
        "cpu": command_output(["lscpu"]),
        "memory": Path("/proc/meminfo").read_text(),
        "os": Path("/etc/os-release").read_text(),
        "disk": command_output(["df", "-h", str(repo)]),
        "image_os": os.environ.get("ImageOS"),
        "image_version": os.environ.get("ImageVersion"),
        "blaze_version": command_output(["ra-jit", "--version"]),
        "blaze_release_tag_source": RELEASE_SHA,
        "blaze_archive_sha256": RELEASE_DIGEST,
        "flags": {"CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_INCREMENTAL": "0"},
    }
    (output / "host.json").write_text(json.dumps(metadata, indent=2) + "\n")
    results = []
    for engine in ENGINES:
        results.append(run_engine(repo, output, *engine))
        (output / "summary.md").write_text(summary(results, metadata))
    return 0 if all(result["status"] == "completed" for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
