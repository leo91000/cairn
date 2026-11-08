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
import urllib.error
from unittest.mock import Mock, patch
import zipfile

SPEC = importlib.util.spec_from_file_location('native_host', Path(__file__).with_name('native-host.py'))
host = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(host)


def release(**changes):
    value = dict(sha256='a' * 64, toolsSha256='b' * 64, size=3, toolsSize=3,
                 os='linux', arch='x86_64', nodeProtocol=2,
                 runtimeId='synthetic-runtime',
                 binaryPath='/internal/nodes/native-binary', toolsPath='/internal/nodes/native-tools')
    value.update(changes)
    return value


class DockerToolPreparationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.payload = b'approved synthetic executable'
        self.url = 'https://example.test/pinned-docker'
        self.tools = ((self.url, hashlib.sha256(self.payload).hexdigest(), {'': 'bin/docker'}),)
        self.now = 0
        self.sleeps = []
        self.errors = []
        self.log = io.StringIO()
        for replacement in (
            patch.object(host, 'INSTALL', self.root),
            patch.object(host, 'DOCKER_TOOLS', self.tools),
            patch.object(host, 'STOP', False),
            patch.object(host.time, 'monotonic', side_effect=lambda: self.now),
            patch.object(host.time, 'sleep', side_effect=self.sleep),
            patch.object(host.subprocess, 'run', return_value=Mock(stdout='{"ApiVersion":"1.53"}')),
            contextlib.redirect_stderr(self.log),
        ):
            replacement.__enter__()
            self.addCleanup(replacement.__exit__, None, None, None)
        self.open_url = self.enterContext(patch.object(host.urllib.request, 'urlopen'))
        # An incomplete synthetic response must never fall through to a network.
        self.open_url.side_effect = AssertionError('Unexpected acquisition')

    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.now += seconds

    def error(self, status):
        body = io.BytesIO(b'private response body')
        error = urllib.error.HTTPError(self.url, status, 'synthetic error', {}, body)
        self.errors.append(body)
        return error

    def assert_unpublished(self):
        self.assertFalse((self.root / 'docker-tools/v2').exists())
        self.assertEqual([], list((self.root / 'docker-tools').iterdir()))
        self.assertTrue(all(body.closed for body in self.errors))
        self.assertNotIn('private response body', self.log.getvalue())
        self.assertNotIn(self.url, self.log.getvalue())

    def test_gateway_failures_recover_only_before_the_verified_atomic_publish(self):
        for status in (502, 503, 504):
            with self.subTest(status=status):
                self.open_url.side_effect = [self.error(status), io.BytesIO(self.payload)]
                target = host.prepare_docker_tools()
                self.assertEqual(self.payload, (target / 'bin/docker').read_bytes())
                self.assertEqual(hashlib.sha256(self.payload).hexdigest(),
                                 json.loads((target / 'manifest.json').read_text())['bin/docker'])
                self.assertEqual(0o555, (target / 'bin/docker').stat().st_mode & 0o777)
                self.assertEqual(0o444, (target / 'manifest.json').stat().st_mode & 0o777)
                self.assertEqual([target], list(target.parent.iterdir()))
                calls = self.open_url.call_count
                host.prepare_docker_tools()
                self.assertEqual(calls, self.open_url.call_count)
                host.shutil.rmtree(target)
        self.assertTrue(all(body.closed for body in self.errors))

    def test_persistent_504_exhausts_three_attempts_and_cleans_the_stage(self):
        self.open_url.side_effect = [self.error(504) for _ in range(3)]
        with self.assertRaises(urllib.error.HTTPError) as failure:
            host.prepare_docker_tools()
        self.assertEqual(504, failure.exception.code)
        self.assertEqual(3, self.open_url.call_count)
        self.assertEqual([1, 2], self.sleeps)
        self.assert_unpublished()

    def test_acquisition_attempts_share_the_original_sixty_second_budget(self):
        def acquire(*args, **kwargs):
            if self.open_url.call_count == 1:
                self.now += 15
                raise self.error(504)
            return io.BytesIO(self.payload)
        self.open_url.side_effect = acquire
        host.prepare_docker_tools()
        self.assertEqual([60, 44], [call.kwargs['timeout'] for call in self.open_url.call_args_list])
        self.assertTrue(all(call.args == (self.url,) for call in self.open_url.call_args_list))

    def test_no_backoff_or_request_when_the_budget_cannot_fit_the_backoff(self):
        def acquire(*args, **kwargs):
            self.now = 59
            raise self.error(504)
        self.open_url.side_effect = acquire
        with self.assertRaises(urllib.error.HTTPError):
            host.prepare_docker_tools()
        self.assertEqual(1, self.open_url.call_count)
        self.assertEqual([], self.sleeps)
        self.assert_unpublished()

    def test_delayed_wakeup_cannot_start_a_request_past_the_deadline(self):
        def delayed_sleep(seconds):
            self.now = 60
        self.open_url.side_effect = self.error(504)
        with patch.object(host.time, 'sleep', side_effect=delayed_sleep), \
                self.assertRaisesRegex(TimeoutError, 'deadline exhausted'):
            host.prepare_docker_tools()
        self.assertEqual(1, self.open_url.call_count)
        self.assert_unpublished()

    def test_late_open_response_is_closed_without_reading_or_publishing(self):
        response = io.BytesIO(self.payload)
        def acquire(*args, **kwargs):
            self.now = 60
            return response
        self.open_url.side_effect = acquire
        with self.assertRaisesRegex(TimeoutError, 'deadline exhausted'):
            host.prepare_docker_tools()
        self.assertTrue(response.closed)
        self.assertEqual(1, self.open_url.call_count)
        self.assert_unpublished()

    def test_exhaustion_of_a_later_artifact_cannot_publish_the_first(self):
        tools = self.tools + (('https://example.test/pinned-plugin', self.tools[0][1], {'': 'plugins/docker-buildx'}),)
        self.open_url.side_effect = [io.BytesIO(self.payload), *(self.error(504) for _ in range(3))]
        with patch.object(host, 'DOCKER_TOOLS', tools), self.assertRaises(urllib.error.HTTPError):
            host.prepare_docker_tools()
        self.assertEqual(4, self.open_url.call_count)
        self.assert_unpublished()

    def test_stop_prevents_retry(self):
        self.open_url.side_effect = self.error(504)
        with patch.object(host, 'STOP', True), self.assertRaises(urllib.error.HTTPError):
            host.prepare_docker_tools()
        self.assertEqual(1, self.open_url.call_count)
        self.assertEqual([], self.sleeps)
        self.assert_unpublished()

    def test_permanent_http_and_transport_errors_are_not_retried(self):
        for status in (400, 401, 403, 404, 429, 500, 'transport', 'timeout'):
            error = (urllib.error.URLError('synthetic transport failure') if status == 'transport' else
                     TimeoutError('synthetic timeout') if status == 'timeout' else self.error(status))
            with self.subTest(error=error), self.assertRaises(type(error)):
                self.open_url.side_effect = error
                host.prepare_docker_tools()
            self.assertEqual([], self.sleeps)
            self.assert_unpublished()
        self.assertEqual(8, self.open_url.call_count)

    def test_retry_does_not_accept_corrupt_or_truncated_payloads(self):
        for payload in (self.payload[:-1], b'x' * len(self.payload)):
            with self.subTest(payload=payload):
                self.open_url.side_effect = [self.error(504), io.BytesIO(payload)]
                with self.assertRaisesRegex(ValueError, 'digest mismatch'):
                    host.prepare_docker_tools()
                self.assert_unpublished()
        self.assertEqual(4, self.open_url.call_count)

    def test_transfer_failure_is_terminal_and_does_not_publish_partial_bytes(self):
        response = self.enterContext(contextlib.closing(io.BytesIO(self.payload)))
        failing_response = Mock(wraps=response)
        failing_response.__enter__ = Mock(return_value=failing_response)
        failing_response.__exit__ = Mock(side_effect=lambda *args: response.close())
        failing_response.read.side_effect = [b'partial', TimeoutError('transfer stalled')]
        self.open_url.side_effect = [self.error(504), failing_response]
        with self.assertRaises(TimeoutError):
            host.prepare_docker_tools()
        self.assertEqual(2, self.open_url.call_count)
        self.assertTrue(response.closed)
        self.assert_unpublished()

    def test_oversized_response_after_retry_is_terminal_before_writing(self):
        class OversizedChunk(bytes):
            def __len__(self):
                return 256 * 1024 * 1024 + 1
        response = self.enterContext(contextlib.closing(io.BytesIO()))
        oversized = Mock(wraps=response)
        oversized.__enter__ = Mock(return_value=oversized)
        oversized.__exit__ = Mock(side_effect=lambda *args: response.close())
        oversized.read.return_value = OversizedChunk(b'x')
        self.open_url.side_effect = [self.error(504), oversized]
        with self.assertRaisesRegex(ValueError, 'size bound'):
            host.prepare_docker_tools()
        self.assertEqual(2, self.open_url.call_count)
        self.assertTrue(response.closed)
        self.assert_unpublished()


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
                       {'os': 'windows'}, {'nodeProtocol': 3}, {'toolsPath': 'https://elsewhere.test/tool'},
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
            for index, runtime in enumerate(('current', 'previous', 'pinned', 'unused')):
                path = install / 'versions' / ((str(index) * 64) + '-' + (str(index) * 64))
                path.mkdir()
                (path / 'release.json').write_text(json.dumps(dict(runtimeId=runtime)))
                paths.append(path)
            with patch.object(host, 'INSTALL', install), patch.object(host, 'ROOT', data):
                host.collect_versions(paths[0], paths[1])
            self.assertTrue(all(path.exists() for path in paths[:3]))
            self.assertFalse(paths[3].exists())

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


if __name__ == '__main__':
    unittest.main()
