import { execFileSync } from 'node:child_process'
import { it } from 'vitest'

it('network bench stops its writers before removing their temporary directory, including on failure', () => {
  execFileSync('python3', ['-B', '-c', `
import importlib.util
import io
import sys

spec = importlib.util.spec_from_file_location("network_bench", "tests/network-bench.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)
closed = []

class Network:
    def close(self):
        assert self.directory.is_dir(), "temporary directory removed while writers are still active"
        (self.directory / "last-write").write_text("writer stopped")
        closed.append(self.directory)

def run(*args):
    if args[0] == "rustc":
        raise RuntimeError("fixture setup failed")
    return ""

# Exercise main's cleanup without namespaces, sudo, or a shared lock.
bench.Network = Network
bench.run = run
bench.open = lambda *args: io.StringIO()
bench.fcntl.flock = lambda *args: None
sys.argv = ["network-bench.py", "same-lan", "--probe-only", "--output", "unused-network-report.json"]
try:
    bench.main()
except RuntimeError as error:
    assert str(error) == "fixture setup failed", str(error)
else:
    raise AssertionError("fixture failure should be propagated")
assert len(closed) == 1, "network resources must be closed"
assert not closed[0].exists(), "temporary directory must be removed after cleanup"
`], { stdio: 'pipe' })
})
