"""Real Linux network namespaces; no VM, no KVM, no production credentials."""
import argparse
import json
import os
import signal
import subprocess
import tempfile
import uuid
from pathlib import Path


def run(*args):
    command = ["sudo", "-n", *args] if args[0] in {"ip", "tc", "iptables", "sysctl"} else list(args)
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()


class Network:
    def __init__(self):
        self.prefix = "l102" + uuid.uuid4().hex[:6]
        self.namespaces = []
        self.links = []
        self.children = []

    def namespace(self, role):
        name = self.prefix + "-" + role
        run("ip", "netns", "add", name)
        self.namespaces.append(name)
        run("ip", "-n", name, "link", "set", "lo", "up")
        return name

    def exec(self, namespace, *args):
        return run("ip", "netns", "exec", namespace, *args)

    def spawn(self, namespace, *args):
        child = subprocess.Popen(["sudo", "-n", "ip", "netns", "exec", namespace, *args])
        self.children.append(child)
        return child

    def close(self):
        for child in self.children:
            child.terminate()
        for child in self.children:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        for namespace in reversed(self.namespaces):
            # Kill descendants too, including a browser surviving an interrupted runner.
            for pid in run("ip", "netns", "pids", namespace).split():
                try:
                    run("sudo", "-n", "kill", "-KILL", pid)
                except ProcessLookupError:
                    pass
            run("ip", "netns", "del", namespace)
        for link in reversed(self.links):
            run("ip", "link", "del", link)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", choices=["same-lan"])
    parser.add_argument("--probe-only", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    run("sudo", "-n", "true")

    network = Network()
    try:
        with tempfile.TemporaryDirectory(prefix="leo-network-") as directory:
            binary = str(Path(directory) / "network-client")
            run("rustc", "--edition=2024", "backend/examples/network_client.rs", "-o", binary)
            client = network.namespace("client")
            installation = network.namespace("installation")
            run("ip", "link", "add", network.prefix + "c", "type", "veth", "peer", "name", network.prefix + "i")
            run("ip", "link", "set", network.prefix + "c", "netns", client)
            run("ip", "link", "set", network.prefix + "i", "netns", installation)
            for namespace, suffix, address in [(client, "c", "10.102.1.2"), (installation, "i", "10.102.1.3")]:
                run("ip", "-n", namespace, "addr", "add", address + "/24", "dev", network.prefix + suffix)
                run("ip", "-n", namespace, "link", "set", network.prefix + suffix, "up")
            for port in [49001, 49002]:
                network.spawn(installation, binary, "udp-server", f"10.102.1.3:{port}")
            # UDP probe retries are bounded; establish listeners before the measured run.
            network.exec(client, binary, "udp-probe", "10.102.1.2:0", "10.102.1.3:49001", "10.102.1.3:49002")
            probe = json.loads(network.exec(client, binary, "udp-probe", "10.102.1.2:0", "10.102.1.3:49001", "10.102.1.3:49002"))
            args.output.write_text(json.dumps({"scenario": args.scenario, "probe": probe}) + "\n")
    finally:
        network.close()


if __name__ == "__main__":
    main()
