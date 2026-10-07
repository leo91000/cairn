"""Exercise the Rust diagnostic client's UDP interface with delayed packets."""
import json
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path


class NetworkProbeTest(unittest.TestCase):
    def test_stale_response_does_not_hide_current_response(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as server:
            binary = Path(directory) / "probe"
            subprocess.run(["rustc", "--edition=2024", "backend/examples/network_client.rs", "-o", str(binary)], check=True)
            server.bind(("127.0.0.1", 0))
            server.settimeout(5)
            address = f"127.0.0.1:{server.getsockname()[1]}"

            def replies():
                previous = None
                for sequence in range(10):
                    message, peer = server.recvfrom(512)
                    if sequence == 0:
                        previous = message
                        continue
                    if sequence == 1:
                        server.sendto(f"{peer[0]}:{peer[1]} ".encode() + previous, peer)
                        time.sleep(0.01)
                    server.sendto(f"{peer[0]}:{peer[1]} ".encode() + message, peer)

            worker = threading.Thread(target=replies)
            worker.start()
            try:
                result = subprocess.run([str(binary), "udp-probe", "127.0.0.1:0", address, address], check=True, capture_output=True, text=True)
                self.assertEqual(json.loads(result.stdout)["received"], 9)
            finally:
                worker.join(timeout=5)


if __name__ == "__main__":
    unittest.main()
