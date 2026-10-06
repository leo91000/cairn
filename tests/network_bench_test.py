"""Public CLI contract: packets cross the actual simulated network (no KVM)."""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


class NetworkBenchTest(unittest.TestCase):
    def test_same_lan_delivers_udp_between_participants(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "network.json"
            subprocess.run(
                ["python3", "tests/network-bench.py", "same-lan",
                 "--probe-only", "--output", str(output)], check=True,
            )
            report = json.loads(output.read_text())
            self.assertEqual(report["scenario"], "same-lan")
            self.assertEqual(report["probe"]["received"], 10)
            self.assertEqual(report["probe"]["sent"], 10)
            self.assertFalse(report["probe"]["translated"])


if __name__ == "__main__":
    unittest.main()
