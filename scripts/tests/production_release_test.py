import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('release_verify', Path(__file__).parents[2] / 'deploy/beacon/release-verify.py')
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class ReleaseVerificationTests(unittest.TestCase):
    def test_forced_command_accepts_only_the_release_protocol(self):
        self.assertEqual(VERIFY.request('verify sha256:' + 'b' * 64), ['verify', 'sha256:' + 'b' * 64])
        for value in ['', 'bash', 'cat /etc/shadow', 'download ../beacon.dump', 'download ' + ('a' * 32) + '; id', 'prepare v0.56.2 ' + ('a' * 32), 'download ' + ('a' * 32), 'prepare v0.56.2 ' + ('a' * 32) + '\nid', 'prepare --help ' + ('a' * 32), 'worker v0.56.2 ' + ('a' * 32)]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                VERIFY.request(value)

    def test_verification_requires_both_images_and_actual_execution_readiness(self):
        config = {'installation_project': 'installation'}
        expected = 'sha256:' + 'e' * 64
        for ready in [True, False]:
            health = {'status': 'ok', 'maintenance': False, 'execution': {'ready': ready}}
            with patch.object(VERIFY, 'output', side_effect=['manager', json.dumps(health), 'runner']), patch.object(VERIFY, 'image', return_value=('image', ['other@sha256:' + 'f' * 64, 'installation@' + expected])):
                self.assertEqual(VERIFY.verify(config, expected)['ready'], ready)


if __name__ == '__main__':
    unittest.main()
