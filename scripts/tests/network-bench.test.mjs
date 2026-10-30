import { execFileSync } from 'node:child_process'
import { it } from 'vitest'

it('android process failure evidence keeps crash categories and frames without raw messages or credentials', () => {
  execFileSync('python3', ['-B', '-c', `
import importlib.util
import json
spec = importlib.util.spec_from_file_location("network_bench", "tests/network-bench.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)
raw = '''E/AndroidRuntime(123): FATAL EXCEPTION: main
E/AndroidRuntime(123): Process: build.cairn.app, PID: 123
E/AndroidRuntime(123): java.lang.IllegalStateException: private-fixture-cookie body=secret
E/AndroidRuntime(123):     at org.webrtc.PeerConnection.dispose(PeerConnection.java:546)
E/AndroidRuntime(123): Caused by: java.lang.ExceptionInInitializerError: private-fixture-cookie
E/AndroidRuntime(123):     at org.webrtc.PeerConnection.<clinit>(PeerConnection.java:44)
E/AndroidRuntime(123):     at org.webrtc.PeerConnection.<init>(PeerConnection.java:45)
E/AndroidRuntime(123): Caused by: java.lang.Throwable: private-fixture-cookie
E/AndroidRuntime(123): Caused by: build.cairn.app.data.DirectRequestTimeout: private-fixture-cookie
F/DEBUG(321): Cmdline: build.cairn.app
F/DEBUG(321): signal 6 (SIGABRT), code -1 (SI_QUEUE)
F/DEBUG(321): Abort message: 'private-fixture-cookie SDP=secret'
F/DEBUG(321): #00 pc 0000000000052450  /apex/com.android.runtime/lib64/bionic/libc.so (abort+152)
F/DEBUG(321): #01 pc 0000000000052451  /data/app/private-path/base.apk (offset 0x12000) (webrtc::Dispose()+42) (BuildId: abcd)
'''
exit_info = '''ApplicationExitInfo #0:
 process=build.cairn.app reason=5 (APP CRASH(NATIVE)) subreason=0 (UNKNOWN) status=6
 description=private-fixture-cookie
ApplicationExitInfo #1:
 process=build.cairn.app
 reason=4 (APP CRASH(EXCEPTION))
 subreason=0 (UNKNOWN)
 status=0
'''
evidence = bench.android_crash_summary(raw, exit_info)
assert evidence['exceptions'] == ['java.lang.IllegalStateException', 'java.lang.ExceptionInInitializerError', 'java.lang.Throwable', 'build.cairn.app.data.DirectRequestTimeout'], evidence
assert evidence['javaFrames'] == ['org.webrtc.PeerConnection.dispose(PeerConnection.java:546)', 'org.webrtc.PeerConnection.<clinit>(PeerConnection.java:44)', 'org.webrtc.PeerConnection.<init>(PeerConnection.java:45)'], evidence
assert evidence['signals'] == ['SIGABRT'], evidence
assert evidence['nativeFrames'] == [{'pc': '0000000000052450', 'library': 'libc.so', 'symbol': 'abort+152'}, {'pc': '0000000000052451', 'library': 'base.apk', 'symbol': 'webrtc::Dispose()+42'}], evidence
assert evidence['exitReasons'] == [{'reason': 5, 'category': 'CRASH_NATIVE', 'status': 6}, {'reason': 4, 'category': 'CRASH', 'status': 0}], evidence
encoded = json.dumps(evidence)
assert 'private-fixture' not in encoded and 'secret' not in encoded and '/apex/' not in encoded, encoded
bounded = bench.android_crash_summary(raw * 1000, exit_info * 1000)
assert len(bounded['javaFrames']) <= 32 and len(bounded['nativeFrames']) <= 32
assert len(bounded['exitReasons']) <= 8
`], { stdio: 'pipe' })
})

it('android instrumentation keeps the first process crash and collects diagnostics without masking ADB failures', () => {
  execFileSync('python3', ['-B', '-c', `
import importlib.util
import json
import subprocess
import tempfile
from pathlib import Path
from types import SimpleNamespace
spec = importlib.util.spec_from_file_location("network_bench", "tests/network-bench.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)
first = 'INSTRUMENTATION_RESULT: shortMsg=Process crashed.\\nINSTRUMENTATION_CODE: 0\\n'
calls = []
def adb(*args, **options):
    calls.append(args)
    if args == ('logcat', '-b', 'crash', '-c'):
        assert options.get('timeout') == 5, options
    if args[:3] == ('shell', 'am', 'instrument'):
        return SimpleNamespace(stdout=first)
    if args[:3] == ('logcat', '-b', 'crash') and '-d' in args:
        return SimpleNamespace(stdout='E/AndroidRuntime(1): Process: build.cairn.app, PID: 1\\nE/AndroidRuntime(1): java.lang.IllegalStateException: secret')
    if 'exit-info' in args:
        raise subprocess.TimeoutExpired('adb', 5)
    return SimpleNamespace(stdout='')
with tempfile.TemporaryDirectory() as directory:
    output = Path(directory) / 'same-lan.json'
    try:
        bench.android_instrumentation(adb, 'same-lan', output)
    except RuntimeError as error:
        assert str(error) == 'Native Android transport scenario failed; see instrumentation result'
    else:
        raise AssertionError('Process crash must fail the qualification')
    assert output.with_suffix('.instrumentation.txt').read_text() == first
    evidence = json.loads(output.with_suffix('.crash.json').read_text())
    assert evidence['exceptions'] == ['java.lang.IllegalStateException'], evidence
    assert evidence['collectionErrors'] == ['exit-info-unavailable'], evidence
    assert 'secret' not in json.dumps(evidence)
    assert calls[0] == ('logcat', '-b', 'crash', '-c'), calls
    def offline(*args, **options):
        raise subprocess.CalledProcessError(1, ['adb'], stderr='secret')
    try:
        bench.android_instrumentation(offline, 'same-lan', output)
    except subprocess.CalledProcessError:
        pass
    else:
        raise AssertionError('ADB failure must propagate')
    evidence = json.loads(output.with_suffix('.crash.json').read_text())
    assert evidence['collectionErrors'] == ['crash-log-unavailable', 'exit-info-unavailable'], evidence
`], { stdio: 'pipe' })
})

it('diagnostic artifact write failures never replace the initial instrumentation failure', () => {
  execFileSync('python3', ['-B', '-c', `
import importlib.util
import subprocess
import tempfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
spec = importlib.util.spec_from_file_location("network_bench", "tests/network-bench.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)
primary = subprocess.TimeoutExpired('instrumentation', 360)
def adb(*args, **options):
    if args[:3] == ('shell', 'am', 'instrument'):
        raise primary
    return SimpleNamespace(stdout='')
original_write = Path.write_text
def write(self, *args, **options):
    if self.name.endswith('.crash.json'):
        raise OSError('synthetic evidence disk full')
    return original_write(self, *args, **options)
with tempfile.TemporaryDirectory() as directory, patch.object(Path, 'write_text', write):
    output = Path(directory) / 'same-lan.json'
    try:
        bench.android_instrumentation(adb, 'same-lan', output)
    except BaseException as error:
        assert error is primary, type(error)
    else:
        raise AssertionError('The original timeout must propagate')
    def crashed(*args, **options):
        return SimpleNamespace(stdout='shortMsg=Process crashed.')
    try:
        bench.android_instrumentation(crashed, 'same-lan', output)
    except RuntimeError as error:
        assert str(error) == 'Native Android transport scenario failed; see instrumentation result'
    else:
        raise AssertionError('The original process crash must fail the qualification')
    storage_error = OSError('synthetic evidence disk full')
    with patch.object(Path, 'write_text', side_effect=storage_error), patch.object(bench, 'android_failure_evidence', wraps=bench.android_failure_evidence) as collect:
        try:
            bench.android_instrumentation(crashed, 'same-lan', output)
        except RuntimeError as error:
            assert str(error) == 'Native Android transport scenario failed; see instrumentation result'
        else:
            raise AssertionError('Transcript failure must not replace the process crash')
        collect.assert_called_once()
        def passed(*args, **options):
            return SimpleNamespace(stdout='OK (1 test)')
        try:
            bench.android_instrumentation(passed, 'same-lan', output)
        except OSError as error:
            assert error is storage_error
        else:
            raise AssertionError('Missing success evidence must still fail qualification')
        collect.assert_called_once()
`], { stdio: 'pipe' })
})

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
