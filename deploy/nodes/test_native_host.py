"""Downloader and update recovery tests; no services, drivers or real credentials."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile

SPEC = importlib.util.spec_from_file_location('native_host', Path(__file__).with_name('native-host.py'))
host = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(host)


def release(**changes):
    value = dict(sha256='a' * 64, toolsSha256='b' * 64, size=3, toolsSize=3,
                 os='linux', arch='x86_64', protocolMin=2, protocolMax=2,
                 dataEpoch=5, runtimeId='synthetic-runtime',
                 binaryPath='/internal/nodes/native-binary', toolsPath='/internal/nodes/native-tools')
    value.update(changes)
    return value


class NativeHostTests(unittest.TestCase):
    def test_disabled_user_namespaces_fail_without_changing_global_restrictions(self):
        with patch.object(host.subprocess, 'run', return_value=Mock(stdout='systemd 255')), \
                patch.object(host.Path, 'exists', return_value=True), \
                patch.object(host.Path, 'read_text', return_value='0'), \
                patch.object(host.Path, 'write_text') as write, \
                self.assertRaisesRegex(ValueError, 'enabled unprivileged user namespaces'):
            host.require_docker_host()
        write.assert_not_called()

    def test_rootless_mapping_fails_before_enrollment_on_implicit_nnp_systemd(self):
        with patch.object(host.subprocess, 'run', return_value=Mock(stdout='systemd 252')), \
                self.assertRaisesRegex(ValueError, 'systemd 255'):
            host.require_docker_host()

    def test_codex_companion_is_extracted_and_rechecked_before_reuse(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / 'tools.zip'
            payload = b'approved executable'
            digest = hashlib.sha256(payload).hexdigest()
            files = []
            with zipfile.ZipFile(archive, 'w') as bundle:
                bundle.writestr('bin/', b'')
                for name in ('codex', 'claude', 'opencode', 'node', 'gh', 'codex-code-mode-host'):
                    files.append(dict(path='bin/' + name, size=len(payload), sha256=digest))
                    bundle.writestr('bin/' + name, payload)
                bundle.writestr('manifest.json', json.dumps(dict(version=1, os='linux', arch='x86_64', files=files)))
            host.unpack_tools(archive, root)
            companion = root / 'tools/bin/codex-code-mode-host'
            self.assertEqual(payload, companion.read_bytes())
            self.assertEqual(0o555, companion.stat().st_mode & 0o777)
            (root / 'cairn').write_bytes(payload)
            metadata = release(size=len(payload), sha256=digest)
            host.verify_installed(root, metadata)
            companion.chmod(0o755)
            companion.write_bytes(b'corrupted companion')
            with self.assertRaisesRegex(ValueError, 'Installed native'):
                host.verify_installed(root, metadata)

    def test_workload_probe_executes_the_companion_as_the_managed_user(self):
        files = [{'path': 'bin/' + name} for name in
                 ('codex', 'claude', 'opencode', 'node', 'gh', 'codex-code-mode-host')]
        manifest = dict(version=1, os='linux', arch='x86_64', files=files)
        workload = Mock(pw_uid=1234, pw_gid=5678)
        with patch.object(host.pwd, 'getpwnam', return_value=workload), \
                patch.object(host.os, 'chown'), \
                patch.object(host.Path, 'read_text', return_value=json.dumps(manifest)), \
                patch.object(host.subprocess, 'run') as run:
            host.verify_workload_tools(Path('/synthetic'))
        companion = run.call_args_list[-1]
        self.assertEqual(['/synthetic/tools/bin/codex-code-mode-host', '--help'], companion.args[0])
        self.assertEqual(1234, companion.kwargs['user'])
        self.assertEqual(5678, companion.kwargs['group'])
        self.assertEqual([], companion.kwargs['extra_groups'])

    def test_inventory_rejects_unlisted_companions_and_duplicate_entries(self):
        files = [{'path': 'bin/' + name} for name in ('codex', 'claude', 'opencode', 'node', 'gh')]
        manifest = dict(version=1, os='linux', arch='x86_64', files=files)
        for extra in ('bin/unapproved-host', 'bin/codex'):
            with self.subTest(extra=extra), self.assertRaisesRegex(ValueError, 'inventory'):
                host.tool_inventory(dict(manifest, files=files + [{'path': extra}]))

    def test_unsupported_cgroups_are_rejected_before_enrollment_or_downloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for controllers in (None, 'cpu io pids'):
                if controllers is not None:
                    (root / 'cgroup.controllers').write_text(controllers)
                with patch.object(host, 'ROOT', root / 'state'), patch.object(host, 'CGROUP_ROOT', root), \
                        patch.object(host.subprocess, 'run', return_value=Mock(stdout='systemd 255')), \
                        patch.object(host.getpass, 'getpass') as enrollment, patch.object(host, 'prepare') as prepare:
                    with self.assertRaisesRegex(ValueError, 'cgroup v2'):
                        host.install('https://manager.example.test')
                    enrollment.assert_not_called()
                    prepare.assert_not_called()

    def test_native_storage_loads_only_its_missing_driver_and_checks_control(self):
        with patch.object(host.shutil, 'which', return_value='/synthetic/tool'), \
                patch.object(host.Path, 'is_dir', return_value=False), \
                patch.object(host.Path, 'stat', return_value=Mock(st_mode=host.stat.S_IFCHR)), \
                patch.object(host.subprocess, 'run') as run:
            host.require_storage()
            self.assertEqual(['modprobe', 'ublk_drv'], run.call_args.args[0])

    def test_loaded_storage_driver_does_not_need_its_module_file_again(self):
        with patch.object(host.shutil, 'which', return_value='/synthetic/tool'), \
                patch.object(host.Path, 'is_dir', return_value=True), \
                patch.object(host.Path, 'stat', return_value=Mock(st_mode=host.stat.S_IFCHR)), \
                patch.object(host.subprocess, 'run') as run:
            host.require_storage()
            run.assert_not_called()

    def test_missing_storage_tooling_or_wrong_control_device_fail_closed(self):
        with patch.object(host.shutil, 'which', return_value=None), self.assertRaisesRegex(ValueError, 'e2fsprogs'):
            host.require_storage()
        with patch.object(host.shutil, 'which', return_value='/synthetic/tool'), \
                patch.object(host.Path, 'is_dir', return_value=True), \
                patch.object(host.Path, 'stat', return_value=Mock(st_mode=host.stat.S_IFREG)), \
                self.assertRaisesRegex(ValueError, 'character device'):
            host.require_storage()

    def test_origin_cannot_redirect_credentials_to_another_path_or_scheme(self):
        self.assertEqual('https://manager.example.test', host.origin('https://manager.example.test/'))
        self.assertEqual('http://127.0.0.1:4310', host.origin('http://127.0.0.1:4310'))
        for value in ('http://remote.example.test', 'https://user:password@manager.test', 'https://manager.test/path', 'https://manager.test?token=x', 'file:///root/private'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                host.origin(value)

    def test_only_exact_compatible_release_artifacts_are_accepted(self):
        self.assertEqual(release(), host.validate_release(release()))
        for change in ({'sha256': '../escape'}, {'size': 1 << 40}, {'arch': 'aarch64'},
                       {'os': 'windows'}, {'protocolMin': 3}, {'toolsPath': 'https://elsewhere.test/tool'},
                       {'dataEpoch': None}, {'runtimeId': 'bad%runtime'}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                host.validate_release(release(**change))

    def test_partial_or_corrupt_download_cannot_replace_an_installed_binary(self):
        payload = b'approved bytes'
        metadata = release(size=len(payload), sha256=hashlib.sha256(payload).hexdigest())
        for received in (payload[:-1], b'x' * len(payload), payload + b'x'):
            with self.subTest(received=received), tempfile.TemporaryDirectory() as root:
                directory = Path(root)
                installed = directory / 'installed'
                installed.write_bytes(b'previous version')
                with patch.object(host, 'request', return_value=io.BytesIO(received)), self.assertRaises(ValueError):
                    host.download('https://manager.test', metadata, directory)
                self.assertEqual(b'previous version', installed.read_bytes())

    def test_download_is_verified_before_it_becomes_executable(self):
        payload = b'approved bytes'
        with tempfile.TemporaryDirectory() as root, patch.object(host, 'request', return_value=io.BytesIO(payload)):
            path = host.download('https://manager.test', release(size=len(payload), sha256=hashlib.sha256(payload).hexdigest()), Path(root))
            self.assertEqual(payload, path.read_bytes())
            self.assertEqual(0o555, path.stat().st_mode & 0o777)

    def test_hostile_tool_archives_are_rejected_without_writing_outside_staging(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root)
            archive = directory / 'archive.zip'
            with zipfile.ZipFile(archive, 'w') as bundle:
                bundle.writestr('../escape', b'outside')
                bundle.writestr('manifest.json', json.dumps({'version': 1}))
            with self.assertRaises(ValueError):
                host.unpack_tools(archive, directory)
            self.assertFalse((directory.parent / 'escape').exists())

    def test_update_umask_does_not_hide_tool_directories_from_workloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / 'tools.zip'
            files = []
            with zipfile.ZipFile(archive, 'w') as bundle:
                bundle.writestr('bin/', b'')
                for name in ('codex', 'claude', 'opencode', 'node', 'gh'):
                    payload = b'synthetic executable'
                    files.append(dict(path='bin/' + name, size=len(payload), sha256=hashlib.sha256(payload).hexdigest()))
                    bundle.writestr('bin/' + name, payload)
                bundle.writestr('manifest.json', json.dumps(dict(version=1, os='linux', arch='x86_64', files=files)))
            previous = os.umask(0o077)
            try:
                host.unpack_tools(archive, root)
            finally:
                os.umask(previous)
            self.assertEqual(0o755, (root / 'tools').stat().st_mode & 0o777)
            self.assertEqual(0o755, (root / 'tools/bin').stat().st_mode & 0o777)
            self.assertEqual(0o555, (root / 'tools/bin/codex').stat().st_mode & 0o777)

    def test_cached_installed_payloads_are_rechecked_before_reuse(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'tools/bin').mkdir(parents=True)
            payload = b'approved binary'
            digest = hashlib.sha256(payload).hexdigest()
            (root / 'cairn').write_bytes(payload)
            entries = []
            for name in ('codex', 'claude', 'opencode', 'node', 'gh'):
                (root / 'tools/bin' / name).write_bytes(payload)
                entries.append(dict(path='bin/' + name, size=len(payload), sha256=digest))
            (root / 'tools/manifest.json').write_text(json.dumps(dict(version=1, os='linux', arch='x86_64', files=entries)))
            metadata = release(size=len(payload), sha256=digest)
            host.verify_installed(root, metadata)
            (root / 'tools/bin/codex').write_bytes(b'corrupted bytes')
            with self.assertRaises(ValueError):
                host.verify_installed(root, metadata)

    def test_version_collection_preserves_current_previous_and_pinned_runtime(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            install, data = root / 'install', root / 'data'
            (install / 'versions').mkdir(parents=True)
            (data / 'state/native/run').mkdir(parents=True)
            (data / 'state/native/run/runtime.json').write_text(json.dumps(dict(runtimeId='pinned')))
            paths = []
            for index, runtime in enumerate(('current', 'previous', 'pinned', 'unused')):
                path = install / 'versions' / ((str(index) * 64) + '-' + (str(index) * 64))
                path.mkdir()
                (path / 'release.json').write_text(json.dumps(dict(runtimeId=runtime)))
                paths.append(path)
            with patch.object(host, 'INSTALL', install), patch.object(host, 'ROOT', data):
                host.collect_versions(paths[0], paths[1])
            self.assertTrue(all(path.exists() for path in paths[:3]))
            self.assertFalse(paths[3].exists())

    def test_failed_candidate_health_restores_the_previous_version(self):
        previous, candidate = Path('/synthetic/previous'), Path('/synthetic/candidate')
        active, trial, restored = Mock(), Mock(), Mock()
        trial.poll.return_value = None
        with patch.object(host, 'terminate') as terminate, patch.object(host, 'start_node', side_effect=[trial, restored]), \
                patch.object(host, 'health', side_effect=[False, True]), patch.object(host, 'select') as select:
            child, selected, error = host.activate(candidate, previous, active)
            self.assertIs(restored, child)
            self.assertEqual(previous, selected)
            self.assertIn('previous compatible version restored', error)
            select.assert_not_called()
            self.assertEqual([unittest.mock.call(active), unittest.mock.call(trial)], terminate.call_args_list)

    def test_success_selects_the_candidate_only_after_health_passes(self):
        previous, candidate = Path('/synthetic/previous'), Path('/synthetic/candidate')
        active, trial = Mock(), Mock()
        trial.poll.return_value = None
        with patch.object(host, 'terminate'), patch.object(host, 'start_node', return_value=trial), \
                patch.object(host, 'health', return_value=True), patch.object(host, 'select') as select:
            child, selected, error = host.activate(candidate, previous, active)
            self.assertIs(trial, child)
            self.assertEqual(candidate, selected)
            self.assertIsNone(error)
            select.assert_called_once_with(candidate)


if __name__ == '__main__':
    unittest.main()
