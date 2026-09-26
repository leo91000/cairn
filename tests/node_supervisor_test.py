"""Exercise the shipped supervisor CLI against the master protocol and a fixture Docker CLI."""
import http.server
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'deploy/nodes/host.py'
OLD = 'registry.example/leo@sha256:' + '1' * 64
NEW = 'registry.example/leo@sha256:' + '2' * 64
DOCKER = r'''#!/usr/bin/env python3
import json,os,sys,pathlib,shutil
root=pathlib.Path(os.environ['LEO_NODE_ROOT']);state=root/'docker.json';args=sys.argv[1:]
with open(root/'commands.jsonl','a') as f:f.write(json.dumps(args)+'\n')
value=json.loads(state.read_text()) if state.exists() else None
if args[0]=='inspect':
 if value is None:sys.exit(1)
 print(json.dumps([value]))
elif args[0]=='rm':
 state.unlink(missing_ok=True)
elif args[0]=='run' and '--network=none' in args:
 cache=root/'state/images/retained-runtime';cache.mkdir(parents=True);(cache/'root.ext4').write_bytes(b'fixture');(cache/'vmlinux').write_bytes(b'fixture')
elif args[0]=='run':
 image=args[-3];healthy=not (image.endswith('2'*64) and os.environ.get('FAIL_CANDIDATE')=='1')
 state.write_text(json.dumps({'Config':{'Image':image,'Labels':{'dev.leo.node.owner':'fixture-node'}},'State':{'Running':healthy}}))
elif args[0]=='exec':
 sys.exit(0 if value and value['State']['Running'] else 1)
elif args[0]=='cp':
 shutil.copyfile(os.environ['FIXTURE_SUPERVISOR'],args[-1])
elif args[0]=='stop':
 value['State']['Running']=False;state.write_text(json.dumps(value))
'''


class Supervisor(unittest.TestCase):
    def scenario(self, fail, lost_completion=False, retained=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'data/node').mkdir(parents=True)
            (root / 'data/node/identity.json').write_text(json.dumps({'nodeId': 'fixture-node', 'token': 'fixture-token'}))
            (root / 'docker').write_text(DOCKER)
            (root / 'docker').chmod(0o755)
            (root / 'docker.json').write_text(json.dumps({'Config': {'Image': OLD, 'Labels': {'dev.leo.node.owner': 'fixture-node'}}, 'State': {'Running': True}}))
            completed = queue.Queue()
            completion_attempts = []

            class Master(http.server.BaseHTTPRequestHandler):
                def log_message(self, *_):
                    pass

                def do_GET(self):
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(json.dumps({'image': NEW, 'protocol': 2}).encode())

                def do_POST(self):
                    if self.headers.get('Authorization') != 'Bearer fixture-token':
                        self.send_response(401)
                        self.end_headers()
                        return
                    value = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    if value['action'] == 'complete' and (value['image'] == NEW or value.get('error')):
                        completion_attempts.append(value)
                        if lost_completion and len(completion_attempts) == 1:
                            self.send_response(503)
                            self.end_headers()
                            return
                        completed.put(value)
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(json.dumps({'runtimes': [{'runtimeId': 'retained-runtime', 'image': OLD}]} if value['action'] == 'runtimes' and retained else {'maintenance': 'ready-to-update'}).encode())

            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Master)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            (root / 'config.json').write_text(json.dumps({'image': OLD, 'master': f'http://127.0.0.1:{server.server_port}'}))
            env = {**os.environ, 'PATH': str(root) + os.pathsep + os.environ['PATH'], 'LEO_NODE_ROOT': str(root), 'FAIL_CANDIDATE': str(int(fail)), 'FIXTURE_SUPERVISOR': str(SOURCE)}
            process = subprocess.Popen([sys.executable, str(SOURCE), 'run'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                value = completed.get(timeout=40)
                if retained:
                    deadline = time.monotonic() + 10
                    while not (root / 'state/images/retained-runtime/root.ext4').exists():
                        self.assertLess(time.monotonic(), deadline)
                        time.sleep(0.05)
                process.send_signal(signal.SIGTERM)
                output, errors = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 0, errors.decode())
                self.assertNotIn(b'fixture-token', output + errors)
                config = json.loads((root / 'config.json').read_text())
                self.assertEqual(config['image'], OLD if fail else NEW)
                self.assertEqual(config.get('failedImage'), NEW if fail else None)
                self.assertEqual(bool(value.get('error')), fail)
                commands = [json.loads(line) for line in (root / 'commands.jsonl').read_text().splitlines()]
                launches = [cmd[-3] for cmd in commands if cmd[0] == 'run' and '--network=none' not in cmd]
                self.assertEqual(launches, [NEW, OLD] if fail else [NEW])
                self.assertTrue(any(cmd[0] == 'stop' for cmd in commands))
                self.assertFalse(any('fixture-token' in ' '.join(cmd) for cmd in commands))
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)

    def test_fetches_retained_runtime_from_its_approved_digest(self):
        self.scenario(False, retained=True)

    def test_success_commits_new_digest_and_stops_cleanly(self):
        self.scenario(False)

    def test_retries_lost_completion_without_restarting_the_healthy_container(self):
        self.scenario(False, lost_completion=True)

    def test_failed_candidate_rolls_back_and_remembers_failed_digest(self):
        self.scenario(True)


if __name__ == '__main__':
    unittest.main()
