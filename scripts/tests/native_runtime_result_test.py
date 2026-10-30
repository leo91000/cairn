"""Unprivileged checks of the native qualification's actual exit-result reader."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import types
import unittest

# This test exercises only result parsing; no S3 or privileged fixture is started.
sys.modules.setdefault('boto3', types.ModuleType('boto3'))
path = Path(__file__).with_name('native_runtime_test.py')
spec = importlib.util.spec_from_file_location('native_runtime_test', path)
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


class ControllerResultTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.exit_file = Path(self.directory.name) / 'attempt.exit'

    def read(self, content):
        self.exit_file.write_text(content)
        return runtime.read_controller_result(self.exit_file)

    def test_legacy_and_workload_seventy_are_not_finalization_failure(self):
        for code in (0, 1, 70, 143, 255):
            with self.subTest(code=code):
                self.assertEqual(self.read(str(code)), {'StatusCode': code, 'FinalizationFailed': False})

    def test_finalization_proof_matches_wait(self):
        self.exit_file.write_text('{"FinalizationFailed":true,"StatusCode":70}')
        expected = {'StatusCode': 70, 'FinalizationFailed': True}
        self.assertEqual(runtime.read_controller_result(self.exit_file), expected)
        self.assertEqual(runtime.match_controller_result(self.exit_file, expected), expected)
        with self.assertRaises(AssertionError):
            runtime.match_controller_result(self.exit_file, {'StatusCode': 70})
        with self.assertRaises(AssertionError):
            runtime.match_controller_result(self.exit_file, {'StatusCode': 1, 'FinalizationFailed': True})
        self.exit_file.write_text('70')
        self.assertEqual(runtime.match_controller_result(self.exit_file, {'StatusCode': 70}),
                         {'StatusCode': 70, 'FinalizationFailed': False})
        with self.assertRaises(AssertionError):
            runtime.match_controller_result(self.exit_file, expected)

    def test_only_true_flag_with_code_seventy_is_authenticated(self):
        for content, expected in [
            ('{"StatusCode":70}', False),
            ('{"StatusCode":70,"FinalizationFailed":false}', False),
            ('{"StatusCode":70,"FinalizationFailed":"true"}', False),
            ('{"StatusCode":1,"FinalizationFailed":true}', False),
            ('{"StatusCode":70,"FinalizationFailed":true}', True),
        ]:
            with self.subTest(content=content):
                self.assertEqual(self.read(content)['FinalizationFailed'], expected)

    def test_rejects_malformed_and_invalid_codes(self):
        for content in ('', 'garbage', '{}', 'true', 'null', '[]', '256', '-1',
                        '{"StatusCode":true}', '{"StatusCode":"70"}',
                        '{"StatusCode":70.0}', '{"StatusCode":256}', '{"StatusCode":-1}'):
            with self.subTest(content=content):
                with self.assertRaises(ValueError):
                    self.read(content)


if __name__ == '__main__':
    unittest.main()
