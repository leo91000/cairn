"""Unprivileged regression for the disposable local-runtime entrypoint guard."""
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import native_runtime_test as runtime

class NativeRuntimeEntrypointTests(unittest.TestCase):
    def test_namespace_pid_is_resolved_only_within_the_owned_cgroup(self):
        with tempfile.TemporaryDirectory() as root:
            proc = Path(root)
            def task(pid, group, namespace_pid):
                directory = proc / str(pid)
                directory.mkdir()
                (directory / 'cgroup').write_text('0::' + group + '\n')
                (directory / 'status').write_text(f'NSpid:\t{pid}\t{namespace_pid}\n')
            task(31, '/unrelated', 31)
            task(101, '/attempt.service', 31)
            task(102, '/attempt.service-neighbor', 31)
            self.assertEqual(runtime.owned_namespace_pid('/attempt.service', 31, proc), 101)
            (proc / '101/cgroup').write_text('0::/attempt.service/child\n')
            self.assertEqual(runtime.owned_namespace_pid('/attempt.service', 31, proc), 101)
            task(103, '/attempt.service/other', 31)
            with self.assertRaisesRegex(AssertionError, 'Expected one owned process'):
                runtime.owned_namespace_pid('/attempt.service', 31, proc)
            with self.assertRaises(AssertionError):
                runtime.owned_namespace_pid('/', 31, proc)

    def test_disposable_entrypoint_reaches_mocked_prerequisites_before_host_operations(self):
        class PrerequisitesReached(Exception):
            pass

        prerequisites = Mock(side_effect=PrerequisitesReached)
        # Give main only this mocked host method. No installer, service or
        # mapping operation can run even if the expected early stop regresses.
        host_stub = SimpleNamespace(require_docker_host=prerequisites)
        temporary_directory = tempfile.TemporaryDirectory
        environment = dict(CAIRN_NATIVE_DISPOSABLE_HOST='1')
        with temporary_directory(prefix='native-entrypoint-test-') as root:
            with patch.dict(os.environ, environment), \
                    patch.object(runtime, 'host', host_stub), \
                    patch.object(runtime.os, 'geteuid', return_value=0), \
                    patch.object(sys, 'argv', ['native_runtime_test.py', sys.executable, sys.executable, sys.executable]), \
                    patch.object(runtime, 'native_fixture', side_effect=AssertionError('Unexpected host fixture.')), \
                    patch.object(runtime, 'command', side_effect=AssertionError('Unexpected host command.')), \
                    patch('builtins.print'):
                with self.assertRaises(PrerequisitesReached):
                    runtime.main()
        prerequisites.assert_called_once_with()


if __name__ == '__main__':
    unittest.main()
