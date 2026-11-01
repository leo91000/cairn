"""Pure evidence-schema checks: no native execution, ELF loading or IPC."""
import unittest
from unittest.mock import patch

from native_shutdown_observer import Inconclusive, UnitReference, final_metadata, workload


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


if __name__ == '__main__':
    unittest.main()
