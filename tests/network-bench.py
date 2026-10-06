"""Real Linux namespaces: network fixtures for #102, without KVM or live credentials."""
import argparse
import asyncio
import fcntl
import socket
import struct
import json
import os
import shlex
import signal
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

SCENARIOS = ["same-lan", "nat-client", "nat-installation", "nat-both",
             "udp-blocked", "symmetric-nat", "network-change", "packet-loss"]


def run(*args):
    command = ["sudo", "-n", *args] if args[0] in {"ip", "tc", "iptables", "sysctl"} else list(args)
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return result.stdout.strip()


class Network:
    def __init__(self):
        self.prefix = "l102" + uuid.uuid4().hex[:6]
        self.namespaces = []
        self.links = []
        self.children = []
        self.uid = os.getuid()
        self.gid = os.getgid()

    def namespace(self, role):
        name = self.prefix + "-" + role
        run("ip", "netns", "add", name)
        self.namespaces.append(name)
        run("ip", "-n", name, "link", "set", "lo", "up")
        # Policy routing through the loss device must not be rejected by the
        # host's inherited reverse-path policy. This affects only test namespaces.
        self.exec(name, "sysctl", "-qw", "net.ipv4.conf.all.rp_filter=0", "net.ipv4.conf.default.rp_filter=0")
        return name

    def exec(self, namespace, *args):
        return run("ip", "netns", "exec", namespace, *args)

    def command(self, namespace, *args):
        return ["sudo", "-n", "ip", "netns", "exec", namespace,
                "setpriv", "--reuid", str(self.uid), "--regid", str(self.gid),
                "--clear-groups", *args]

    def spawn(self, namespace, *args, privileged=False):
        command = ["sudo", "-n", "ip", "netns", "exec", namespace, *args] if privileged else self.command(namespace, *args)
        child = subprocess.Popen(command,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.children.append(child)
        return child

    def pair(self, left, right, name):
        first, second = self.prefix + name + "a", self.prefix + name + "b"
        run("ip", "link", "add", first, "type", "veth", "peer", "name", second)
        # Track the root end immediately, including partial setup failures.
        self.links.append(first)
        for namespace, link in [(left, first), (right, second)]:
            if namespace:
                run("ip", "link", "set", link, "netns", namespace)
                run("ip", "-n", namespace, "link", "set", link, "up")
            else:
                run("ip", "link", "set", link, "up")
        return first, second

    def address(self, namespace, link, address):
        if namespace:
            run("ip", "-n", namespace, "addr", "add", address, "dev", link)
        else:
            run("ip", "addr", "add", address, "dev", link)

    def bridge(self, namespace, name):
        run("ip", "-n", namespace, "link", "add", name, "type", "bridge")
        run("ip", "-n", namespace, "link", "set", name, "up")

    def close(self):
        for child in self.children:
            if child.poll() is None:
                child.terminate()
        for child in self.children:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        for namespace in reversed(self.namespaces):
            for pid in run("ip", "netns", "pids", namespace).split():
                subprocess.run(["sudo", "-n", "kill", "-KILL", pid],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            run("ip", "netns", "del", namespace)
        for link in reversed(self.links):
            # A veth moved into a deleted namespace has already disappeared.
            subprocess.run(["sudo", "-n", "ip", "link", "del", link],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def setup(self, scenario):
        internet = self.namespace("internet")
        self.internet = internet
        self.bridge(internet, "bus")
        self.address(internet, "bus", "198.18.102.1/24")
        host, uplink = self.pair(None, internet, "h")
        self.address(None, host, "198.18.103.1/30")
        self.address(internet, uplink, "198.18.103.2/30")
        self.exec(internet, "sysctl", "-qw", "net.ipv4.ip_forward=1")
        # Only the bench namespace performs NAT; never change host firewall rules.
        self.exec(internet, "iptables", "-t", "nat", "-A", "POSTROUTING",
                  "-o", uplink, "-j", "MASQUERADE")
        self.participants = {}
        for index, role in enumerate(["client", "installation"], 1):
            participant = self.namespace(role)
            router = self.namespace("router" + str(index))
            external, bus = self.pair(router, internet, "e" + str(index))
            run("ip", "-n", internet, "link", "set", bus, "master", "bus")
            external_ip = "198.18.102." + str(index + 1)
            self.address(router, external, external_ip + "/24")
            self.exec(router, "sysctl", "-qw", "net.ipv4.ip_forward=1")
            run("ip", "-n", router, "route", "add", "default", "via", "198.18.102.1")
            self.bridge(router, "lan")
            self.address(router, "lan", f"10.102.{index}.1/24")
            inside, endpoint = self.pair(router, participant, "p" + str(index))
            run("ip", "-n", router, "link", "set", inside, "master", "lan")
            address = f"10.102.{index}.2"
            self.address(participant, endpoint, address + "/24")
            run("ip", "-n", participant, "route", "add", "default", "via", f"10.102.{index}.1")
            run("ip", "-n", internet, "route", "add", f"10.102.{index}.0/24", "via", external_ip)
            nat = scenario in {"nat-both", "symmetric-nat"} or scenario == "nat-" + role
            if nat:
                if scenario == "symmetric-nat":
                    for port in [49001, 49002]:
                        self.exec(router, "iptables", "-t", "nat", "-A", "POSTROUTING",
                                  "-o", external, "-p", "udp", "--dport", str(port),
                                  "-j", "SNAT", "--to-source", f"{external_ip}:{port + 2000}")
                self.exec(router, "iptables", "-t", "nat", "-A", "POSTROUTING",
                          "-o", external, "-j", "MASQUERADE")
                self.exec(router, "iptables", "-A", "FORWARD", "-i", external,
                          "-m", "conntrack", "--ctstate", "NEW", "-j", "DROP")
            if scenario == "udp-blocked":
                self.exec(router, "iptables", "-A", "FORWARD", "-p", "udp", "-j", "DROP")
            if scenario == "packet-loss":
                # A TUN router gives deterministic loss even on kernels without
                # sch_netem/xt_statistic/nft_numgen. It forwards IP packets only.
                self.exec(router, "ip", "tuntap", "add", "dev", "loss", "mode", "tun")
                self.exec(router, "ip", "link", "set", "loss", "up")
                ready = self.directory / ("loss-ready-" + role)
                child = self.spawn(router, os.sys.executable, str(Path(__file__).resolve()),
                                   "loss-router", external, str(ready), privileged=True)
                deadline = time.monotonic() + 5
                while not ready.exists():
                    if child.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError("Deterministic loss router did not start")
                    time.sleep(0.01)
                self.exec(router, "ip", "route", "add", "table", "102", "default", "dev", "loss")
                self.exec(router, "ip", "rule", "add", "iif", "lan", "lookup", "102")
            self.participants[role] = {"namespace": participant, "router": router,
                                       "link": endpoint, "address": address}
        if scenario == "same-lan":
            # Move the installation's LAN end onto the client's LAN bridge.
            installation = self.participants["installation"]
            client = self.participants["client"]
            previous = self.prefix + "p2a"
            run("ip", "-n", installation["router"], "link", "set", previous, "nomaster")
            run("ip", "-n", installation["router"], "link", "set", previous, "netns", client["router"])
            run("ip", "-n", client["router"], "link", "set", previous, "master", "lan")
            run("ip", "-n", client["router"], "link", "set", previous, "up")
            run("ip", "-n", installation["namespace"], "addr", "flush", "dev", installation["link"])
            installation["address"] = "10.102.1.3"
            self.address(installation["namespace"], installation["link"], "10.102.1.3/24")
            run("ip", "-n", installation["namespace"], "route", "replace", "default", "via", "10.102.1.1")
        return self

    def listen(self, binary, namespace, address, port):
        ready = self.directory / f"udp-ready-{port}"
        child = self.spawn(namespace, binary, "udp-server", f"{address}:{port}", str(ready))
        deadline = time.monotonic() + 5
        while not ready.exists():
            if child.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError("UDP diagnostic listener did not start")
            time.sleep(0.01)

    def probe(self, binary, role, targets=("198.18.102.1:49001", "198.18.102.1:49002")):
        participant = self.participants[role]
        return json.loads(self.exec(participant["namespace"], binary, "udp-probe",
                                   participant["address"] + ":0", *targets))


async def forward(port, target):
    async def connection(reader, writer):
        remote_writer = None
        try:
            remote_reader, remote_writer = await asyncio.open_connection(target, port)

            async def copy(source, destination):
                while data := await source.read(65536):
                    destination.write(data)
                    await destination.drain()
            tasks = [asyncio.create_task(copy(reader, remote_writer)),
                     asyncio.create_task(copy(remote_reader, writer))]
            _, pending = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            for task in pending:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
        except (ConnectionError, OSError):
            pass
        finally:
            writer.close()
            if remote_writer:
                remote_writer.close()
    server = await asyncio.start_server(connection, "127.0.0.1", port)
    async with server:
        await server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", choices=SCENARIOS)
    parser.add_argument("--probe-only", action="store_true")
    parser.add_argument("--expect-route", choices=["direct", "relay"], default="relay")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    def interrupted(_signal, _frame):
        raise SystemExit("Network bench interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    run("sudo", "-n", "true")
    network = Network()
    # Private /24s are fixed for reproducibility. Fail rather than overlap a second bench.
    lock = open("/tmp/leo-network-bench.lock", "a")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        parser.error("Another network bench is active; run scenarios sequentially")
    if "198.18.103.0/30" in run("ip", "route", "show"):
        parser.error("Another network bench is active; run scenarios sequentially")
    try:
        with tempfile.TemporaryDirectory(prefix="leo-network-") as directory:
            binary = str(Path(directory) / "network-client")
            network.directory = Path(directory)
            run("rustc", "--edition=2024", "backend/examples/network_client.rs", "-o", binary)
            network.setup(args.scenario)
            for port in [49001, 49002]:
                network.listen(binary, network.internet, "198.18.102.1", port)
            report = {"scenario": args.scenario, "probe": network.probe(binary, "client"),
                      "installationProbe": network.probe(binary, "installation")}
            expected_received = 0 if args.scenario == "udp-blocked" else 8 if args.scenario == "packet-loss" else 10
            for role, probe in [("client", report["probe"]), ("installation", report["installationProbe"])]:
                expected_nat = args.scenario in {"nat-both", "symmetric-nat"} or args.scenario == "nat-" + role
                if probe["received"] != expected_received or probe["translated"] != expected_nat:
                    raise RuntimeError(f"{role}: the observed packets do not match {args.scenario}: {probe}")
                if args.scenario == "symmetric-nat" and probe["mappings"] != 2:
                    raise RuntimeError(f"{role}: destination-specific NAT mappings were not observed")
            if args.scenario in {"same-lan", "udp-blocked"}:
                installation = network.participants["installation"]
                for port in [49011, 49012]:
                    network.listen(binary, installation["namespace"], installation["address"], port)
                report["peerProbe"] = network.probe(binary, "client", tuple(f"{installation['address']}:{port}" for port in [49011, 49012]))
                peer_received = 10 if args.scenario == "same-lan" else 0
                if report["peerProbe"]["received"] != peer_received:
                    raise RuntimeError("Peer-to-peer diagnostic packets did not match the scenario")
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report) + "\n")
            if not args.probe_only:
                browser(network, binary, Path(directory), args, report)
    finally:
        network.close()


def browser(network, binary, directory, args, report):
    if not os.environ.get("LEO_OFFICIAL_TEST_DATABASE_URL"):
        raise RuntimeError("Set LEO_OFFICIAL_TEST_DATABASE_URL to a disposable Postgres database")
    python = os.sys.executable
    script = str(Path(__file__).resolve())
    chromium = run("pnpm", "exec", "node", "--input-type=module", "-e",
                   "import {chromium} from '@playwright/test'; console.log(chromium.executablePath())")
    env = os.environ.copy()
    for key in ["LEO_AUTH_SOCKET", "CLAUDE_SECURESTORAGE_CONFIG_DIR", "LEO_MCP_RUN_TOKEN",
                "CODEX_HOME", "OPENAI_API_KEY", "CODEX_API_KEY", "CLAUDE_CONFIG_DIR",
                "CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]:
        env.pop(key, None)
    for role in ["client", "installation"]:
        participant = network.participants[role]
        network.spawn(participant["namespace"], python, script, "forward", "4398", "198.18.103.1")
    proxy = subprocess.Popen([python, script, "forward", "4398", "198.18.103.1"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    network.children.append(proxy)

    def wrapper(name, role, executable, preserve=""):
        path = directory / name
        command = network.command(network.participants[role]["namespace"], executable)
        if preserve:
            command.insert(2, "--preserve-env=" + preserve)
        path.write_text("#!/bin/sh\nexec " + shlex.join(command) + ' "$@"\n')
        path.chmod(0o700)
        return str(path)

    env["LEO_NETWORK_INSTALLATION_BINARY"] = wrapper(
        "installation", "installation", str(Path("target/debug/leo").resolve()),
        "DATA_DIR,AGENT_HOME,WORKSPACE_ROOTS,NODE_ENV,WORKER_ENABLED,HOST,PORT,LEO_OFFICIAL_ORIGIN,LEO_INSTALLATION_CLAIM_CODE,LEO_INSTALLATION_NAME")
    env["LEO_NETWORK_CHROMIUM"] = wrapper("chromium", "client", chromium)
    env["LEO_NETWORK_RUST_CLIENT"] = wrapper("rust-client", "client", binary)
    env["LEO_NETWORK_LOCAL_CLIENT"] = wrapper("local-client", "installation", binary)
    env["LEO_NETWORK_SCENARIO"] = args.scenario
    env["LEO_NETWORK_EXPECT_ROUTE"] = args.expect_route
    env["LEO_NETWORK_OUTPUT"] = str(args.output.resolve())
    env["LEO_NETWORK_PLAYWRIGHT_OUTPUT"] = str(args.output.parent.resolve() / (args.scenario + "-playwright"))
    participant = network.participants["client"]
    change = directory / "change-network"
    commands = [
        ["sudo", "-n", "ip", "-n", participant["namespace"], "addr", "add", "10.102.1.9/24", "dev", participant["link"]],
        ["sudo", "-n", "ip", "-n", participant["namespace"], "addr", "del", "10.102.1.2/24", "dev", participant["link"]],
        # Real sockets on the old interface are cut, as on a mobile network switch.
        ["sudo", "-n", "ip", "netns", "exec", participant["namespace"], "ss", "-K", "src", "10.102.1.2"],
    ]
    change.write_text("#!/bin/sh\nset -eu\n" + "\n".join(shlex.join(command) for command in commands) + "\n")
    change.chmod(0o700)
    env["LEO_NETWORK_CHANGE"] = str(change)
    result = subprocess.run(["pnpm", "exec", "playwright", "test", "--config", "playwright.network.config.ts"], env=env)
    if result.returncode:
        raise RuntimeError("Authenticated network scenario failed; see Playwright diagnostics")


def loss_router(external, ready):
    # Read the TUN's routed IPv4 packets; inject surviving packets onto the WAN.
    # No parsing, logging, rewriting or persistence of application content.
    with open("/dev/net/tun", "r+b", buffering=0) as interface:
        fcntl.ioctl(interface, 0x400454ca, struct.pack("16sH", b"loss", 0x0001 | 0x1000))
        with socket.socket(socket.AF_INET, socket.SOCK_RAW, socket.IPPROTO_RAW) as outgoing:
            outgoing.setsockopt(socket.IPPROTO_IP, socket.IP_HDRINCL, 1)
            outgoing.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, external.encode() + b"\0")
            Path(ready).touch()
            count = 0
            while packet := interface.read(65536):
                # TUN link-up also emits IPv6 neighbor discovery; the bench's
                # controlled topology is IPv4, and these are not test packets.
                if packet[0] >> 4 != 4:
                    continue
                if count % 5:
                    outgoing.sendto(packet, (socket.inet_ntoa(packet[16:20]), 0))
                count += 1


if __name__ == "__main__":
    if len(os.sys.argv) == 4 and os.sys.argv[1] == "forward":
        asyncio.run(forward(int(os.sys.argv[2]), os.sys.argv[3]))
    elif len(os.sys.argv) == 4 and os.sys.argv[1] == "loss-router":
        loss_router(os.sys.argv[2], os.sys.argv[3])
    else:
        main()
