"""Pure evidence-schema checks: no native execution, ELF loading or IPC."""
import base64
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch

from native_shutdown_observer import Inconclusive, UnitReference, control_observation, final_metadata, logical_evidence, stop_reason, workload, write_matrix


ATTEMPT = '11111111-1111-4111-8111-111111111111'


def active():
    return dict(Id='cairn-native-11111111-1111-4111-8111-111111111111.service',
                InvocationID='a' * 32, MainPID=123, ExecMainPID=123)


def terminal():
    return dict(active(), MainPID=0, ActiveState='failed', Result='timeout',
                ExecMainCode=1, ExecMainStatus=0, ExecMainExitTimestampMonotonic=100)


class Evidence(unittest.TestCase):
    def test_primary_success_and_cleanup_timeout_remain_separate(self):
        result = final_metadata(active(), terminal())
        self.assertEqual(result['ExecMainStatus'], 0)
        self.assertEqual(result['Result'], 'timeout')

    def test_collected_defaults_are_not_historical_success(self):
        final = dict(terminal(), ExecMainPID=0, ExecMainExitTimestampMonotonic=0, Result='success')
        with self.assertRaises(Inconclusive):
            final_metadata(active(), final)

    def test_replacement_invocation_is_rejected(self):
        with self.assertRaises(Inconclusive):
            final_metadata(active(), dict(terminal(), InvocationID='b' * 32))

    def test_wrong_main_process_and_live_unit_are_rejected(self):
        for changes in (dict(ExecMainPID=124), dict(MainPID=123, ActiveState='active')):
            with self.subTest(changes=changes), self.assertRaises(Inconclusive):
                final_metadata(active(), dict(terminal(), **changes))

    def test_genuine_failure_and_signal_are_not_normalized(self):
        for code, status, result in ((1, 23, 'exit-code'), (2, 15, 'signal')):
            final = final_metadata(active(), dict(terminal(), ExecMainCode=code,
                                                  ExecMainStatus=status, Result=result))
            self.assertEqual((final['ExecMainCode'], final['ExecMainStatus'], final['Result']),
                             (code, status, result))

    def test_import_and_construct_never_load_library(self):
        with patch('native_shutdown_observer.ctypes.CDLL', side_effect=AssertionError('ELF load')):
            reference = UnitReference(active()['Id'])
            reference.close()

    def test_cases_cannot_inject_arbitrary_commands(self):
        with self.assertRaises(ValueError):
            workload('arbitrary-command')
        self.assertIn('SIG_IGN', workload('residual'))
        self.assertIn('exit 23', workload('primary-failure'))

    def test_root_writer_hands_private_inode_to_runner_before_publication(self):
        # No privileged chown is executed: the ownership syscall is observed,
        # while a real ordinary-user temporary file proves its creation mode.
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            directory.chmod(0o700)
            parent = directory.stat()
            environment = dict(CAIRN_NATIVE_EVIDENCE_UID=str(parent.st_uid), CAIRN_NATIVE_EVIDENCE_GID=str(parent.st_gid))
            calls = []
            replace = os.replace
            def handoff(descriptor, uid, gid):
                calls.append(('owner', uid, gid, stat.S_IMODE(os.fstat(descriptor).st_mode)))
            def publish(source, target):
                calls.append(('publish',))
                replace(source, target)
            with patch.dict(os.environ, environment), patch('native_shutdown_observer.os.fchown', side_effect=handoff), patch('native_shutdown_observer.os.replace', side_effect=publish):
                write_matrix(directory, dict(schemaVersion=1, cases=[]))
            output = directory / 'native-shutdown-matrix.json'
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            self.assertEqual(calls, [('owner', parent.st_uid, parent.st_gid, 0o600), ('publish',)])
            self.assertEqual(json.loads(output.read_text()), dict(schemaVersion=1, cases=[]))
            self.assertEqual([path.name for path in directory.iterdir()], ['native-shutdown-matrix.json'])

    def test_evidence_owner_and_private_directory_are_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            parent = directory.stat()
            invalid = dict(CAIRN_NATIVE_EVIDENCE_UID='0', CAIRN_NATIVE_EVIDENCE_GID=str(parent.st_gid))
            with patch.dict(os.environ, invalid), self.assertRaises(Inconclusive):
                write_matrix(directory, {})
            owner = dict(CAIRN_NATIVE_EVIDENCE_UID=str(parent.st_uid), CAIRN_NATIVE_EVIDENCE_GID=str(parent.st_gid))
            directory.chmod(0o755)
            with patch.dict(os.environ, owner), self.assertRaises(Inconclusive):
                write_matrix(directory, {})
            self.assertEqual(list(directory.iterdir()), [])

    def test_guard_bridge_uses_separate_exclusive_private_runner_output(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / '.github/workflows/ci.yaml').read_text()
        checkpoint = (root / 'crates/installation/src/worker/checkpoint.rs').read_text()
        self.assertIn('CAIRN_NATIVE_EVIDENCE_UID="$(id -u)"', workflow)
        self.assertIn('CAIRN_NATIVE_EVIDENCE_GID="$(id -g)"', workflow)
        self.assertIn('CAIRN_NATIVE_SHUTDOWN_GUARD_OUTPUT:', workflow)
        self.assertIn('native-shutdown-manager-guard.json', workflow)
        self.assertIn('assert_ne!(path, output)', checkpoint)
        self.assertIn('.create_new(true)\n            .mode(0o600)', checkpoint)
        self.assertNotIn('std::fs::write(path, serde_json::to_vec_pretty(&evidence)', checkpoint)

    def test_ordinary_reader_does_not_need_to_overwrite_read_only_input(self):
        # Model the runner's access to a root-owned 0644 input without chown or
        # privileged execution: our own 0400 inode is readable, not writable.
        self.assertNotEqual(os.geteuid(), 0, 'This contract check requires an ordinary user.')
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / 'matrix.json'
            source.write_text('{"cases":[]}')
            source.chmod(0o400)
            original = source.read_bytes()
            with self.assertRaises(PermissionError):
                source.write_text('old bridge overwrites existing inode')
            output = Path(temporary) / 'manager-guard.json'
            descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, 'w') as target:
                json.dump(json.loads(source.read_text()), target)
            self.assertEqual(source.read_bytes(), original)
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            with self.assertRaises(FileExistsError):
                os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)

    def test_gone_descendant_never_proves_post_primary_lifetime(self):
        value = dict(case='residual', StatusCode=0, providerCompleted=True, PhysicalFinalized=True,
                     ownedDescendantGone=True, primary=dict(ExecMainCode=1, ExecMainStatus=0, Result='success'))
        observed = control_observation(value)
        self.assertEqual(observed['status'], 'not-exercised')
        self.assertFalse(observed['descendantAliveAfterPrimaryExitProven'])
        value['primary']['Result'] = 'timeout'
        observed = control_observation(value)
        self.assertEqual(observed['status'], 'not-attributed')
        self.assertTrue(observed['primarySuccessWithCleanupTimeout'])
        self.assertFalse(observed['descendantAliveAfterPrimaryExitProven'])

    def test_negative_control_capture_can_report_not_matched(self):
        value = dict(case='cancel', StatusCode=0, providerCompleted=True, PhysicalFinalized=True)
        self.assertEqual(control_observation(value)['status'], 'not-attributed')
        value.update(StatusCode=143, providerCompleted=False)
        self.assertEqual(control_observation(value)['status'], 'not-attributed')
        value['stopReason'] = 'cancelled'
        self.assertEqual(control_observation(value)['status'], 'matched')

    def test_signal_stop_is_not_a_genuine_logical_failure(self):
        value = dict(case='primary-failure', attempt=ATTEMPT, StatusCode=143,
                     providerCompleted=False, PhysicalFinalized=True,
                     primary=dict(ExecMainCode=2, ExecMainStatus=15, Result='signal'))
        self.assertEqual(control_observation(value)['status'], 'not-attributed')
        value['logicalEvidence'] = dict(attempt=ATTEMPT, failed=True, syntheticExitCode=23)
        self.assertEqual(control_observation(value)['status'], 'not-matched')
        value['StatusCode'] = 1
        self.assertEqual(control_observation(value)['status'], 'matched')
        for changes in (dict(failed=False), dict(syntheticExitCode=-15), dict(attempt='22222222-2222-4222-8222-222222222222')):
            with self.subTest(changes=changes):
                proof = dict(attempt=ATTEMPT, failed=True, syntheticExitCode=23)
                proof.update(changes)
                value['logicalEvidence'] = proof
                self.assertEqual(control_observation(value)['status'], 'not-attributed')

    def test_actual_synthetic_exit_is_projected_from_attempt_bound_failed_frame(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / (ATTEMPT + '.log')
            event = dict(type='turn.failed', error=dict(message='Synthetic native workload failed: 23\nprivate fixture tail'))
            encoded = (json.dumps(event) + '\n').encode()
            chunks = [encoded[:19], encoded[19:]]
            records = [dict(stderr=False, data=base64.b64encode(chunk).decode()) for chunk in chunks]
            path.write_text(''.join(json.dumps(record) + '\n' for record in records))
            self.assertEqual(logical_evidence(path, ATTEMPT),
                dict(attempt=ATTEMPT, completed=False, failed=True, syntheticExitCode=23))
            with self.assertRaises(Inconclusive):
                logical_evidence(path, '22222222-2222-4222-8222-222222222222')

    def test_workload_stderr_and_unrelated_errors_cannot_prove_exit23(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / (ATTEMPT + '.log')
            failed = dict(type='turn.failed', error=dict(message='Synthetic native workload failed: 23\n'))
            stderr = dict(stderr=True, data=base64.b64encode((json.dumps(failed) + '\n').encode()).decode())
            unrelated = dict(type='turn.failed', error=dict(message='Unrelated interrupted provider'))
            stdout = dict(stderr=False, data=base64.b64encode((json.dumps(unrelated) + '\n').encode()).decode())
            path.write_text(json.dumps(stderr) + '\n' + json.dumps(stdout) + '\n')
            self.assertEqual(logical_evidence(path, ATTEMPT),
                dict(attempt=ATTEMPT, completed=False, failed=True, syntheticExitCode=None))

    def test_cancel_and_lease_use_only_the_attempt_fixed_stop_reason(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / (ATTEMPT + '.stop-reason.json')
            self.assertIsNone(stop_reason(path, ATTEMPT))
            path.write_text(json.dumps(dict(reason='lease_expired', usage=dict(private='not exported'))))
            self.assertEqual(stop_reason(path, ATTEMPT), 'lease_expired')
            value = dict(case='lease', StatusCode=143, providerCompleted=False,
                         PhysicalFinalized=True, stopReason=stop_reason(path, ATTEMPT))
            self.assertEqual(control_observation(value)['status'], 'matched')
            value['case'] = 'cancel'
            self.assertEqual(control_observation(value)['status'], 'not-matched')
            path.write_text(json.dumps(dict(reason='untrusted arbitrary payload')))
            self.assertEqual(stop_reason(path, ATTEMPT), 'other')


if __name__ == '__main__':
    unittest.main()
