import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

from native_phase_overlay import instrument


class NativePhaseTests(unittest.TestCase):
    def test_blocked_main_thread_has_a_bounded_stack_without_local_values(self):
        with tempfile.TemporaryDirectory() as directory:
            secret = 'private-local-value-never-captured'
            code = '''
import time
from native_phase_diagnostics import NativePhaseDiagnostics
private_value = %r
observer = NativePhaseDiagnostics(%r, 'reference', interval=.03)
observer.enter('restored-measure-read')
def blocked_file_operation():
    private_payload = private_value
    time.sleep(3)
blocked_file_operation()
''' % (secret, directory)
            child = subprocess.Popen([sys.executable, '-c', code], cwd=Path(__file__).parent)
            try:
                target = Path(directory) / 'native-phase.json'
                deadline = time.monotonic() + 2
                while time.monotonic() < deadline:
                    if target.exists():
                        text = target.read_text()
                        data = json.loads(text)
                        if data['phase'] == 'restored-measure-read':
                            break
                    time.sleep(.01)
                else:
                    self.fail('No phase proof while the main operation was still blocked')
                self.assertIsNone(child.poll())
                self.assertNotIn(secret, text)
                self.assertLessEqual(len(text.encode()), 65537)
                main = next(stack for stack in data['pythonStacks'] if stack['mainThread'])
                self.assertIn('blocked_file_operation', [frame['function'] for frame in main['frames']])
                self.assertLessEqual(len(main['frames']), 12)
                self.assertTrue(all(set(frame) == {'file', 'line', 'function'} for frame in main['frames']))
            finally:
                child.terminate()
                child.wait(timeout=2)

    def test_both_roles_receive_identical_insertions_without_changing_workload(self):
        source = Path(__file__).with_name('native_runtime_test.py').read_text()
        for role in ('reference', 'candidate'):
            observed = instrument(source, role)
            self.assertIn('post-eviction-resume-complete', observed)
            self.assertIn('restored-measure-read', observed)
            self.assertIn('namespace-check-before', observed)
            with self.assertRaises(ValueError):
                instrument(observed, role)
        self.assertEqual(instrument(source, 'reference').replace("start_diagnostics('reference')", "start_diagnostics('candidate')"), instrument(source, 'candidate'))
        with self.assertRaises(ValueError):
            instrument(source.replace("progress('attempt-ready')", "progress('different')"), 'reference')


if __name__ == '__main__':
    unittest.main()
