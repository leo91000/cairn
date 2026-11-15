"""Qualification readiness regressions without Docker or host services."""
from pathlib import Path
import itertools
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import native_docker_workload as workload
import native_runtime_test as runtime
import native_administrator_network as network


class ComposeReadinessTests(unittest.TestCase):
    def run_probe(self, redis_results, postgres_results=None):
        elapsed, calls = [0.0], []
        results = iter(redis_results)
        postgres = iter(postgres_results) if postgres_results is not None else itertools.repeat(
            SimpleNamespace(returncode=0, stdout='1\n', stderr=''))

        def run(args, **options):
            self.assertGreater(options['timeout'], 0)
            self.assertLessEqual(options['timeout'], min(10, 45 - elapsed[0]))
            service = args[args.index('-T') + 1]
            calls.append((service, options['timeout']))
            if service == 'db':
                # A Unix-only initialization server must never qualify as an
                # application server, even when pg_isready would report success.
                self.assertIn('-h 127.0.0.1', args[-1])
                self.assertIn('SELECT 1', args[-1])
                result = next(postgres)
            else:
                result = next(results)
            if isinstance(result, Exception):
                elapsed[0] += options['timeout']
                raise result
            return result

        def sleep(seconds):
            elapsed[0] += seconds
            self.assertLessEqual(elapsed[0], 45)

        with patch.object(workload.subprocess, 'run', side_effect=run), \
                patch.object(workload.time, 'monotonic', side_effect=lambda: elapsed[0]), \
                patch.object(workload.time, 'sleep', side_effect=sleep):
            workload.wait_compose_services(Path('compose.json'))
        return elapsed[0], calls

    def test_postgres_ready_before_redis_does_not_finish_the_probe(self):
        def result(code, text):
            return SimpleNamespace(returncode=code, stdout=text, stderr='')
        elapsed, calls = self.run_probe([
            result(1, 'Connection refused'), result(0, 'LOADING'), result(0, 'PONG\n'),
        ])
        self.assertEqual([service for service, _ in calls], ['db', 'redis', 'redis', 'redis'])
        self.assertGreater(elapsed, 0)

    def test_initialization_socket_does_not_satisfy_application_readiness(self):
        pong = SimpleNamespace(returncode=0, stdout='PONG', stderr='')
        refused = SimpleNamespace(returncode=1, stdout='', stderr='TCP connection refused')
        ready = SimpleNamespace(returncode=0, stdout='1\n', stderr='')
        _, calls = self.run_probe([pong], postgres_results=[refused, ready])
        self.assertEqual([service for service, _ in calls], ['db', 'redis', 'db'])

    def test_failed_readiness_keeps_the_original_shared_deadline(self):
        refused = SimpleNamespace(returncode=1, stdout='', stderr='Connection refused')
        with self.assertRaisesRegex(AssertionError, 'redis.*Connection refused'):
            self.run_probe([refused] * 300)

    def test_a_hung_probe_cannot_reset_the_shared_deadline(self):
        expired = subprocess.TimeoutExpired(['synthetic'], 10)
        with self.assertRaisesRegex(AssertionError, 'redis.*timed out'):
            self.run_probe([expired] * 5)


class AdministratorNetworkQualificationTests(unittest.TestCase):
    def test_retained_bind_rejects_a_replaced_directory_after_mount_success(self):
        import os
        import tempfile
        with tempfile.TemporaryDirectory(prefix='native-bind-') as temporary:
            original, replacement = Path(temporary) / 'original', Path(temporary) / 'replacement'
            original.mkdir()
            replacement.mkdir()
            descriptor = os.open(original, os.O_RDONLY | os.O_DIRECTORY)
            try:
                with patch.object(network.subprocess, 'run'):
                    with self.assertRaisesRegex(AssertionError, 'Retained directory was not restored'):
                        network.bind_directory(descriptor, str(replacement))
            finally:
                os.close(descriptor)

    def test_owned_systemd_unit_has_explicit_sbin_tool_lookup(self):
        arguments = network.unit_command('fixture.service', 'attempt', '/candidate/cairn',
                                         '/candidate/rootfs.tar', '/candidate/go', '/candidate/tools')
        environments = [argument.removeprefix('--setenv=') for argument in arguments
                        if argument.startswith('--setenv=')]
        environment = dict(value.split('=', 1) for value in environments)
        self.assertEqual(environment['PATH'].split(':'), ['/usr/sbin', '/usr/bin', '/sbin', '/bin'])
        # Ubuntu keeps iptables-save in /usr/sbin, whereas systemd's compiled
        # default can omit it. A real subprocess lookup models that split.
        import os
        import tempfile
        with tempfile.TemporaryDirectory(prefix='native-tool-path-') as root:
            root = Path(root)
            for directory in ['usr/sbin', 'usr/bin', 'sbin', 'bin']:
                (root / directory).mkdir(parents=True)
            tool = root / 'usr/sbin/iptables-save'
            tool.write_text('#!/bin/sh\nprintf fixture-tool\n')
            tool.chmod(0o755)
            with self.assertRaises(FileNotFoundError):
                subprocess.run(['iptables-save'], env={'PATH': str(root / 'usr/bin')}, check=True)
            path = ':'.join(str(root / directory.lstrip('/')) for directory in environment['PATH'].split(':'))
            result = subprocess.run(['iptables-save'], env={'PATH': path}, check=True, capture_output=True, text=True)
            self.assertEqual(result.stdout, 'fixture-tool')

    def test_failed_qualifier_preserves_bounded_synthetic_diagnostics(self):
        result = SimpleNamespace(returncode=1, stdout='fixture phase reached\n', stderr='fortified compiler failure\n')
        with patch.object(runtime.subprocess, 'run', return_value=result) as run:
            with self.assertRaisesRegex(AssertionError, 'fortified compiler failure') as error:
                runtime.qualify_administrator_network('/candidate/cairn', '/candidate/docker-tools')
        self.assertIn('fixture phase reached', str(error.exception))
        self.assertEqual(run.call_args.kwargs['timeout'], 150)

    def test_success_requires_the_completed_probe_receipt(self):
        for output, succeeds in [('', False), ('READY cgroup\n', False), ('PASS cgroup ancestry, TCP/UDP publication\n', True)]:
            result = SimpleNamespace(returncode=0, stdout=output, stderr='')
            with self.subTest(output=output), patch.object(runtime.subprocess, 'run', return_value=result):
                if succeeds:
                    runtime.qualify_administrator_network('/candidate/cairn', '/candidate/docker-tools')
                else:
                    with self.assertRaisesRegex(AssertionError, 'completion receipt=False'):
                        runtime.qualify_administrator_network('/candidate/cairn', '/candidate/docker-tools')

    def test_timeout_and_large_failure_keep_only_the_bounded_tail(self):
        error = subprocess.TimeoutExpired(['fixture'], 150, output=b'early output\n', stderr=b'network still pending\n')
        with patch.object(runtime.subprocess, 'run', side_effect=error):
            with self.assertRaisesRegex(AssertionError, 'network still pending'):
                runtime.qualify_administrator_network('/candidate/cairn', '/candidate/docker-tools')
        result = SimpleNamespace(returncode=1, stdout='old output' + 'x' * 20000, stderr='last diagnostic')
        with patch.object(runtime.subprocess, 'run', return_value=result):
            with self.assertRaises(AssertionError) as captured:
                runtime.qualify_administrator_network('/candidate/cairn', '/candidate/docker-tools')
        self.assertLess(len(str(captured.exception)), 17000)
        self.assertNotIn('old output', str(captured.exception))
        self.assertIn('last diagnostic', str(captured.exception))


if __name__ == '__main__':
    unittest.main()
