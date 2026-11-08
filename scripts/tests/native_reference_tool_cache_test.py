"""Synthetic setup checks; no native services, downloads or real host mutation."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / 'native-reference-tool-cache.py'
spec = importlib.util.spec_from_file_location('native_reference_tool_cache', SCRIPT)
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class CacheSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cache = self.root / 'cache'
        (self.cache / 'bin').mkdir(parents=True)
        (self.cache / 'plugins').mkdir()
        names = ['bin/tool' + str(index) for index in range(9)] + ['plugins/buildx', 'plugins/compose']
        self.definitions = [('https://example.invalid/archive.tgz', 'a' * 64,
                             {str(index): name for index, name in enumerate(names)})]
        self.wrapper = '#!/bin/sh\nexit 0\n'
        self.manifest = {}
        for name in ['docker', 'engine-api.json', *names]:
            path = self.cache / name
            path.write_text(self.wrapper if name == 'docker' else name)
            path.chmod(0o444 if name == 'engine-api.json' else 0o555)
            self.manifest[name] = setup.digest(path)
        (self.cache / 'manifest.json').write_text(json.dumps(self.manifest))
        (self.cache / 'manifest.json').chmod(0o444)
        self.preparation = dict(
            acquisitionQualified=True, physicalRuntimeQualified=False, cacheReuseVerified=True,
            cachePublished=True, toolDefinitions=json.loads(json.dumps(self.definitions)),
            wrapperSha256=hashlib.sha256(self.wrapper.encode()).hexdigest(), manifest=self.manifest,
            candidate='a' * 40, reference='b' * 40,
        )

    def verify(self, **overrides):
        values = dict(cache=self.cache, preparation=self.preparation,
                      definitions=self.definitions, wrapper=self.wrapper)
        values.update(overrides)
        return setup.verify_cache(**values)

    def test_pinned_definition_parser_accepts_original_and_rejects_code(self):
        helper_path = SCRIPT.with_name('native-tool-preparation-qualification.py')
        helper_spec = importlib.util.spec_from_file_location('native_preparation', helper_path)
        preparation = importlib.util.module_from_spec(helper_spec)
        helper_spec.loader.exec_module(preparation)
        actual = (SCRIPT.parent.parent / 'deploy/nodes/native-host.py').read_text()
        definitions = preparation.definitions(actual)
        self.assertEqual(len(definitions), 5)
        self.assertEqual(len(definitions[0][2]), 7)
        for expression in ("get_tools()", "value.tools", "value[0]", "(1 * 2,)"):
            with self.subTest(expression=expression), self.assertRaises(ValueError):
                preparation.definitions('DOCKER_TOOLS = ' + expression)

    def test_complete_inventory_has_fourteen_files(self):
        inventory = self.verify()
        self.assertEqual(len(inventory), 14)
        self.assertIn('manifest.json', inventory)

    def test_cold_or_partial_receipt_is_refused(self):
        for key, value in [('acquisitionQualified', False), ('physicalRuntimeQualified', True),
                           ('cacheReuseVerified', False), ('cachePublished', False)]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.verify(preparation={**self.preparation, key: value})

    def test_changed_definition_or_wrapper_is_refused(self):
        for index in (0, 1, 2):
            definitions = copy.deepcopy(self.preparation['toolDefinitions'])
            definitions[0][index] = 'changed' if index < 2 else {'new': 'bin/new'}
            with self.subTest(index=index), self.assertRaises(ValueError):
                self.verify(preparation={**self.preparation, 'toolDefinitions': definitions})
        with self.assertRaises(ValueError):
            self.verify(wrapper=self.wrapper + '# changed')

    def test_unexpected_files_symlinks_or_digest_changes_are_refused(self):
        extra = self.cache / 'extra'
        extra.write_text('unexpected')
        with self.assertRaises(ValueError):
            self.verify()
        extra.unlink()
        extra.symlink_to(self.cache / 'bin/tool0')
        with self.assertRaises(ValueError):
            self.verify()
        extra.unlink()
        (self.cache / 'bin/tool0').chmod(0o755)
        with self.assertRaises(ValueError):
            self.verify()
        (self.cache / 'bin/tool0').write_text('corrupt')
        (self.cache / 'bin/tool0').chmod(0o555)
        with self.assertRaises(ValueError):
            self.verify()

    def test_manifest_or_payload_symlink_is_refused_before_read(self):
        for name in ('manifest.json', 'bin/tool0'):
            with self.subTest(name=name):
                path = self.cache / name
                original = path.read_bytes()
                mode = path.stat().st_mode & 0o777
                path.unlink()
                path.symlink_to(self.root / 'missing')
                with self.assertRaises(ValueError):
                    self.verify()
                path.unlink()
                path.write_bytes(original)
                path.chmod(mode)

    def test_cache_path_alias_is_refused(self):
        alias = self.root / 'alias'
        alias.symlink_to(self.cache, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.verify(cache=alias)

    def test_seed_uses_private_independent_copy_and_preserves_source(self):
        fixture = self.root / 'fixture'
        fixture.mkdir(mode=0o700)
        helper = fixture / 'helper.py'
        helper.write_text('# exact reference helper\n')
        host = types.SimpleNamespace(__file__=str(helper), INSTALL=fixture / 'managed-tools',
                                     DOCKER_TOOLS=self.definitions, DOCKER_WRAPPER=self.wrapper)
        preparation = self.root / 'preparation.json'
        preparation.write_text(json.dumps(self.preparation))
        receipt = self.root / 'receipt.json'
        env = dict(CAIRN_NATIVE_DISPOSABLE_HOST='1', CAIRN_NODE_TEST_S3_ENDPOINT='http://127.0.0.1:1234')
        with patch.dict(os.environ, env), patch.object(setup.os, 'geteuid', return_value=0):
            # Actual ownership is allowed only for this synthetic fixture root.
            original_stat = Path.stat
            def stat_as_fixture_owner(path, *args, **kwargs):
                result = original_stat(path, *args, **kwargs)
                if path == fixture:
                    return types.SimpleNamespace(st_uid=0)
                return result
            with patch.object(Path, 'stat', stat_as_fixture_owner):
                setup.seed_reference(host, fixture, self.cache, preparation, receipt, setup.digest(helper))
        target = host.INSTALL / 'docker-tools/v2'
        self.assertEqual(self.verify(), setup.verify_cache(target, self.preparation, self.definitions, self.wrapper))
        for name in self.verify():
            self.assertNotEqual((self.cache / name).stat().st_ino, (target / name).stat().st_ino)
        # Fixture can subsequently add capability-helper bytes without changing pristine source.
        (target / 'bin/newuidmap').write_text('synthetic helper')
        self.assertEqual(len(self.verify()), 14)
        self.assertEqual(json.loads(receipt.read_text())['fileCount'], 14)

    def test_actual_fixture_insertion_preserves_the_entire_source(self):
        source = SCRIPT.with_name('tests').joinpath('native_runtime_test.py').read_text()
        insertion = ("        from native_reference_tool_cache import seed_reference\n"
                     "        seed_reference(host, root, Path('/cache'), Path('/prep'), Path('/receipt'), 'sha')\n")
        self.assertEqual(setup.instrument(source, insertion).replace(insertion, '', 1), source)

    def test_real_host_admission_is_refused(self):
        with patch.dict(os.environ, {}, clear=True), self.assertRaises(RuntimeError):
            setup.seed_reference(None, self.root, self.cache, self.root / 'missing',
                                 self.root / 'receipt', 'a' * 64)

    def test_fixture_insertion_preserves_all_original_ast(self):
        source = '''def main():
    with temporary() as temporary:
        root = Path(temporary)
        host.INSTALL = root / 'managed-tools'
        docker_tools = host.prepare_docker_tools()
        assert real_runtime(docker_tools)
'''
        insertion = ("        from native_reference_tool_cache import seed_reference\n"
                     "        seed_reference(host, root, Path('/cache'), Path('/prep'), Path('/receipt'), 'sha')\n")
        result = setup.instrument(source, insertion)
        self.assertEqual(result.replace(insertion, '', 1), source)
        with self.assertRaises(ValueError):
            setup.instrument(result, insertion)
        with self.assertRaises(ValueError):
            setup.instrument(source.replace(setup.ANCHOR, ''), insertion)


if __name__ == '__main__':
    unittest.main()
