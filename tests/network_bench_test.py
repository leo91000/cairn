"""Public CLI contract: packets cross the actual simulated network (no KVM)."""
import json
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path


class NetworkBenchTest(unittest.TestCase):
    def probe(self, scenario):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "network.json"
            subprocess.run(
                ["python3", "tests/network-bench.py", scenario,
                 "--probe-only", "--output", str(output)], check=True,
            )
            return json.loads(output.read_text())

    def test_cleanup_continues_after_a_namespace_has_disappeared(self):
        spec = importlib.util.spec_from_file_location("network_bench", "tests/network-bench.py")
        bench = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bench)
        network = bench.Network()
        try:
            first = network.namespace("first")
            second = network.namespace("second")
            bench.run("ip", "netns", "del", second)
            network.close()
            self.assertNotIn(first, bench.run("ip", "netns", "list"))
        finally:
            for namespace in network.namespaces:
                subprocess.run(["sudo", "-n", "ip", "netns", "del", namespace], capture_output=True)

    def test_same_lan_delivers_udp_between_participants(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "network.json"
            subprocess.run(
                ["python3", "tests/network-bench.py", "same-lan",
                 "--probe-only", "--output", str(output)], check=True,
            )
            report = json.loads(output.read_text())
            self.assertEqual(report["scenario"], "same-lan")
            self.assertEqual(report["stunResponder"], "beacon-rust")
            self.assertEqual(report["probe"]["received"], 10)
            self.assertEqual(report["probe"]["sent"], 10)
            self.assertFalse(report["probe"]["translated"])
            self.assertEqual(report["peerProbe"]["received"], 10)

    def test_mdns_only_client_has_no_translated_client_candidate_and_an_inbound_filtered_peer(self):
        report = self.probe("mdns-only-client")
        self.assertEqual(report["scenario"], "mdns-only-client")
        self.assertEqual(report["probe"]["received"], 10)
        self.assertTrue(report["installationProbe"]["translated"])
        self.assertFalse(report["probe"]["translated"])
        self.assertEqual(report["stun"]["client"]["address"], "10.102.1.2")
        self.assertEqual(report["stun"]["installation"]["address"], "198.18.102.3")

    def test_nat_translates_only_the_selected_side(self):
        for scenario, client, installation in [
            ("nat-client", True, False), ("nat-installation", False, True),
            ("nat-both", True, True),
        ]:
            with self.subTest(scenario=scenario):
                report = self.probe(scenario)
                self.assertEqual(report["probe"]["received"], 10)
                self.assertEqual(report["probe"]["translated"], client)
                self.assertEqual(report["installationProbe"]["translated"], installation)
                self.assertEqual(report["stun"]["client"]["address"], "198.18.102.2" if client else "10.102.1.2")
                self.assertEqual(report["stun"]["installation"]["address"], "198.18.102.3" if installation else "10.102.2.2")

    def test_udp_blocked_cannot_deliver_a_datagram(self):
        report = self.probe("udp-blocked")
        self.assertEqual(report["probe"]["received"], 0)
        self.assertEqual(report["installationProbe"]["received"], 0)
        self.assertEqual(report["peerProbe"]["received"], 0)

    def test_symmetric_nat_uses_destination_specific_mappings(self):
        report = self.probe("symmetric-nat")
        for probe in [report["probe"], report["installationProbe"]]:
            self.assertEqual(probe["received"], 10)
            self.assertTrue(probe["translated"])
            self.assertEqual(probe["mappings"], 2)

    def test_same_server_stun_preserves_external_client_but_detects_hairpin_gateway(self):
        report = self.probe("same-server")
        self.assertEqual(report["stun"]["client"]["address"], "198.18.102.2")
        self.assertEqual(report["stun"]["installation"]["address"], "10.102.2.1")

    def test_packet_loss_is_deterministic(self):
        for _ in range(2):
            report = self.probe("packet-loss")
            self.assertEqual(report["probe"]["received"], 8)
            self.assertEqual(report["installationProbe"]["received"], 8)


if __name__ == "__main__":
    unittest.main()
