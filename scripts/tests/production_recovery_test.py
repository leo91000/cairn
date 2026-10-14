import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('release_backup', Path(__file__).parents[2] / 'deploy/beacon/release-backup.py')
BACKUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BACKUP)
IDENTIFIER = 'a' * 32


class RecoveryTests(unittest.TestCase):
    def test_forced_command_accepts_only_the_release_protocol(self):
        self.assertEqual(BACKUP.request('prepare v0.56.2 ' + IDENTIFIER), ['prepare', 'v0.56.2', IDENTIFIER])
        self.assertEqual(BACKUP.request('download ' + IDENTIFIER), ['download', IDENTIFIER])
        self.assertEqual(BACKUP.request('verify sha256:' + 'b' * 64), ['verify', 'sha256:' + 'b' * 64])
        for value in ['', 'bash', 'cat /etc/shadow', 'download ../beacon.dump', 'download ' + IDENTIFIER + '; id', 'prepare v0.56.2 ' + IDENTIFIER + '\nid', 'prepare --help ' + IDENTIFIER, 'worker v0.56.2 ' + IDENTIFIER]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                BACKUP.request(value)

    def test_failure_before_or_after_stop_always_attempts_recovery_without_publishing(self):
        for failed in ['stop', 'backup']:
            with self.subTest(failed=failed), tempfile.TemporaryDirectory() as root:
                config = {'directory': root, 'helper': 'helper', 'recipient': 'recipient'}
                calls = []

                def execute(*args):
                    calls.append(args)
                    if failed in args:
                        raise RuntimeError('synthetic failure')

                with patch.object(BACKUP, 'container', side_effect=['beacon', 'postgres']), patch.object(BACKUP, 'image', return_value=('postgres-image', ['beacon@sha256:' + 'c' * 64])), patch.object(BACKUP, 'public', side_effect=[{'status': 'ok', 'commit': 'd' * 40}, {'image': 'installation'}]), patch.object(BACKUP, 'tables', return_value=21), patch.object(BACKUP, 'recover') as recover, patch.object(BACKUP, 'run', side_effect=execute):
                    with self.assertRaises(RuntimeError):
                        BACKUP.backup(config, 'v0.56.2', IDENTIFIER)
                    recover.assert_called_once_with(config, IDENTIFIER)
                self.assertFalse((Path(root) / IDENTIFIER / 'release.json').exists())

    def test_restore_failure_and_schema_mismatch_never_publish_recovery_evidence(self):
        for failure in ['restore', 'schema']:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as root:
                config = {'directory': root, 'helper': 'helper', 'recipient': 'recipient'}
                calls = []

                def execute(*args):
                    calls.append(args)
                    if failure == 'restore' and 'restore' in args:
                        raise RuntimeError('synthetic restore failure')

                with patch.object(BACKUP, 'container', side_effect=['beacon', 'postgres']), patch.object(BACKUP, 'image', return_value=('postgres-image', ['beacon@sha256:' + 'c' * 64])), patch.object(BACKUP, 'public', side_effect=[{'status': 'ok', 'commit': 'd' * 40}, {'image': 'installation'}]), patch.object(BACKUP, 'tables', side_effect=[21, 20]), patch.object(BACKUP, 'recover'), patch.object(BACKUP, 'subprocess') as process, patch.object(BACKUP, 'run', side_effect=execute):
                    process.run.return_value.returncode = 0
                    with self.assertRaises(RuntimeError):
                        BACKUP.backup(config, 'v0.56.2', IDENTIFIER)
                self.assertTrue(any(command[:3] == ('docker', 'rm', '-fv') for command in calls))
                self.assertFalse(any(command[0] == 'age' for command in calls))
                self.assertFalse((Path(root) / IDENTIFIER / 'release.json').exists())

    def test_recovery_does_not_start_an_old_beacon_beside_its_replacement(self):
        with tempfile.TemporaryDirectory() as root:
            state = Path(root) / IDENTIFIER
            state.mkdir()
            (state / 'state.json').write_text(json.dumps({'beacon': 'old', 'resume': True}))
            config = {'directory': root, 'project': 'beacon-project'}
            with patch.object(BACKUP, 'subprocess') as process, patch.object(BACKUP, 'output', side_effect=[json.dumps({'com.docker.compose.project': 'beacon-project', 'com.docker.compose.service': 'beacon'}), 'replacement']), patch.object(BACKUP, 'run') as execute:
                with self.assertRaises(RuntimeError):
                    BACKUP.recover(config, IDENTIFIER)
                execute.assert_not_called()
                process.run.assert_called_once()

    def test_verification_requires_both_images_and_actual_execution_readiness(self):
        config = {'installation_project': 'installation'}
        expected = 'sha256:' + 'e' * 64
        for ready in [True, False]:
            health = {'status': 'ok', 'maintenance': False, 'execution': {'ready': ready}}
            with patch.object(BACKUP, 'output', side_effect=['manager', json.dumps(health), 'runner']), patch.object(BACKUP, 'image', return_value=('image', ['other@sha256:' + 'f' * 64, 'installation@' + expected])):
                self.assertEqual(BACKUP.verify(config, expected)['ready'], ready)


if __name__ == '__main__':
    unittest.main()
