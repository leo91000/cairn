"""Unprivileged regression for the disposable local-runtime entrypoint guard."""
import os
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import native_runtime_test as runtime

class NativeRuntimeEntrypointTests(unittest.TestCase):
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
            def isolated_directory(**options):
                return temporary_directory(**dict(options, dir=root))

            with patch.dict(os.environ, environment), \
                    patch.object(runtime, 'host', host_stub), \
                    patch.object(runtime.os, 'geteuid', return_value=0), \
                    patch.object(sys, 'argv', ['native_runtime_test.py', sys.executable, sys.executable]), \
                    patch.object(runtime.tempfile, 'TemporaryDirectory', side_effect=isolated_directory), \
                    patch.object(runtime, 'command', side_effect=AssertionError('Unexpected host command.')), \
                    patch('builtins.print'):
                with self.assertRaises(PrerequisitesReached):
                    runtime.main()
        prerequisites.assert_called_once_with()


if __name__ == '__main__':
    unittest.main()
