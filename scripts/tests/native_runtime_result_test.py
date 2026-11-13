"""Unprivileged checks of native exit records and retained health observations."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, call, patch

# No privileged fixture is started for these observation and parsing checks.
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

    def test_readonly_fault_consumes_authenticated_result_without_workload_log(self):
        self.exit_file.write_text('{"StatusCode":70,"FinalizationFailed":true}')
        proof = {'StatusCode': 70, 'FinalizationFailed': True}
        self.assertEqual(runtime.require_finalization_failure(self.exit_file, proof), proof)
        for wait in ({'StatusCode': 70}, {'StatusCode': 1}, {'StatusCode': 70, 'FinalizationFailed': False}):
            with self.subTest(wait=wait), self.assertRaises(AssertionError):
                runtime.require_finalization_failure(self.exit_file, wait)
        self.exit_file.write_text('70')
        with self.assertRaises(AssertionError):
            runtime.require_finalization_failure(self.exit_file, {'StatusCode': 70})

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


class RunningLocalStorageTest(unittest.TestCase):
    def test_waits_for_the_same_unpaused_snapshot_after_local_write_pressure(self):
        ready = dict(paused=False, published=dict(local=True, generation=2))
        samples = [dict(paused=True), dict(availability='unavailable'), ready]
        api = Mock(side_effect=samples)
        with patch.object(runtime.time, 'sleep'):
            snapshot = runtime.running_local_storage(api, 'run')
        self.assertIs(snapshot, ready)
        self.assertIs(snapshot['paused'], False)
        self.assertEqual(api.call_args_list, [call('/disks/run/storage-status')] * len(samples))

    def test_ready_snapshot_is_not_replaced_by_a_later_pressure_observation(self):
        ready = dict(paused=False)
        api = Mock(side_effect=[ready, dict(paused=True)])
        self.assertIs(runtime.running_local_storage(api, 'run'), ready)
        api.assert_called_once_with('/disks/run/storage-status')

    def test_persistent_pressure_fails_at_the_bounded_observation_deadline(self):
        api = Mock(return_value=dict(paused=True))
        with patch.object(runtime.time, 'monotonic', side_effect=[0, 1, 26]), \
                patch.object(runtime.time, 'sleep'):
            with self.assertRaisesRegex(AssertionError, 'Timed out waiting for native runtime evidence'):
                runtime.running_local_storage(api, 'run')
        api.assert_called_once_with('/disks/run/storage-status')


class RetainedHealthTest(unittest.TestCase):
    def test_busy_missing_and_stale_credit_wait_for_the_same_fresh_positive_snapshot(self):
        run = 'stopped-run'
        credited = dict(status='ok', usage=dict(diskBytes=4096),
                        diskGrowth=dict(retainedBytes={run: 4096}))
        observations = [
            dict(status='ok', diskGrowth=dict(retainedBytes={})),
            dict(status='degraded'),
            dict(status='ok', diskGrowth=None),
            dict(status='ok', diskGrowth=dict(retainedBytes={'another-run': 4096})),
            dict(status='ok', diskGrowth=dict(retainedBytes={run: 0})),
            dict(status='degraded', diskGrowth=dict(retainedBytes={run: 4096})),
            credited,
        ]
        api = Mock(side_effect=observations)
        with patch.object(runtime.time, 'sleep'):
            snapshot = runtime.retained_health(api, run)
        # These are the real qualification assertions. Reading an immediate
        # busy snapshot reproduces its KeyError instead of proving any credit.
        self.assertGreater(snapshot['diskGrowth']['retainedBytes'][run], 0)
        self.assertGreater(snapshot['usage']['diskBytes'], 0)
        self.assertIs(snapshot, credited)
        self.assertEqual(api.call_args_list, [call('/health', method='GET')] * len(observations))

    def test_an_already_credited_sample_is_not_replaced_with_another_read(self):
        snapshot = dict(status='ok', diskGrowth=dict(retainedBytes={'run': 7}))
        api = Mock(side_effect=[snapshot, dict(status='ok', diskGrowth=dict(retainedBytes={}))])
        self.assertIs(runtime.retained_health(api, 'run'), snapshot)
        api.assert_called_once_with('/health', method='GET')

    def test_missing_credit_uses_the_existing_bounded_observation_deadline(self):
        api = Mock(return_value=dict(status='ok', diskGrowth=dict(retainedBytes={})))
        with patch.object(runtime.time, 'monotonic', side_effect=[0, 1, 61]), \
                patch.object(runtime.time, 'sleep'):
            with self.assertRaisesRegex(AssertionError, 'Timed out waiting for native runtime evidence'):
                runtime.retained_health(api, 'run')
        api.assert_called_once_with('/health', method='GET')


class CompletedLocalDiskTest(unittest.TestCase):
    def receipt(self):
        return dict(status=dict(dirtyBytes=0, published=dict(local=True), completedGeneration=1))

    def test_completed_local_disk_requires_a_durable_local_receipt(self):
        receipt = self.receipt()
        manager = Mock(return_value=receipt)
        api = Mock(return_value=receipt['status'])
        self.assertIs(runtime.completed_local_disk(api, manager, 'run'), receipt)
        api.assert_called_once_with('/disks/run/storage-status')
        manager.assert_called_once_with('consolidate', run='run')

    def test_remote_dirty_and_missing_completion_receipts_are_rejected(self):
        for wrong in ('remote', 'dirty', 'completion'):
            with self.subTest(wrong=wrong):
                receipt = self.receipt()
                if wrong == 'remote':
                    receipt['status']['published'] = dict(backupId='old-remote-point')
                elif wrong == 'dirty':
                    receipt['status']['dirtyBytes'] = 4096
                else:
                    receipt['status']['completedGeneration'] = 0
                with self.assertRaises((AssertionError, KeyError)):
                    runtime.completed_local_disk(Mock(return_value=self.receipt()['status']), Mock(return_value=receipt), 'run')


if __name__ == '__main__':
    unittest.main()
