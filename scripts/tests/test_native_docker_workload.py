"""Qualification readiness regressions without Docker or host services."""
from pathlib import Path
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import native_docker_workload as workload


class ComposeReadinessTests(unittest.TestCase):
    def run_probe(self, redis_results):
        elapsed, calls = [0.0], []
        results = iter(redis_results)

        def run(args, **options):
            self.assertGreater(options['timeout'], 0)
            self.assertLessEqual(options['timeout'], min(10, 45 - elapsed[0]))
            service = args[args.index('-T') + 1]
            calls.append((service, options['timeout']))
            if service == 'db':
                return SimpleNamespace(returncode=0, stdout='accepting connections', stderr='')
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

    def test_failed_readiness_keeps_the_original_shared_deadline(self):
        refused = SimpleNamespace(returncode=1, stdout='', stderr='Connection refused')
        with self.assertRaisesRegex(AssertionError, 'redis.*Connection refused'):
            self.run_probe([refused] * 300)

    def test_a_hung_probe_cannot_reset_the_shared_deadline(self):
        expired = subprocess.TimeoutExpired(['synthetic'], 10)
        with self.assertRaisesRegex(AssertionError, 'redis.*timed out'):
            self.run_probe([expired] * 5)


if __name__ == '__main__':
    unittest.main()
