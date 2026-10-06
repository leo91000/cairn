"""Exercise the shipped supervisor CLI against the master protocol and a fixture Docker CLI."""
import http.server
import importlib.util
import json
import os
from pathlib import Path
import queue
import signal
import shlex
import stat
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

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
    def test_snapshot_options_survive_launch_and_can_be_disabled_without_changing_layout(self):
        spec = importlib.util.spec_from_file_location('node_supervisor', SOURCE)
        host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(host)
        self.assertEqual(host.vm_arguments({}), [])
        config = {'blockTransport': 'ublk', 'vmSnapshots': True}
        self.assertEqual(host.vm_arguments(config), ['-e', 'LEO_DISK_LAYOUT=paired-ext4-v1', '-e', 'LEO_VM_SNAPSHOTS=true'])
        config['readyVmPoolSize'] = 4
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'data/node').mkdir(parents=True)
            (root / 'data/node/identity.json').write_text(json.dumps({'nodeId': 'fixture-node'}))
            (root / 'config.json').write_text(json.dumps(config))
            with patch.object(host, 'ROOT', root), patch.object(host, 'remove'), patch.object(host, 'block_device_arguments', return_value=[]), patch.object(host, 'command') as command:
                host.launch(NEW)
            launch = command.call_args_list[0].args[0]
            self.assertIn('LEO_DISK_LAYOUT=paired-ext4-v1', launch)
            self.assertIn('LEO_VM_SNAPSHOTS=true', launch)
            self.assertIn('LEO_READY_VM_POOL_SIZE=4', launch)
        config.update({'vmSnapshots': False, 'diskLayout': 'paired-ext4-v1'})
        self.assertEqual(host.vm_arguments(config), ['-e', 'LEO_READY_VM_POOL_SIZE=4', '-e', 'LEO_DISK_LAYOUT=paired-ext4-v1', '-e', 'LEO_VM_SNAPSHOTS=false'])

    def test_incompatible_snapshot_configuration_never_removes_the_running_node(self):
        spec = importlib.util.spec_from_file_location('node_supervisor', SOURCE)
        host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(host)
        for config in [{'readyVmPoolSize': value} for value in [0, 5, True, '4', 1.5]] + [{'vmSnapshots': True}, {'diskLayout': 'paired-ext4-v1'}, {'vmSnapshots': 'true'}, {'vmSnapshots': 1}, {'diskLayout': 'invalid'}, {'blockTransport': 'ublk', 'vmSnapshots': True, 'diskLayout': 'flat-ext4-v1'}]:
            with self.subTest(config=config), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'config.json').write_text(json.dumps(config))
                with patch.object(host, 'ROOT', root), patch.object(host, 'remove') as remove:
                    with self.assertRaises(ValueError):
                        host.launch(NEW)
                remove.assert_not_called()

    def test_ublk_permissions_use_detected_majors_without_privileged_mode(self):
        spec = importlib.util.spec_from_file_location('node_supervisor', SOURCE)
        host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(host)
        devices = 'Character devices:\n 507 ublk-char\n\nBlock devices:\n 260 blkext\n'
        control = SimpleNamespace(st_mode=stat.S_IFCHR, st_rdev=os.makedev(10, 261))

        def read(path):
            return '0' if path.name == 'io_uring_disabled' else devices

        with patch.object(host, 'command') as command, patch.object(Path, 'is_dir', return_value=False), patch.object(Path, 'stat', return_value=control), patch.object(Path, 'read_text', read):
            arguments = host.block_device_arguments('ublk')
        self.assertEqual(command.call_args.args[0], ['modprobe', 'ublk_drv'])
        self.assertEqual(arguments, ['--cap-add=SYS_RESOURCE', '--device=/dev/ublk-control', '--device-cgroup-rule=c 507:* rwm', '--device-cgroup-rule=b 260:* rwm', '-e', 'LEO_BLOCK_TRANSPORT=ublk'])
        self.assertEqual(host.block_device_arguments('vhost-user'), [])

        with patch.object(host, 'command'), patch.object(Path, 'stat', return_value=control), patch.object(Path, 'read_text', lambda path: '2' if path.name == 'io_uring_disabled' else devices):
            with self.assertRaisesRegex(RuntimeError, 'requires io_uring'):
                host.block_device_arguments('ublk')
        with patch.object(host, 'command'), patch.object(Path, 'stat', return_value=control), patch.object(Path, 'read_text', return_value=''):
            with self.assertRaisesRegex(RuntimeError, 'Missing ublk device classes'):
                host.block_device_arguments('ublk')
        with self.assertRaisesRegex(ValueError, 'Unsupported block transport'):
            host.block_device_arguments('unknown')

    def test_loaded_ublk_does_not_require_the_running_kernels_module_files(self):
        spec = importlib.util.spec_from_file_location('node_supervisor', SOURCE)
        host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(host)
        control = SimpleNamespace(st_mode=stat.S_IFCHR, st_rdev=os.makedev(10, 261))
        devices = 'Character devices:\n 507 ublk-char\n\nBlock devices:\n 260 blkext\n'

        def read(path):
            return '0' if path.name == 'io_uring_disabled' else devices

        with patch.object(host, 'command', side_effect=RuntimeError('Running kernel modules were upgraded')) as command, patch.object(Path, 'is_dir', return_value=True), patch.object(Path, 'stat', return_value=control), patch.object(Path, 'read_text', read):
            arguments = host.block_device_arguments('ublk')

        command.assert_not_called()
        self.assertIn('--device=/dev/ublk-control', arguments)
        self.assertIn('--device-cgroup-rule=c 507:* rwm', arguments)
        self.assertIn('--device-cgroup-rule=b 260:* rwm', arguments)

    def test_ublk_preflight_does_not_remove_a_running_node(self):
        spec = importlib.util.spec_from_file_location('node_supervisor', SOURCE)
        host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(host)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'config.json').write_text(json.dumps({'blockTransport': 'ublk'}))
            with patch.object(host, 'ROOT', root), patch.object(host, 'remove') as remove, patch.object(host, 'block_device_arguments', side_effect=RuntimeError('Kernel lacks ublk')):
                with self.assertRaisesRegex(RuntimeError, 'Kernel lacks ublk'):
                    host.launch(NEW)
            remove.assert_not_called()

    def test_health_checks_accept_idle_on_demand_nodes(self):
        spec = importlib.util.spec_from_file_location('node_supervisor', SOURCE)
        host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(host)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'data/node').mkdir(parents=True)
            (root / 'data/node/identity.json').write_text(json.dumps({'nodeId': 'fixture-node'}))
            with patch.object(host, 'ROOT', root), patch.object(host, 'remove'), patch.object(host, 'command') as command:
                host.launch(NEW)
            launch = command.call_args_list[0].args[0]
            probe = command.call_args_list[1].args[0][3:]
            self.assertIn('--health-cmd', launch)
            self.assertEqual(shlex.split(launch[launch.index('--health-cmd') + 1]), probe)
            health = {'status': 'ok', 'nodeProtocol': 2, 'pool': {'capacity': 4, 'ready': 0, 'occupied': 0}}
            shim = 'globalThis.fetch=async()=>({ok:true,json:async()=>JSON.parse(process.argv[1])});'
            for value, expected in [(health, 0), ({**health, 'nodeProtocol': 1}, 1), ({**health, 'status': 'error'}, 1), ({**health, 'pool': {'capacity': 0}}, 1)]:
                result = subprocess.run(['node', '-e', shim + probe[-1], json.dumps(value)], capture_output=True)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

    def scenario(self, fail, lost_completion=False, retained=False, cache=True):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'data/node').mkdir(parents=True)
            (root / 'data/node/identity.json').write_text(json.dumps({'nodeId': 'fixture-node', 'token': 'fixture-token'}))
            (root / 'docker').write_text(DOCKER)
            (root / 'docker').chmod(0o755)
            (root / 'docker.json').write_text(json.dumps({'Config': {'Image': OLD, 'Labels': {'dev.leo.node.owner': 'fixture-node'}}, 'State': {'Running': True}}))
            completed = queue.Queue()
            completion_attempts = []
            runtime_requests = []

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
                    if value['action'] == 'runtimes':
                        runtime_requests.append(value)
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
            (root / 'config.json').write_text(json.dumps({'image': OLD, 'master': f'http://127.0.0.1:{server.server_port}', 'cacheApprovedRuntimes': cache}))
            env = {**os.environ, 'PATH': str(root) + os.pathsep + os.environ['PATH'], 'LEO_NODE_ROOT': str(root), 'FAIL_CANDIDATE': str(int(fail)), 'FIXTURE_SUPERVISOR': str(SOURCE)}
            process = subprocess.Popen([sys.executable, str(SOURCE), 'run'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                value = completed.get(timeout=40)
                if retained and cache:
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
                if not cache:
                    self.assertEqual(runtime_requests, [])
                    self.assertFalse(any('--network=none' in cmd for cmd in commands))
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)

    def test_fetches_retained_runtime_from_its_approved_digest(self):
        self.scenario(False, retained=True)

    def test_disabled_runtime_preloading_keeps_the_healthy_node_without_downloading_old_images(self):
        self.scenario(False, retained=True, cache=False)

    def test_success_commits_new_digest_and_stops_cleanly(self):
        self.scenario(False)

    def test_retries_lost_completion_without_restarting_the_healthy_container(self):
        self.scenario(False, lost_completion=True)

    def test_failed_candidate_rolls_back_and_remembers_failed_digest(self):
        self.scenario(True)


if __name__ == '__main__':
    unittest.main()
