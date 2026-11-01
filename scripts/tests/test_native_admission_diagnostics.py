"""Pure/offline projection and actual fixture-api observation checks."""
import ast
import contextlib
import io
import json
from pathlib import Path
import tempfile
from unittest.mock import patch
import types
import unittest
import urllib.error

from native_admission_overlay import instrument, main
from native_http_diagnostics import REASONS, rejection_signal


FIXTURE = Path(__file__).with_name('native_runtime_test.py')
PATH = '/runs/12345678-1234-1234-1234-123456789abc'


class DiagnosticsTests(unittest.TestCase):
    def test_known_reasons_and_unknown_bodies_are_fixed_codes_only(self):
        for message, reason in REASONS.items():
            payload = json.dumps({'error': message}).encode()
            self.assertEqual(rejection_signal(PATH, 'POST', 409, payload, 'reference')['reason'], reason)
        sensitive = 'do-not-print-this-private-data'
        for payload in (json.dumps({'error': sensitive}).encode(), b'x' * 4097,
                        b'not-json', b'\xff', b'[]', b'{"error":null}',
                        json.dumps({'error': 'Native workspace is still active.', 'private': sensitive}).encode(),
                        b'[' * 4000):
            signal = rejection_signal(PATH, 'POST', 409, payload, 'candidate')
            self.assertEqual(signal['reason'], 'unrecognized')
            self.assertNotIn(sensitive, json.dumps(signal))
            self.assertEqual(signal['responseBytes'], len(payload))

    def test_other_routes_statuses_methods_roles_produce_no_output(self):
        for path, method, status, role in ((PATH + '/wait', 'POST', 409, 'reference'),
                                          ('/private?token=secret', 'POST', 409, 'candidate'),
                                          (PATH, 'GET', 409, 'candidate'), (PATH, 'POST', 200, 'candidate'),
                                          (PATH, 'POST', 409, 'private-role')):
            self.assertIsNone(rejection_signal(path, method, status, b'{}', role))

    def test_api_keeps_request_response_exception_and_close_contract(self):
        for failure in (None, BrokenPipeError(), ValueError('closed stream')):
            original = FIXTURE.read_text()
            for role in ('reference', 'candidate'):
                observed = instrument(original, role)
                for status, payload in ((409, b'{"error":"Native workspace is still active."}'),
                                        (409, b'{"error":"private-do-not-print"}'),
                                        (200, b'{"status":"ok"}')):
                    results = []
                    for source in (original, observed):
                        requests, closures = [], []
                        response = types.SimpleNamespace(status=status, headers={}, read=lambda: payload)
                        connection = types.SimpleNamespace(request=lambda *a, **kw: requests.append((a, kw)),
                                                           getresponse=lambda: response, close=lambda: closures.append(True))
                        host = types.SimpleNamespace(UnixControllerConnection=lambda *a, **kw: connection)
                        namespace = dict(host=host, state=Path('/synthetic'), token='synthetic', json=json, urllib=urllib)
                        api = next(node for node in ast.walk(ast.parse(source)) if isinstance(node, ast.FunctionDef) and node.name == 'api')
                        module = ast.Module(body=[api], type_ignores=[])
                        exec(compile(ast.fix_missing_locations(module), '<fixture-api>', 'exec'), namespace)
                        output = io.StringIO()
                        with contextlib.redirect_stdout(output), patch('builtins.print', side_effect=failure) if failure else contextlib.nullcontext():
                            try:
                                result = namespace['api'](PATH, {'synthetic': True})
                            except urllib.error.HTTPError as error:
                                result = (type(error), error.code, error.reason, error.read())
                        self.assertNotIn('private-do-not-print', output.getvalue())
                        self.assertEqual(len(closures), 1)
                        results.append((result, requests, closures))
                    self.assertEqual(results[0], results[1])

    def test_archived_fixture_overlay_keeps_source_and_copies_helper(self):
        original = FIXTURE.read_text()
        for role in ('reference', 'candidate'):
            with tempfile.TemporaryDirectory() as directory:
                fixture = Path(directory) / 'scripts/tests/native_runtime_test.py'
                fixture.parent.mkdir(parents=True)
                fixture.write_text(original)
                output = io.StringIO()
                with patch('sys.argv', ['overlay', directory, role]), contextlib.redirect_stdout(output):
                    main()
                self.assertEqual(fixture.read_text(), instrument(original, role))
                self.assertEqual(fixture.with_name('native_http_diagnostics.py').read_bytes(),
                                 Path(__file__).with_name('native_http_diagnostics.py').read_bytes())
                provenance = json.loads(output.getvalue())
                self.assertEqual(provenance['role'], role)
                self.assertTrue(provenance['observationOnly'])
                self.assertRegex(provenance['originalSha256'], r'^[0-9a-f]{64}$')

    def test_overlay_rejects_shape_changes_and_duplicate_application(self):
        original = FIXTURE.read_text()
        for source in (original.replace('if response.status >= 400:', 'if response.status > 399:'),
                       original + '\n' + original):
            with self.assertRaises(ValueError):
                instrument(source, 'reference')
        with self.assertRaises((AssertionError, ValueError)):
            instrument(instrument(original, 'reference'), 'reference')


if __name__ == '__main__':
    unittest.main()
