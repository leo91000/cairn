"""Exercise the shipped selector against real shallow Git histories, offline."""
import importlib.util
import io
import json
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


class NativeArtifactTests(unittest.TestCase):
    environment = dict(GITHUB_REPOSITORY='leo91000/cairn', GITHUB_RUN_ID='123', GITHUB_TOKEN='synthetic-fixture')

    def test_only_unexpired_exact_name_in_current_run_is_reused(self):
        def opener(request, timeout):
            self.assertEqual(request.full_url, 'https://api.github.com/repos/leo91000/cairn/actions/runs/123/artifacts?per_page=100&page=1')
            self.assertEqual(timeout, 30)
            self.assertEqual(request.get_header('Authorization'), 'Bearer synthetic-fixture')
            return io.BytesIO(json.dumps(dict(artifacts=[dict(name='exact-reference', expired=False)])).encode())
        self.assertTrue(reference.current_run_artifact('exact-reference', self.environment, opener))

    def test_missing_expired_and_other_image_artifacts_require_a_build(self):
        for artifacts in [[], [dict(name='exact-reference', expired=True)], [dict(name='other-image', expired=False)]]:
            def opener(*args, **kwargs):
                return io.BytesIO(json.dumps(dict(artifacts=artifacts)).encode())
            self.assertFalse(reference.current_run_artifact('exact-reference', self.environment, opener))

    def test_follows_every_page_before_building_again(self):
        pages = []
        def opener(request, timeout):
            pages.append(request.full_url)
            artifacts = [dict(name='other', expired=False)] * 100 if len(pages) == 1 else [dict(name='exact-reference', expired=False)]
            return io.BytesIO(json.dumps(dict(artifacts=artifacts)).encode())
        self.assertTrue(reference.current_run_artifact('exact-reference', self.environment, opener))
        self.assertEqual(len(pages), 2)
        self.assertTrue(pages[-1].endswith('page=2'))

    def test_lookup_errors_are_not_silently_treated_as_missing(self):
        def opener(*args, **kwargs):
            raise OSError('fixture API unavailable')
        with self.assertRaises(OSError):
            reference.current_run_artifact('exact-reference', self.environment, opener)

    def test_invalid_scope_is_rejected_before_sending_credentials(self):
        def opener(*args, **kwargs):
            self.fail('Invalid scope must not make a request')
        for field, value in [('GITHUB_REPOSITORY', 'other/path/extra'), ('GITHUB_RUN_ID', 'latest')]:
            with self.assertRaises(ValueError):
                reference.current_run_artifact('exact-reference', dict(self.environment, **{field: value}), opener)


if __name__ == '__main__':
    unittest.main()
