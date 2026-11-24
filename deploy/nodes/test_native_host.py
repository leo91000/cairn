"""Downloader and update recovery tests; no services, drivers or real credentials."""
import ast
import contextlib
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
                 os='linux', arch='x86_64', nativeHostProtocol=2, executionProtocol=2,
                 runtimeId='synthetic-runtime',
                 binaryPath='/internal/nodes/native-binary', toolsPath='/internal/nodes/native-tools')
    value.update(changes)
    return value


def extra(**changes):
    value = dict(extraToolsSha256='c' * 64, extraToolsSize=3, extraToolsPath='/internal/nodes/native-extra-tools')
    value.update(changes)
    return release(**value)


def write_bundle(archive, files, **manifest):
    entries = []
    with zipfile.ZipFile(archive, 'w') as bundle:
        bundle.writestr('bin/', b'')
        for name, payload in files.items():
            entries.append(dict(path='bin/' + name, size=len(payload), sha256=hashlib.sha256(payload).hexdigest()))
            bundle.writestr('bin/' + name, payload)
        bundle.writestr('manifest.json', json.dumps(dict(version=1, os='linux', arch='x86_64', files=entries, **manifest)))
    return entries


MAIN_TOOLS = ('codex', 'claude', 'opencode', 'node', 'gh', 'codex-code-mode-host')


class NativeHostTests(unittest.TestCase):
    def test_entrypoint_exposes_only_allowlisted_prerequisite_diagnostics(self):
        source = Path(SPEC.origin).read_text()
        entrypoint = ast.parse(source).body[-1]
        self.assertIsInstance(entrypoint, ast.If)
        code = compile(ast.Module(body=[entrypoint], type_ignores=[]), SPEC.origin, 'exec')
        for error, expected in [
            (host.PrerequisiteError('uidmap'), 'distribution uidmap package'),
            (host.PrerequisiteError('subids'), '/etc/subuid and /etc/subgid'),
            (host.PrerequisiteError('tun'), '/dev/net/tun'),
            (host.PrerequisiteError('systemd'), 'systemd 255'),
            (ValueError('synthetic-private-credential'), 'Native node failed: ValueError'),
            (OSError('synthetic-private-credential'), 'Native node failed: OSError'),
        ]:
            with self.subTest(error=type(error).__name__, expected=expected):
                scope = dict(vars(host), __name__='__main__', install=Mock(side_effect=error))
                output = io.StringIO()
                with patch.object(host.os, 'geteuid', return_value=0), \
                        patch.object(host.signal, 'signal'), \
                        patch.object(host.sys, 'argv', ['native-host.py', 'install', 'https://manager.example.test']), \
                        contextlib.redirect_stderr(output), self.assertRaises(SystemExit) as exit_status:
                    exec(code, scope)
                self.assertEqual(exit_status.exception.code, 1)
                scope['install'].assert_called_once_with('https://manager.example.test')
                self.assertIn(expected, output.getvalue())
                self.assertNotIn('synthetic-private-credential', output.getvalue())

    def test_missing_uidmap_reports_a_controlled_prerequisite(self):
        with patch.object(host.subprocess, 'run', return_value=Mock(stdout='systemd 255')), \
                patch.object(host.Path, 'exists', return_value=False), \
                patch.object(host.shutil, 'which', return_value=None), \
                self.assertRaisesRegex(host.PrerequisiteError, 'distribution uidmap package'):
            host.require_docker_host()

    def test_apparmor_profile_failure_does_not_expose_arbitrary_os_error(self):
        with patch.object(host.Path, 'exists', return_value=True), \
                patch.object(host.Path, 'read_text', return_value='1'), \
                patch.object(host.Path, 'write_text', side_effect=PermissionError('synthetic-private-credential')), \
                patch.object(host.shutil, 'which', return_value='/synthetic/apparmor_parser'), \
                self.assertRaisesRegex(host.PrerequisiteError, 'AppArmor tooling') as error:
            host.configure_docker_apparmor(Path('/synthetic/docker'))
        self.assertNotIn('synthetic-private-credential', str(error.exception))

    def test_mapping_helpers_accept_only_their_effective_global_file_capability(self):
        metadata = Mock(st_uid=0, st_mode=host.stat.S_IFREG | 0o755)
        def capability(*words):
            return b''.join(word.to_bytes(4, 'little') for word in words)
        for bit in (1 << 7, 1 << 6):
            for data, expected in [
                (capability(0x02000001, bit, 0, 0, 0), True),
                (capability(0x03000001, bit, 0, 0, 0, 0), True),
                (capability(0x02000000, bit, 0, 0, 0), False),
                (capability(0x02000001, bit | 1, 0, 0, 0), False),
                (capability(0x03000001, bit, 0, 0, 0, 1000), False),
            ]:
                with patch.object(host.Path, 'stat', return_value=metadata), patch.object(host.os, 'getxattr', return_value=data):
                    self.assertEqual(expected, host.trusted_mapping_helper('/synthetic', bit))
        with patch.object(host.Path, 'stat', return_value=Mock(st_uid=0, st_mode=host.stat.S_IFREG | 0o4755)):
            self.assertTrue(host.trusted_mapping_helper('/synthetic', 1 << 7))

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

    def test_extra_tool_bundle_is_optional_but_complete_when_named(self):
        self.assertEqual(extra(), host.validate_release(extra()))
        self.assertFalse(host.has_extra_tools(host.validate_release(release())))
        self.assertEqual('a' * 64 + '-' + 'b' * 64, host.version_name(release()))
        self.assertEqual('a' * 64 + '-' + 'b' * 64 + '-' + 'c' * 64, host.version_name(extra()))
        partial = extra()
        partial.pop('extraToolsSize')
        for value in (partial, extra(extraToolsSha256='../escape'), extra(extraToolsPath='/internal/nodes/native-tools'),
                      extra(extraToolsSize=256 * 1024 * 1024 + 1), extra(extraToolsSize=0), extra(extraToolsSize='3')):
            with self.subTest(value=value), self.assertRaises(ValueError):
                host.validate_release(value)

    def test_extra_tools_join_the_workload_bin_directory_and_are_rechecked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            payload = b'approved executable'
            write_bundle(root / 'tools.zip', {name: payload for name in MAIN_TOOLS})
            host.unpack_tools(root / 'tools.zip', root)
            write_bundle(root / 'extra-tools.zip', {'agy': b'approved antigravity'}, bundle='extra')
            host.unpack_extra_tools(root / 'extra-tools.zip', root)
            agy = root / 'tools/bin/agy'
            self.assertEqual(b'approved antigravity', agy.read_bytes())
            self.assertEqual(0o555, agy.stat().st_mode & 0o777)
            self.assertEqual(['bin/agy'], [entry['path'] for entry in json.loads((root / 'tools/extra-manifest.json').read_text())['files']])
            (root / 'cairn').write_bytes(payload)
            metadata = extra(size=len(payload), sha256=hashlib.sha256(payload).hexdigest())
            host.verify_installed(root, metadata)
            agy.chmod(0o755)
            agy.write_bytes(b'corrupted antigravity')
            with self.assertRaisesRegex(ValueError, 'Installed native'):
                host.verify_installed(root, metadata)
            (root / 'tools/extra-manifest.json').unlink()
            with self.assertRaises(OSError):
                host.verify_installed(root, metadata)

    def test_extra_tool_bundle_cannot_replace_or_add_unlisted_tools(self):
        for files, manifest in [({'agy': b'x', 'codex': b'x'}, dict(bundle='extra')), ({'codex': b'x'}, dict(bundle='extra')),
                                ({'agy': b'x'}, {}), ({'agy': b'x'}, dict(bundle='main'))]:
            with self.subTest(files=list(files), manifest=manifest), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                write_bundle(root / 'tools.zip', {name: b'approved' for name in MAIN_TOOLS})
                host.unpack_tools(root / 'tools.zip', root)
                write_bundle(root / 'extra-tools.zip', files, **manifest)
                with self.assertRaisesRegex(ValueError, 'native tool'):
                    host.unpack_extra_tools(root / 'extra-tools.zip', root)
                self.assertEqual(b'approved', (root / 'tools/bin/codex').read_bytes())
                self.assertFalse((root / 'tools/extra-manifest.json').exists())

    def test_prepare_installs_the_extra_bundle_named_by_the_release(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / 'stage'
            stage.mkdir()
            binary = b'#!/bin/sh\nexit 0\n'
            write_bundle(stage / 'tools.zip', {name: b'tool' for name in MAIN_TOOLS})
            write_bundle(stage / 'extra-tools.zip', {'agy': b'antigravity'}, bundle='extra')
            payloads = {'/internal/nodes/native-binary': binary, '/internal/nodes/native-tools': (stage / 'tools.zip').read_bytes(),
                        '/internal/nodes/native-extra-tools': (stage / 'extra-tools.zip').read_bytes()}
            digest = lambda data: hashlib.sha256(data).hexdigest()
            metadata = extra(sha256=digest(binary), size=len(binary),
                             toolsSha256=digest(payloads['/internal/nodes/native-tools']), toolsSize=len(payloads['/internal/nodes/native-tools']),
                             extraToolsSha256=digest(payloads['/internal/nodes/native-extra-tools']),
                             extraToolsSize=len(payloads['/internal/nodes/native-extra-tools']))
            requested = []

            def request(_manager, endpoint, _payload=None, _token=None):
                requested.append(endpoint)
                return io.BytesIO(payloads[endpoint])

            with patch.object(host, 'INSTALL', root / 'install'), patch.object(host, 'request', side_effect=request):
                directory = host.prepare('https://manager.test', metadata, token='synthetic')
            self.assertEqual(host.version_name(metadata), directory.name)
            self.assertEqual(list(payloads), requested)
            self.assertEqual(b'antigravity', (directory / 'tools/bin/agy').read_bytes())
            self.assertFalse((directory / 'extra-tools.zip').exists())
            host.verify_installed(directory, metadata)

    def test_version_installed_without_the_extra_bundle_is_replaced(self):
        with tempfile.TemporaryDirectory() as temporary:
            versions = Path(temporary)
            metadata = extra()
            legacy = versions / (metadata['sha256'] + '-' + metadata['toolsSha256'])
            current = versions / host.version_name(metadata)
            for directory in (legacy, current):
                directory.mkdir()
                # Supervisors record the full release they were offered, including fields they ignore.
                (directory / 'release.json').write_text(json.dumps(metadata))
            self.assertFalse(host.installed_matches(legacy, metadata))
            self.assertTrue(host.installed_matches(current, metadata))
            self.assertFalse(host.installed_matches(current, extra(extraToolsSha256='d' * 64)))
            (legacy / 'release.json').write_text(json.dumps(release()))
            self.assertTrue(host.installed_matches(legacy, release()))

    def test_workload_probe_runs_extra_tools_without_self_updates(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'tools').mkdir()
            (root / 'tools/manifest.json').write_text(json.dumps(dict(version=1, os='linux', arch='x86_64',
                                                                        files=[{'path': 'bin/' + name} for name in MAIN_TOOLS])))
            (root / 'tools/extra-manifest.json').write_text(json.dumps(dict(version=1, os='linux', arch='x86_64', bundle='extra',
                                                                              files=[{'path': 'bin/agy'}])))
            with patch.object(host.pwd, 'getpwnam', return_value=Mock(pw_uid=1234, pw_gid=5678)), \
                    patch.object(host.os, 'chown'), patch.object(host.subprocess, 'run') as run:
                host.verify_workload_tools(root)
            agy = run.call_args_list[-1]
            self.assertEqual([str(root / 'tools/bin/agy'), '--version'], agy.args[0])
            self.assertEqual('true', agy.kwargs['env']['AGY_CLI_DISABLE_AUTO_UPDATE'])

    def test_inventory_rejects_unlisted_companions_and_duplicate_entries(self):
        files = [{'path': 'bin/' + name} for name in ('codex', 'claude', 'opencode', 'node', 'gh', 'codex-code-mode-host')]
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
        with patch.object(host, 'require_data_pool'), patch.object(host, 'require_storage_kernel'), \
                patch.object(host, 'legacy_storage_present', return_value=True):
            with patch.object(host.shutil, 'which', return_value='/synthetic/tool'), \
                    patch.object(host.Path, 'is_dir', return_value=False), \
                    patch.object(host.Path, 'stat', return_value=Mock(st_mode=host.stat.S_IFCHR)), \
                    patch.object(host.subprocess, 'run') as run:
                host.require_storage()
                self.assertEqual(['modprobe', 'ublk_drv'], run.call_args.args[0])

    def test_loaded_storage_driver_does_not_need_its_module_file_again(self):
        with patch.object(host, 'require_data_pool'), patch.object(host, 'require_storage_kernel'), \
                patch.object(host, 'legacy_storage_present', return_value=True):
            with patch.object(host.shutil, 'which', return_value='/synthetic/tool'), \
                    patch.object(host.Path, 'is_dir', return_value=True), \
                    patch.object(host.Path, 'stat', return_value=Mock(st_mode=host.stat.S_IFCHR)), \
                    patch.object(host.subprocess, 'run') as run:
                host.require_storage()
                run.assert_not_called()

    def test_missing_storage_tooling_or_wrong_control_device_fail_closed(self):
        with patch.object(host, 'require_data_pool'), patch.object(host, 'require_storage_kernel'), \
                patch.object(host, 'legacy_storage_present', return_value=True):
            with patch.object(host.shutil, 'which', side_effect=lambda name: '/synthetic/rsync' if name == 'rsync' else None), self.assertRaisesRegex(ValueError, 'e2fsprogs'):
                host.require_storage()
            with patch.object(host.shutil, 'which', return_value='/synthetic/tool'), \
                    patch.object(host.Path, 'is_dir', return_value=True), \
                    patch.object(host.Path, 'stat', return_value=Mock(st_mode=host.stat.S_IFREG)), \
                    self.assertRaisesRegex(ValueError, 'character device'):
                host.require_storage()

    def test_interrupted_resize_sources_require_the_legacy_driver_after_reboot(self):
        for base in ('disks', 'environments'):
            with self.subTest(base=base), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                journal = root / 'state' / base / 'fixture-environment' / 'resize-source/journal-v2.sqlite'
                journal.parent.mkdir(parents=True)
                journal.write_bytes(b'fixture metadata presence')
                with patch.object(host, 'ROOT', root):
                    self.assertTrue(host.legacy_storage_present())
                journal.unlink()
                with patch.object(host, 'ROOT', root):
                    self.assertFalse(host.legacy_storage_present())

    def test_fresh_directory_storage_never_loads_a_block_driver(self):
        with patch.object(host, 'require_data_pool'), patch.object(host, 'require_storage_kernel'), \
                patch.object(host, 'legacy_storage_present', return_value=False), \
                patch.object(host.shutil, 'which', return_value='/synthetic/tool'), \
                patch.object(host.subprocess, 'run') as run:
            host.require_storage()
            run.assert_not_called()

    def test_quota_preflight_requires_a_separate_protected_ext4_pool(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pool = root / 'state/native-data'
            pool.mkdir(parents=True)
            original_stat = host.Path.stat
            def metadata(path, *args, **kwargs):
                if path == pool:
                    return SimpleNamespace(st_dev=original_stat(pool.parent).st_dev + 1, st_uid=0, st_mode=host.stat.S_IFDIR | 0o700)
                return original_stat(path, *args, **kwargs)
            def mounted(**changes):
                entry = dict(target=str(pool), fstype='ext4', options='rw,prjquota')
                entry.update(changes)
                return Mock(stdout=json.dumps(dict(filesystems=[entry])))
            with patch.object(host, 'ROOT', root), patch.object(host.Path, 'stat', autospec=True, side_effect=metadata), \
                    patch.object(host.subprocess, 'run', return_value=mounted()) as probe:
                host.require_data_pool()
                self.assertEqual(probe.call_args.args[0][0], 'findmnt')
                probe.return_value = mounted(fstype='xfs', options='rw,relatime,inode64,logbufs=8,logbsize=32k,prjquota')
                host.require_data_pool()
                for change in [dict(target=str(root)), dict(fstype='btrfs'), dict(options='rw'),
                               dict(fstype='xfs', options='rw,inode64,pqnoenforce'), dict(fstype='btrfs', options='rw,prjquota')]:
                    probe.return_value = mounted(**change)
                    with self.assertRaisesRegex(ValueError, 'prjquota'):
                        host.require_data_pool()
            with patch.object(host, 'ROOT', root), patch.object(host.subprocess, 'run', return_value=mounted()):
                with self.assertRaisesRegex(ValueError, 'prjquota'):
                    host.require_data_pool()

    def test_legacy_release_metadata_is_refused(self):
        value = release()
        value.pop('nativeHostProtocol')
        value.pop('executionProtocol')
        value['nodeProtocol'] = 2
        with self.assertRaisesRegex(ValueError, 'protocol'):
            host.validate_release(value)

    def test_origin_cannot_redirect_credentials_to_another_path_or_scheme(self):
        self.assertEqual('https://manager.example.test', host.origin('https://manager.example.test/'))
        self.assertEqual('http://127.0.0.1:4310', host.origin('http://127.0.0.1:4310'))
        for value in ('http://remote.example.test', 'https://user:password@manager.test', 'https://manager.test/path', 'https://manager.test?token=x', 'file:///root/private'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                host.origin(value)

    def test_only_exact_compatible_release_artifacts_are_accepted(self):
        self.assertEqual(release(), host.validate_release(release()))
        for change in ({'sha256': '../escape'}, {'size': 1 << 40}, {'arch': 'aarch64'},
                       {'os': 'windows'}, {'nativeHostProtocol': 1}, {'executionProtocol': 3}, {'toolsPath': 'https://elsewhere.test/tool'},
                       {'runtimeId': 'bad%runtime'}):
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
                for name in ('codex', 'claude', 'opencode', 'node', 'gh', 'codex-code-mode-host'):
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
            for name in ('codex', 'claude', 'opencode', 'node', 'gh', 'codex-code-mode-host'):
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
            for index, runtime in enumerate(('current', 'previous', 'pinned', 'unused', 'unused-extra')):
                path = install / 'versions' / '-'.join([str(index) * 64] * (3 if index % 2 else 2))
                path.mkdir()
                (path / 'release.json').write_text(json.dumps(dict(runtimeId=runtime)))
                paths.append(path)
            with patch.object(host, 'INSTALL', install), patch.object(host, 'ROOT', data):
                host.collect_versions(paths[0], paths[1])
            self.assertTrue(all(path.exists() for path in paths[:3]))
            self.assertFalse(paths[3].exists())
            self.assertFalse(paths[4].exists())

    def test_failed_candidate_health_retains_candidate_for_restart(self):
        candidate = Path('/synthetic/candidate')
        active, trial = Mock(), Mock()
        trial.poll.return_value = None
        with patch.object(host, 'terminate') as terminate, patch.object(host, 'start_node', return_value=trial) as start, \
                patch.object(host, 'health', return_value=False), patch.object(host, 'select') as select:
            with self.assertRaisesRegex(RuntimeError, 'candidate retained'):
                host.activate(candidate, active)
            start.assert_called_once_with(candidate)
            select.assert_called_once_with(candidate)
            self.assertEqual([unittest.mock.call(active), unittest.mock.call(trial)], terminate.call_args_list)

    def test_healthy_candidate_runs_after_selection(self):
        candidate = Path('/synthetic/candidate')
        active, trial = Mock(), Mock()
        trial.poll.return_value = None
        with patch.object(host, 'terminate'), patch.object(host, 'start_node', return_value=trial), \
                patch.object(host, 'health', return_value=True), patch.object(host, 'select') as select:
            child = host.activate(candidate, active)
            self.assertIs(trial, child)
            select.assert_called_once_with(candidate)

    def test_restarted_healthy_candidate_completes_maintenance_even_after_a_lost_reply(self):
        for lost_reply in [False, True]:
            with self.subTest(lost_reply=lost_reply), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary) / 'state'
                install = Path(temporary) / 'install'
                identity = root / 'data/node/identity.json'
                identity.parent.mkdir(parents=True)
                identity.write_text(json.dumps({'token': 'synthetic-node-token'}))
                (root / 'settings.json').write_text(json.dumps({'manager': 'https://manager.example.test'}))
                previous_release = release(sha256='0' * 64, toolsSha256='1' * 64, runtimeId='previous')
                candidate_release = release(sha256='2' * 64, toolsSha256='3' * 64, runtimeId='candidate')
                versions = []
                for metadata in [previous_release, candidate_release]:
                    directory = install / 'versions' / (metadata['sha256'] + '-' + metadata['toolsSha256'])
                    directory.mkdir(parents=True)
                    (directory / 'release.json').write_text(json.dumps(metadata))
                    versions.append(directory)
                previous, candidate = versions
                maintenance = None
                completions = []

                def request(_manager, path, body=None, token=None):
                    nonlocal maintenance
                    if path.endswith('/native-release'):
                        return candidate_release
                    self.assertEqual(token, 'synthetic-node-token')
                    if body['action'] == 'drain':
                        maintenance = 'ready-to-update'
                    elif body['action'] == 'complete':
                        self.assertIsNone(body['error'])
                        maintenance = None
                        completions.append(body)
                        host.STOP = True
                        if lost_reply and len(completions) == 1:
                            raise OSError('Synthetic lost acknowledgement')
                    return {'maintenance': maintenance}

                def terminate(child):
                    child.poll.return_value = 0

                children = [Mock() for _ in range(4)]
                for child in children:
                    child.poll.return_value = None
                with patch.object(host, 'ROOT', root), patch.object(host, 'INSTALL', install), \
                        patch.object(host, 'STOP', False), patch.object(host, 'require_storage'), \
                        patch.object(host, 'require_docker_host'), \
                        patch.object(host, 'prepare_docker_tools', return_value=install / 'docker-tools'), \
                        patch.object(host, 'configure_docker_apparmor') as apparmor, \
                        patch.object(host, 'start_node', side_effect=children), \
                        patch.object(host, 'terminate', side_effect=terminate), \
                        patch.object(host, 'prepare', return_value=candidate), \
                        patch.object(host, 'verify_workload_tools'), patch.object(host, 'json_request', side_effect=request):
                    host.select(previous)
                    with patch.object(host, 'health', side_effect=[True, False]):
                        with self.assertRaisesRegex(RuntimeError, 'no longer running'):
                            host.run()
                    self.assertEqual((install / 'current').resolve(), candidate)
                    self.assertEqual(maintenance, 'ready-to-update')
                    self.assertTrue((root / 'update-pending.json').exists())
                    self.assertEqual(completions, [])

                    with patch.object(host, 'health', return_value=True):
                        host.run()
                        if lost_reply:
                            self.assertTrue((root / 'update-pending.json').exists())
                            host.STOP = False
                            host.run()
                    self.assertIsNone(maintenance)
                    self.assertFalse((root / 'update-pending.json').exists())
                    self.assertEqual(len(completions), 2 if lost_reply else 1)
                    apparmor.assert_called_with(install / 'docker-tools')


PLAN = dict(filesystem='ext4', reflink=False, usedBytes=10 * 1024 ** 2, skipped=['.cairn-bases', 'lost+found'], blockers=[],
            environments=[dict(run='11111111-1111-4111-8111-111111111111', project=3, usedBytes=4096, inodes=7,
                               blockHardBytes=8 << 20, inodeHard=500),
                          dict(run='22222222-2222-4222-8222-222222222222', project=0, usedBytes=0, inodes=0,
                               blockHardBytes=0, inodeHard=0)])
OLD = dict(unit='var-lib-cairn\\x2dnative-state-native\\x2ddata.mount', fragment='/etc/systemd/system/var-lib-cairn\\x2dnative-state-native\\x2ddata.mount',
           what='/var/lib/cairn-native/native-data.ext4', type='ext4', options='loop,prjquota,nodiscard')


def pool_arguments(**changes):
    from types import SimpleNamespace
    value = dict(device=None, image=None, size=None, force=False, stop=False, dry_run=False)
    value.update(changes)
    return SimpleNamespace(**value)


class PoolMigrationTests(unittest.TestCase):
    """Planning and ordering of migrate-pool with fake system commands."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / 'state/native-data').mkdir(parents=True)
        self.units = self.root / 'units'
        self.units.mkdir()
        for patcher in (patch.object(host, 'ROOT', self.root), patch.object(host, 'UNIT_DIRECTORY', self.units)):
            patcher.start()
            self.addCleanup(patcher.stop)

    def tearDown(self):
        self.temporary.cleanup()

    def test_sizes_and_paths_are_validated_before_any_change(self):
        self.assertEqual(500 * 1024 ** 3, host.parse_size('500G'))
        self.assertEqual(2 * 1024 ** 4, host.parse_size('2T'))
        self.assertEqual(4 * 1024 ** 3, host.parse_size(str(4 * 1024 ** 3)))
        for value in ('', 'big', '-1G', '512M'):
            with self.subTest(value=value), self.assertRaises(host.MigrationError):
                host.parse_size(value)
        self.assertEqual(Path('/var/lib/cairn-native/native-data.xfs'), host.safe_path('/var/lib/cairn-native/native-data.xfs'))
        for value in ('relative.xfs', '/var/lib/with space.xfs', '/var/lib/../etc/x', '/var/%n.xfs'):
            with self.subTest(value=value), self.assertRaises(host.MigrationError):
                host.safe_path(value)
        with self.assertRaisesRegex(host.MigrationError, 'outside the data pool'):
            host.migrate_pool(pool_arguments(image=str(self.root / 'state/native-data/pool.xfs'), size='10G'))
        self.assertFalse(host.migration_directory().exists())

    def test_running_node_is_refused_unless_stop_is_explicit(self):
        with patch.object(host, 'active_units', return_value=['cairn-native-node.service']), \
                patch.object(host, 'controller_running', return_value=False), \
                patch.object(host, 'checked') as checked, \
                self.assertRaisesRegex(host.MigrationError, '--stop'):
            host.require_stopped(False)
        checked.assert_not_called()
        states = iter([['cairn-native-guardian-a.service', host.SERVICE], []])
        with patch.object(host, 'active_units', side_effect=lambda: next(states)), \
                patch.object(host, 'controller_running', return_value=False), \
                patch.object(host, 'checked') as checked:
            host.require_stopped(True)
        self.assertEqual([call.args[:3] for call in checked.call_args_list],
                         [('systemctl', 'stop', host.SERVICE), ('systemctl', 'stop', 'cairn-native-guardian-a.service')])
        with patch.object(host, 'active_units', return_value=[]), \
                patch.object(host, 'controller_running', return_value=True), \
                self.assertRaisesRegex(host.MigrationError, 'controller.lock'):
            host.require_stopped(True)

    def test_active_unit_listing_ignores_the_pool_loop_and_slices(self):
        listing = '\n'.join([host.LOOP_SERVICE + ' loaded active exited loop', 'cairn-native-node.slice loaded active active x',
                             'cairn-native-abc.service loaded active running workload', ''])
        with patch.object(host, 'checked', return_value=listing):
            self.assertEqual(['cairn-native-abc.service'], host.active_units())

    def test_device_targets_must_be_unused_unless_forced(self):
        from types import SimpleNamespace
        device = Path('/dev/synthetic-volume')
        metadata = SimpleNamespace(st_mode=host.stat.S_IFBLK | 0o600, st_rdev=os.makedev(253, 7))

        def system(mounted=False, signature=''):
            def run(args, **_options):
                if args[0] == 'findmnt':
                    return Mock(returncode=0 if mounted else 1, stdout='')
                return Mock(returncode=0 if signature else 2, stdout=signature)
            return run
        with patch.object(host.Path, 'stat', return_value=metadata):
            with patch.object(host.subprocess, 'run', side_effect=system()):
                self.assertFalse(host.check_device(device, False))
            with patch.object(host.subprocess, 'run', side_effect=system(mounted=True)), \
                    self.assertRaisesRegex(host.MigrationError, 'mounted'):
                host.check_device(device, True)
            with patch.object(host.subprocess, 'run', side_effect=system(signature='TYPE=ext4\nUUID=old\n')):
                with self.assertRaisesRegex(host.MigrationError, 'already contains ext4'):
                    host.check_device(device, False)
                self.assertFalse(host.check_device(device, True))
            with patch.object(host.subprocess, 'run', side_effect=system(signature='TYPE=xfs\nUUID=ours\n')):
                self.assertTrue(host.check_device(device, False, uuid='ours'))
        with patch.object(host.Path, 'stat', return_value=SimpleNamespace(st_mode=host.stat.S_IFREG | 0o600, st_rdev=0)), \
                self.assertRaisesRegex(host.MigrationError, 'not a block device'):
            host.check_device(device, True)

    def test_units_attach_the_image_with_direct_io_and_mount_by_uuid(self):
        pool = Path('/var/lib/cairn-native/state/native-data')
        image = Path('/var/lib/cairn-native/native-data.xfs')
        loop = host.loop_unit(image, OLD['unit'])
        self.assertIn('losetup --direct-io=on --find --show /var/lib/cairn-native/native-data.xfs', loop)
        self.assertIn('RequiresMountsFor=/var/lib/cairn-native', loop)
        self.assertIn('Before=' + OLD['unit'], loop)
        self.assertNotRegex(loop.replace('$$', ''), r'\$')
        unit = host.pool_unit(pool, 'f00d', image)
        for line in ('What=/dev/disk/by-uuid/f00d', 'Where=' + str(pool), 'Type=xfs', 'Options=prjquota',
                     'Before=' + host.SERVICE, 'Requires=' + host.LOOP_SERVICE, 'WantedBy=local-fs.target'):
            self.assertIn(line + '\n', unit)
        self.assertNotIn(host.LOOP_SERVICE, host.pool_unit(pool, 'f00d', None))

    def test_pool_mounted_from_fstab_is_not_rewritten(self):
        pool = host.pool_path()
        responses = {'systemd-escape': OLD['unit'] + '\n', 'systemctl': 'FragmentPath=/run/systemd/generator/x.mount\nSourcePath=/etc/fstab\n'}
        with patch.object(host, 'mount_of', return_value=dict(source='/dev/loop0', fstype='ext4', options='rw,prjquota')), \
                patch.object(host, 'checked', side_effect=lambda *args, **_: responses[args[0]]), \
                self.assertRaisesRegex(host.MigrationError, 'fstab'):
            host.old_pool(pool)
        with patch.object(host, 'mount_of', return_value=dict(source='/dev/sdb1', fstype='xfs', options='rw,prjquota')), \
                self.assertRaisesRegex(host.MigrationError, 'ext4 project-quota pool'):
            host.old_pool(pool)

    def test_existing_unit_settings_are_preserved_for_rollback(self):
        fragment = self.units / OLD['unit']
        fragment.write_text('[Mount]\nWhat=/var/lib/cairn-native/native-data.ext4\nWhere=/x\nType=ext4\nOptions=loop,prjquota,nodiscard\n')
        responses = {'systemd-escape': OLD['unit'] + '\n', 'systemctl': f'FragmentPath={fragment}\nSourcePath=\n'}
        with patch.object(host, 'mount_of', return_value=dict(source='/dev/loop0', fstype='ext4', options='rw,relatime,prjquota')), \
                patch.object(host, 'checked', side_effect=lambda *args, **_: responses[args[0]]):
            old = host.old_pool(host.pool_path())
        self.assertEqual(dict(OLD, fragment=str(fragment)), old)

    def test_dry_run_prints_the_plan_without_changing_anything(self):
        tool = Mock(return_value=PLAN)
        output = io.StringIO()
        with patch.object(host, 'mount_of', return_value=dict(source='/dev/loop0', fstype='ext4', options='rw,prjquota')), \
                patch.object(host, 'old_pool', return_value=OLD), patch.object(host, 'pool_tool', tool), \
                patch.object(host, 'active_units', return_value=[host.SERVICE]), \
                patch.object(host.os, 'statvfs', return_value=Mock(f_bavail=1 << 40, f_frsize=1)), \
                patch.object(host, 'checked') as checked, contextlib.redirect_stdout(output):
            host.migrate_pool(pool_arguments(image='/var/lib/cairn-native/native-data.xfs', size='100G', dry_run=True))
        checked.assert_not_called()
        self.assertEqual([call.args[0] for call in tool.call_args_list], ['plan'])
        self.assertFalse(host.migration_directory().exists())
        text = output.getvalue()
        for expected in ('mkfs.xfs -m crc=1,reflink=1', 'project 3', 'no project quota assigned', '.cairn-bases',
                         'keep it unchanged', host.LOOP_SERVICE, 'The node is running'):
            self.assertIn(expected, text)

    def test_blockers_and_insufficient_space_stop_before_any_change(self):
        blocked = dict(PLAN, blockers=['Environment x has an unfinished legacy journal conversion.'])
        with patch.object(host, 'old_pool', return_value=OLD), patch.object(host, 'pool_tool', return_value=blocked), \
                self.assertRaisesRegex(host.MigrationError, 'legacy journal conversion'):
            host.migrate_pool(pool_arguments(device='/dev/synthetic'))
        with patch.object(host, 'old_pool', return_value=OLD), patch.object(host, 'pool_tool', return_value=PLAN), \
                patch.object(host.os, 'statvfs', return_value=Mock(f_bavail=1, f_frsize=1)), \
                self.assertRaisesRegex(host.MigrationError, 'needs at least'):
            host.migrate_pool(pool_arguments(image='/var/lib/cairn-native/native-data.xfs', size='100G'))
        self.assertFalse(host.migration_directory().exists())

    def test_copy_verification_must_report_no_difference(self):
        outputs = iter(['', '>f.st...... data/home/file\n'])
        with patch.object(host, 'checked', side_effect=lambda *args, **_: next(outputs)) as checked, \
                self.assertRaisesRegex(host.MigrationError, 'found differences'):
            host.copy_environment('run', Path('/old'), Path('/new'))
        copy, verify = (call.args for call in checked.call_args_list)
        self.assertEqual(copy[:5], ('rsync', '-aHAX', '--numeric-ids', '--sparse', '--delete'))
        self.assertEqual(verify[:5], ('rsync', '-aHAXnc', '--numeric-ids', '--delete', '--itemize-changes'))
        for command in (copy, verify):
            self.assertIn('--filter=-x trusted.SGI_ACL_FILE', command)
            self.assertEqual(command[-2:], ('/old/run/', '/new/run/'))

    def fake_migration(self, mountpoint, events):
        def tool(action, *args):
            events.append((action, *[str(arg) for arg in args[-1:]]))
            if action == 'plan':
                return PLAN
            if action == 'check':
                return dict(filesystem='xfs', reflink=True, environments=2)
            return {}

        def new_pool(record, old, pool):
            events.append(('mount-new', record['phase']))
        return [patch.object(host, 'old_pool', return_value=OLD), patch.object(host, 'pool_tool', side_effect=tool),
                patch.object(host, 'require_stopped'), patch.object(host, 'prepare_target', return_value=mountpoint),
                patch.object(host, 'copy_environment', side_effect=lambda run, *_: events.append(('copy', run))),
                patch.object(host, 'checked', side_effect=lambda *args, **_: events.append(args[:1]) or ''),
                patch.object(host, 'unmount_old_pool', side_effect=lambda *_: events.append(('unmount-old',))),
                patch.object(host, 'mount_new_pool', side_effect=new_pool),
                patch.object(host, 'new_pool_mounted', side_effect=lambda *_: any(event[0] == 'mount-new' for event in events)),
                patch.object(host, 'mount_of', return_value=None)]

    def test_migration_copies_every_environment_before_switching_records(self):
        mountpoint = self.root / 'target'
        (mountpoint / 'stale-attempt').mkdir(parents=True)
        events = []
        with contextlib.ExitStack() as stack, contextlib.redirect_stdout(io.StringIO()) as output:
            for patcher in self.fake_migration(mountpoint, events):
                stack.enter_context(patcher)
            (self.units / OLD['unit']).write_text('[Mount]\nWhat=old\n')
            host.migrate_pool(pool_arguments(device='/dev/synthetic'))
        first, second = PLAN['environments'][0]['run'], PLAN['environments'][1]['run']
        self.assertEqual(events, [('plan', str(self.root / 'state')),
                                  ('prepare', first), ('copy', first), ('commit', first),
                                  ('prepare', second), ('copy', second), ('commit', second),
                                  ('umount',), ('unmount-old',), ('mount-new', 'copied'),
                                  ('switch', str(host.migration_directory())), ('check', str(self.root / 'state'))])
        self.assertFalse((mountpoint / 'stale-attempt').exists())
        self.assertEqual('complete', host.load_migration()['phase'])
        self.assertIn('rollback-pool', output.getvalue())

    def test_rerun_after_the_switch_only_completes_the_records(self):
        host.migration_directory().mkdir()
        host.save_migration(dict(version=1, target=dict(device='/dev/synthetic'), phase='switched', old=OLD, uuid='f00d'))
        events = []
        with contextlib.ExitStack() as stack, contextlib.redirect_stdout(io.StringIO()):
            for patcher in self.fake_migration(self.root / 'target', events):
                stack.enter_context(patcher)
            stack.enter_context(patch.object(host, 'new_pool_mounted', return_value=True))
            host.migrate_pool(pool_arguments())
        self.assertEqual([event[0] for event in events], ['switch', 'check'])
        with self.assertRaisesRegex(host.MigrationError, 'in progress'):
            host.migrate_pool(pool_arguments(device='/dev/another'))

    def test_resume_after_a_crash_between_mounts_remounts_the_original_first(self):
        host.migration_directory().mkdir()
        (self.root / 'target').mkdir()
        host.save_migration(dict(version=1, target=dict(device='/dev/synthetic'), phase='copied', old=OLD, uuid='f00d'))
        events = []
        with contextlib.ExitStack() as stack, contextlib.redirect_stdout(io.StringIO()):
            for patcher in self.fake_migration(self.root / 'target', events):
                stack.enter_context(patcher)
            stack.enter_context(patch.object(host, 'remount_old_pool', side_effect=lambda *_: events.append(('remount-old',))))
            host.migrate_pool(pool_arguments())
        self.assertEqual(events[:2], [('remount-old',), ('plan', str(self.root / 'state'))])
        self.assertIn(('copy', PLAN['environments'][0]['run']), events)

    def test_rollback_restores_the_original_unit_and_records(self):
        host.migration_directory().mkdir()
        host.save_migration(dict(version=1, target=dict(image='/var/lib/cairn-native/native-data.xfs', size=1 << 30),
                                 phase='complete', old=OLD, uuid='f00d'))
        backup = host.migration_directory() / 'units' / OLD['unit']
        backup.parent.mkdir()
        backup.write_text('original ext4 unit\n')
        (self.units / OLD['unit']).write_text(host.pool_unit(host.pool_path(), 'f00d', 'image'))
        (self.units / host.LOOP_SERVICE).write_text('loop\n')
        mounts = iter([None, dict(fstype='xfs', source='/dev/loop1'), None, dict(fstype='ext4', source='/dev/loop0')])
        commands = []
        with patch.object(host, 'require_stopped'), patch.object(host, 'mount_of', side_effect=lambda *_: next(mounts)), \
                patch.object(host, 'checked', side_effect=lambda *args, **_: commands.append(args) or ''), \
                patch.object(host, 'attached_loop', return_value=Path('/dev/loop1')), \
                patch.object(host, 'pool_tool', side_effect=lambda action, *_: commands.append(('pool', action)) or {}), \
                contextlib.redirect_stdout(io.StringIO()):
            host.rollback_pool(pool_arguments())
        self.assertEqual('original ext4 unit\n', (self.units / OLD['unit']).read_text())
        self.assertFalse((self.units / host.LOOP_SERVICE).exists())
        self.assertEqual(commands, [('systemctl', 'stop', OLD['unit']), ('systemctl', 'disable', '--now', host.LOOP_SERVICE),
                                    ('losetup', '--detach', '/dev/loop1'), ('systemctl', 'daemon-reload'),
                                    ('systemctl', 'start', OLD['unit']), ('pool', 'rollback'), ('pool', 'check')])
        self.assertFalse(host.migration_directory().exists())
        self.assertTrue(any(path.name.startswith('native-pool-migration.rolled-back-') for path in self.root.iterdir()))

    def test_migration_refusals_are_printed_for_the_operator(self):
        source = Path(SPEC.origin).read_text()
        entrypoint = ast.parse(source).body[-1]
        code = compile(ast.Module(body=[entrypoint], type_ignores=[]), SPEC.origin, 'exec')
        scope = dict(vars(host), __name__='__main__', migrate_pool=Mock(side_effect=host.MigrationError('Stop the native node first')))
        output = io.StringIO()
        with patch.object(host.os, 'geteuid', return_value=0), patch.object(host.signal, 'signal'), \
                patch.object(host.sys, 'argv', ['native-host.py', 'migrate-pool', '--device', '/dev/x']), \
                contextlib.redirect_stderr(output), self.assertRaises(SystemExit):
            exec(code, scope)
        self.assertIn('Native pool migration stopped: Stop the native node first', output.getvalue())


if __name__ == '__main__':
    unittest.main()
