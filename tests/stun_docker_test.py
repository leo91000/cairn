"""A published Docker UDP port must preserve an external STUN client's source."""
import importlib.util
import json
import subprocess
import time
import unittest
from pathlib import Path

IMAGE = "python@sha256:2d9aefe2fef018a7eb2c13064c89c71929800fd2e5dccdbf52ea5da5bb8d929a"


class PublishedStunTest(unittest.TestCase):
    def test_published_udp_preserves_external_client_address(self):
        spec = importlib.util.spec_from_file_location("network_bench", "tests/network-bench.py")
        bench = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bench)
        network = bench.Network()
        name = network.prefix + "-stun"
        try:
            client = network.namespace("external")
            host_link, client_link = network.pair(None, client, "d")
            network.address(None, host_link, "198.18.104.1/30")
            network.address(client, client_link, "198.18.104.2/30")
            bench.run("ip", "-n", client, "route", "add", "default", "via", "198.18.104.1")
            bench.run("docker", "run", "--detach", "--rm", "--name", name,
                      "--publish", "0:3478/udp", "--volume",
                      str(Path("tests/network-bench.py").resolve()) + ":/bench.py:ro",
                      IMAGE, "python", "/bench.py", "stun-server", "0.0.0.0:3478", "/tmp/ready")
            deadline = time.monotonic() + 30
            while True:
                try:
                    ready = subprocess.run(["docker", "exec", name, "test", "-f", "/tmp/ready"],
                                           capture_output=True, timeout=2).returncode == 0
                except subprocess.TimeoutExpired:
                    ready = False
                if ready:
                    break
                if time.monotonic() >= deadline:
                    self.fail("Published STUN fixture did not become ready")
                time.sleep(0.1)
            inspect = json.loads(bench.run("docker", "inspect", name))[0]
            port = inspect["NetworkSettings"]["Ports"]["3478/udp"][0]["HostPort"]
            observed = json.loads(network.exec(client, "python3", str(Path("tests/network-bench.py").resolve()),
                                                "stun-probe", "198.18.104.1:" + port))
            self.assertEqual(observed["address"], "198.18.104.2")
            self.assertEqual(observed["port"], observed["localPort"])
            output = Path("test-results/network/docker-stun.json")
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(json.dumps({"publishedUdp": True, "expectedSource": "198.18.104.2",
                                          "observed": observed}) + "\n")
        finally:
            try:
                subprocess.run(["docker", "rm", "--force", name], capture_output=True, check=True)
            finally:
                network.close()


if __name__ == "__main__":
    unittest.main()
