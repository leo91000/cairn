import ast
import base64
import copy
import io
import json
from pathlib import Path
import subprocess
import sys
import symtable
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import Mock

from native_phase_overlay import OBSERVER, instrument


def replay_readiness_boundary(source, role, observed=True, failed=False):
    """Execute the real nested readiness/failure block; replace only its I/O.

    The full helper's symbol table is retained separately below. This slice
    avoids admission/systemd/S3 setup, but keeps the complete original failure
    branch, including its local assignment, redaction, evidence and exception.
    """
    text = instrument(source, role) if observed else source
    tree = ast.parse(text)
    main = copy.deepcopy(next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'main'))
    start = copy.deepcopy(next(node for node in ast.walk(main) if isinstance(node, ast.FunctionDef) and node.name == 'start'))
    first = next(index for index, node in enumerate(start.body)
                 if isinstance(node, ast.Expr) and ast.unparse(node.value) == "progress('attempt-ready')")
    last = next(index for index, node in enumerate(start.body)
                if isinstance(node, ast.Assign) and ast.unparse(node.targets[0]) == 'startup_cpu_ms')
    start.body = start.body[first:last + 1] + [ast.Return(value=ast.Constant(value='ready'))]
    start.args = ast.arguments(posonlyargs=[], args=[], kwonlyargs=[], kw_defaults=[], defaults=[])
    main.body = []
    if observed:
        main.body.append(copy.deepcopy(next(node for node in ast.walk(tree)
                              if isinstance(node, ast.Assign) and isinstance(node.value, ast.Call)
                              and ast.unparse(node.value.func) == 'start_diagnostics')))
    main.body += [start, ast.Return(value=ast.Name(id='start', ctx=ast.Load()))]
    module = ast.fix_missing_locations(ast.Module(body=[main], type_ignores=[]))
    observer = SimpleNamespace(watch=Mock())
    commands = Mock(return_value=SimpleNamespace(stdout='test-private-secret test-private-token test-private-grant', stderr=''))
    stderr = io.StringIO()
    phases = []
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        if failed:
            (root / 'attempt.exit').write_text('{"StatusCode": 9}')
            (root / 'attempt.log').write_text(json.dumps(dict(data=base64.b64encode(b'test-private-secret failure').decode())) + '\n')
        namespace = dict(start_diagnostics=lambda selected: observer, progress=phases.append,
                         supervisor='fixture-supervisor.service', attempt='attempt', state=root,
                         read_controller_result=lambda path: json.loads(path.read_text()),
                         base64=base64, json=json, SECRET='test-private-secret', token='test-private-token',
                         contexts={'run': {'grant': 'test-private-grant'}}, run='run', subprocess=SimpleNamespace(run=commands),
                         sys=SimpleNamespace(stderr=stderr), Path=Path, os=SimpleNamespace(environ={}), root=root)
        exec(compile(module, '<native-readiness-boundary>', 'exec'), namespace)
        try:
            outcome = namespace['main']()()
        except AssertionError as error:
            outcome = ('AssertionError', str(error))
        evidence = root / 'evidence/failure-diagnostics.txt'
        return dict(outcome=outcome, stderr=stderr.getvalue(), evidence=evidence.read_text() if evidence.exists() else None,
                    phases=phases, commands=commands.call_args_list, watches=observer.watch.call_args_list)


class NativePhaseTests(unittest.TestCase):
    def assert_readiness_binding(self, source, role):
        for failed in (False, True):
            baseline = replay_readiness_boundary(source, role, observed=False, failed=failed)
            observed = replay_readiness_boundary(source, role, failed=failed)
            self.assertEqual(observed.pop('watches'), [unittest.mock.call('fixture-supervisor.service', 'attempt')])
            baseline.pop('watches')
            self.assertEqual(observed, baseline)
            if failed:
                self.assertEqual(observed['outcome'], ('AssertionError', 'Synthetic workload failed before readiness.'))
                self.assertEqual(len(observed['commands']), 4)
                self.assertIn('Synthetic workload exit code:', observed['evidence'])
                for secret in ('test-private-secret', 'test-private-token', 'test-private-grant'):
                    self.assertNotIn(secret, observed['stderr'])
                    self.assertNotIn(secret, observed['evidence'])
            else:
                self.assertEqual(observed['outcome'], 'ready')
                self.assertIsNone(observed['evidence'])

    def test_real_nested_readiness_binding_preserves_success_and_failure(self):
        source = Path(__file__).with_name('native_runtime_test.py').read_text()
        for role in ('reference', 'candidate'):
            with self.subTest(role=role):
                self.assert_readiness_binding(source, role)

    def test_entire_fixture_closes_over_observer_and_keeps_failure_diagnostics_local(self):
        source = Path(__file__).with_name('native_runtime_test.py').read_text()
        for role in ('reference', 'candidate'):
            main = next(scope for scope in symtable.symtable(instrument(source, role), '<fixture>', 'exec').get_children()
                        if scope.get_name() == 'main')
            self.assertTrue(main.lookup(OBSERVER).is_local())
            for name in ('progress', 'start'):
                scope = next(child for child in main.get_children() if child.get_name() == name)
                self.assertTrue(scope.lookup(OBSERVER).is_free())
                if name == 'start':
                    self.assertTrue(scope.lookup('diagnostics').is_local())
        for binding in ("            _cairn_ci_phase_observer = None\n", "            def nested(_cairn_ci_phase_observer): pass\n",
                        "            start_diagnostics = None\n"):
            with self.assertRaisesRegex(ValueError, 'reserved diagnostic binding'):
                instrument(source.replace("            progress('attempt-ready')\n", "            progress('attempt-ready')\n" + binding), 'candidate')

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
            self.assertIn('post-local-resume-complete', observed)
            self.assertIn('restored-measure-read', observed)
            self.assertIn('namespace-check-before', observed)
            with self.assertRaises(ValueError):
                instrument(observed, role)
        self.assertEqual(instrument(source, 'reference').replace("start_diagnostics('reference')", "start_diagnostics('candidate')"), instrument(source, 'candidate'))
        with self.assertRaises(ValueError):
            instrument(source.replace("progress('attempt-ready')", "progress('different')"), 'reference')


if __name__ == '__main__':
    unittest.main()
