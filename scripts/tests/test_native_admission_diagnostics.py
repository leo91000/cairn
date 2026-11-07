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
DELETE_ID = '12345678-1234-1234-1234-123456789abc'
DELETE_PATH = f'/disks/{DELETE_ID}/delete'
DELETE_GUARD_REASONS = {
    'Native workspace is active.': 'workspace_owner_busy',
    'Native disk is active.': 'disk_owner_busy',
    'Disk is opening; retry restoration.': 'runtime_opening',
    'Disk replacement is in progress.': 'runtime_replacing',
    'Disk is still in use; retry restoration.': 'runtime_in_use',
}


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

    def test_disk_delete_guard_reasons_use_a_fixed_route_without_uuid(self):
        for message, reason in DELETE_GUARD_REASONS.items():
            payload = json.dumps({'error': message}).encode()
            signal = rejection_signal(DELETE_PATH, 'POST', 409, payload, 'candidate')
            self.assertEqual(signal, dict(role='candidate', method='POST', route='native_disk_delete',
                                          status=409, reason=reason, responseBytes=len(payload)))
            self.assertNotIn(DELETE_ID, json.dumps(signal))

    def test_disk_delete_observation_rejects_nonexact_requests_and_non409_statuses(self):
        known = json.dumps({'error': 'Native disk is active.'}).encode()
        paths = (DELETE_PATH + '?token=private', DELETE_PATH + '/', DELETE_PATH + '/extra',
                 '/disks/not-a-uuid/delete')
        for path in paths:
            self.assertIsNone(rejection_signal(path, 'POST', 409, known, 'reference'))
        for method, status, role in (('GET', 409, 'reference'), ('POST', 200, 'reference'),
                                     ('POST', 503, 'reference'), ('POST', 409.0, 'reference'),
                                     ('POST', 409, 'untrusted')):
            self.assertIsNone(rejection_signal(DELETE_PATH, method, status, known, role))

    def test_disk_delete_unknown_oversized_and_deep_payloads_never_leak(self):
        secret = 'private-delete-error-token'
        payloads = (
            json.dumps({'error': secret}).encode(),
            b'x' * 4097,
            b'not-json',
            b'\xff',
            b'[]',
            b'{"error":null}',
            json.dumps({'error': 'Native disk is active.', 'private': secret}).encode(),
            b'[' * 1500 + b'0' + b']' * 1500,
        )
        for payload in payloads:
            signal = rejection_signal(DELETE_PATH, 'POST', 409, payload, 'candidate')
            self.assertEqual(signal['route'], 'native_disk_delete')
            self.assertEqual(signal['reason'], 'unrecognized')
            self.assertEqual(signal['responseBytes'], len(payload))
            self.assertNotIn(secret, json.dumps(signal))
            self.assertNotIn(DELETE_ID, json.dumps(signal))

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
                cases = (
                    (PATH, 409, b'{"error":"Native workspace is still active."}',
                     'native_attempt_admission', 'workspace_owner_busy'),
                    (DELETE_PATH, 409, b'{"error":"Native workspace is active."}',
                     'native_disk_delete', 'workspace_owner_busy'),
                    (DELETE_PATH, 409, b'{"error":"private-do-not-print"}',
                     'native_disk_delete', 'unrecognized'),
                    (DELETE_PATH + '?token=private', 409, b'{"error":"Native disk is active."}', None, None),
                    (DELETE_PATH, 503, b'{"error":"Native disk is active."}', None, None),
                    (DELETE_PATH, 200, b'{"status":"ok"}', None, None),
                )
                for path, status, payload, route, reason in cases:
                    results = []
                    for is_observed, source in ((False, original), (True, observed)):
                        requests, reads, closures = [], [], []

                        def read_response():
                            reads.append(True)
                            return payload

                        response = types.SimpleNamespace(
                            status=status,
                            headers={'X-Private-Response': 'private-response-header'},
                            read=read_response,
                        )
                        connection = types.SimpleNamespace(request=lambda *a, **kw: requests.append((a, kw)),
                                                           getresponse=lambda: response, close=lambda: closures.append(True))
                        host = types.SimpleNamespace(UnixControllerConnection=lambda *a, **kw: connection)
                        namespace = dict(host=host, state=Path('/synthetic'), token='private-request-token', json=json, urllib=urllib)
                        api = next(node for node in ast.walk(ast.parse(source)) if isinstance(node, ast.FunctionDef) and node.name == 'api')
                        module = ast.Module(body=[api], type_ignores=[])
                        exec(compile(ast.fix_missing_locations(module), '<fixture-api>', 'exec'), namespace)
                        output = io.StringIO()
                        with contextlib.redirect_stdout(output), patch('builtins.print', side_effect=failure) if failure else contextlib.nullcontext():
                            try:
                                result = namespace['api'](path, {'private-request-body': 'private-request-value'})
                            except urllib.error.HTTPError as error:
                                result = (type(error), error.code, error.reason, error.read())
                        logged = output.getvalue()
                        self.assertNotIn('private-do-not-print', logged)
                        self.assertNotIn('private-request-token', logged)
                        self.assertNotIn('private-request-value', logged)
                        self.assertNotIn('private-response-header', logged)
                        if is_observed and failure is None and route is not None:
                            lines = logged.splitlines()
                            self.assertEqual(len(lines), 1)
                            marker, encoded = lines[0].split(' ', 1)
                            self.assertEqual(marker, 'NATIVE_CI_ADMISSION_REJECTION')
                            record = json.loads(encoded)
                            self.assertEqual(record['route'], route)
                            self.assertEqual(record['reason'], reason)
                            if route == 'native_disk_delete':
                                self.assertNotIn('attempt', record)
                                self.assertNotIn(DELETE_ID, logged)
                                self.assertEqual(
                                    set(record),
                                    {'role', 'method', 'route', 'status', 'reason', 'responseBytes'},
                                )
                            else:
                                self.assertEqual(record, dict(
                                    role=role,
                                    method='POST',
                                    route='native_attempt_admission',
                                    attempt=DELETE_ID,
                                    status=409,
                                    reason=reason,
                                    responseBytes=len(payload),
                                ))
                        else:
                            self.assertEqual(logged, '')
                        self.assertEqual(requests, [(('POST', path), {
                            'body': json.dumps({'private-request-body': 'private-request-value'}).encode(),
                            'headers': {
                                'Authorization': 'Bearer private-request-token',
                                'Content-Type': 'application/json',
                            },
                        })])
                        self.assertEqual(reads, [True])
                        self.assertEqual(len(closures), 1)
                        results.append((result, requests, reads, closures))
                        if status >= 400:
                            self.assertEqual(result[:3], (urllib.error.HTTPError, status, 'Native request rejected'))
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
