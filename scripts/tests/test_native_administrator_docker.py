"""Execute the actual administrator wrapper with isolated runtime/CLI fixtures."""

import json
from pathlib import Path
import subprocess
import tempfile
import unittest

WRAPPER = Path(__file__).resolve().parents[2] / "deploy/nodes/docker-administrator"


class AdministratorWrapperTests(unittest.TestCase):
    def invoke(self, args):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for directory in ["bin", "tools/bin", "tools/plugins", "entry", "home", "config"]:
                (root / directory).mkdir(parents=True)
            docker = root / "tools/bin/docker"
            docker.write_text(
                """#!/usr/bin/python3
import json, os, sys
from pathlib import Path
with Path(os.environ['FIXTURE_CALLS']).open('a') as output:
    output.write(json.dumps(dict(args=sys.argv[1:], builder=os.environ.get('BUILDX_BUILDER'))) + '\\n')
"""
            )
            docker.chmod(0o755)
            cairn = root / "entry/cairn"
            cairn.write_text(
                """#!/bin/sh
case "$1" in
  native-administrator-docker-ready)
    while ! test -e "$FIXTURE_STARTED"; do sleep 0.01; done
    echo /fixture.service ;;
  *) touch "$FIXTURE_STARTED" ;;
esac
"""
            )
            cairn.chmod(0o755)
            sudo = root / "bin/sudo"
            sudo.write_text('#!/bin/sh\nshift\nexec "$@"\n')
            sudo.chmod(0o755)
            source = (
                WRAPPER.read_text()
                .replace("/opt/cairn-docker", str(root / "tools"))
                .replace("/run/cairn-entrypoint", str(root / "entry"))
                .replace("/home/node", str(root / "home"))
                .replace("/tmp/cairn-docker-supervisor.log", str(root / "supervisor.log"))
            )
            wrapper = root / "docker"
            wrapper.write_text(source)
            config = root / "config"
            uses_config = any("<config>" in word for word in args)
            args = [word.replace("<config>", str(config)) for word in args]
            env = dict(
                PATH=str(root / "bin") + ":/usr/bin:/bin",
                HOME=str(root / "home"),
                FIXTURE_CALLS=str(root / "calls"),
                FIXTURE_STARTED=str(root / "started"),
            )
            result = subprocess.run(
                ["/bin/sh", str(wrapper), *args],
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in (root / "calls").read_text().splitlines()]
            client_config = config if uses_config else root / "home/.docker"
            self.assertEqual(
                (client_config / "cli-plugins/docker-compose").readlink(),
                root / "tools/plugins/docker-compose",
            )
            return calls, (root / "started").exists()

    def test_builds_with_global_options_and_aliases_use_current_attempt_builder(self):
        for args in [
            ["build", "."],
            ["--debug", "build", "."],
            ["--config", "<config>", "build", "."],
            ["--config=<config>", "builder", "build", "."],
            ["image", "build", "."],
        ]:
            with self.subTest(args=args):
                calls, started = self.invoke(args)
                self.assertTrue(started)
                self.assertEqual(calls[-1]["builder"], "cairn-managed")
                self.assertIn("--driver=docker-container", calls[0]["args"])
                self.assertTrue(
                    any(
                        "cgroup-parent=/fixture.service" in word and "restart-policy=no" in word
                        for word in calls[0]["args"]
                    )
                )

    def test_client_version_and_help_do_not_initialize_a_builder(self):
        for args in [
            ["--version"],
            ["--debug", "--help"],
            ["compose", "version"],
            ["buildx", "version"],
        ]:
            with self.subTest(args=args):
                calls, started = self.invoke(args)
                self.assertFalse(started)
                self.assertEqual(len(calls), 1)
                self.assertIsNone(calls[0]["builder"])


if __name__ == "__main__":
    unittest.main()
