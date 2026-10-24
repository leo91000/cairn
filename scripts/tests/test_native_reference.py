"""Exercise the shipped selector against real shallow Git histories, offline."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('native_reference', Path(__file__).resolve().parents[1] / 'native_reference.py')
reference = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reference)


class NativeReferenceTests(unittest.TestCase):
    def setUp(self):
        isolated_git = patch.dict(os.environ, {'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_CONFIG_NOSYSTEM': '1'})
        isolated_git.start()
        self.addCleanup(isolated_git.stop)
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        origin = root / 'origin'
        origin.mkdir()
        reference.git('init', '--initial-branch=main', cwd=origin)
        reference.git('config', 'user.name', 'Native fixture', cwd=origin)
        reference.git('config', 'user.email', 'fixture@cairn.test', cwd=origin)
        reference.git('config', 'commit.gpgsign', 'false', cwd=origin)
        self.commits = []
        for index in range(3):
            (origin / 'fixture').write_text(str(index))
            reference.git('add', 'fixture', cwd=origin)
            reference.git('commit', '-m', 'fixture ' + str(index), cwd=origin)
            self.commits.append(reference.git('rev-parse', 'HEAD', cwd=origin))
        reference.git('tag', '-a', 'fixture-tag', '-m', 'fixture tag', cwd=origin)
        self.tag = reference.git('rev-parse', 'refs/tags/fixture-tag', cwd=origin)
        self.checkout = root / 'checkout'
        reference.git('clone', '--depth=1', origin.as_uri(), str(self.checkout))
        self.assertEqual(reference.git('rev-parse', '--is-shallow-repository', cwd=self.checkout), 'true')

    def test_pr_base_and_main_push_before_remain_exact(self):
        for preferred in self.commits[:2]:
            self.assertEqual(reference.select_reference(preferred, self.commits[-1], self.checkout), preferred)

    def test_manual_event_fetches_first_parent_from_shallow_checkout(self):
        self.assertEqual(reference.select_reference('', self.commits[-1], self.checkout), self.commits[-2])

    def test_new_tag_zero_before_fetches_first_parent_from_shallow_checkout(self):
        self.assertEqual(reference.select_reference('0' * 40, self.commits[-1], self.checkout), self.commits[-2])

    def test_annotated_tag_objects_resolve_to_the_checked_out_commit(self):
        self.assertEqual(reference.select_reference('', self.tag, self.checkout), self.commits[-2])

    def test_invalid_event_and_mismatched_checkout_fail_explicitly(self):
        for preferred, current in [('main', self.commits[-1]), ('', '0' * 40), ('', self.commits[0])]:
            with self.assertRaises(ValueError):
                reference.select_reference(preferred, current, self.checkout)


if __name__ == '__main__':
    unittest.main()
