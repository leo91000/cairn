"""Exercise the native qualification RPC without Docker or host services."""
import json
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


class NativeChatFixtureTests(unittest.TestCase):
    def turn(self, script):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / 'codex'
            shutil.copyfile(Path(__file__).with_name('native_chat_fixture.py'), fixture)
            workload = root / 'workload'
            workload.write_text('#!/usr/bin/python3\n' + script)
            workload.chmod(0o755)
            child = subprocess.Popen([sys.executable, str(fixture), '--fixture-workload', workload.name],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            try:
                child.stdin.write(json.dumps(dict(id=1, method='turn/start')).encode() + b'\n')
                child.stdin.flush()
                frames, pending = [], b''
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if not select.select([child.stdout], [], [], max(0, deadline - time.monotonic()))[0]:
                        break
                    chunk = child.stdout.read1(65536)
                    if not chunk:
                        break
                    pending += chunk
                    while b'\n' in pending:
                        line, pending = pending.split(b'\n', 1)
                        frame = json.loads(line)
                        frames.append(frame)
                        if frame.get('method') == 'turn/completed':
                            self.assertEqual(frames[0]['id'], 1)
                            return frame['params']['turn']
                self.fail('Fixture failed to complete its RPC turn.')
            finally:
                child.stdin.close()
                try:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                child.stdout.close()

    def test_success_output_does_not_corrupt_rpc_frames(self):
        turn = self.turn("print('ordinary workload output')\n")
        self.assertEqual(turn['status'], 'completed')
        self.assertNotIn('error', turn)

    def test_failure_exposes_bounded_workload_diagnostic_tail(self):
        turn = self.turn("import sys\nsys.stderr.write('x' * 200000 + 'diagnostic-marker\\n')\nsys.exit(7)\n")
        self.assertEqual(turn['status'], 'failed')
        message = turn['error']['message']
        self.assertIn('failed: 7', message)
        self.assertIn('diagnostic-marker', message)
        self.assertLess(len(message), 16500)


if __name__ == '__main__':
    unittest.main()
