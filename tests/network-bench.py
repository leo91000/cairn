"""Real Linux namespaces: network fixtures for #102, without KVM or live credentials."""
import argparse
import array
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
             "udp-blocked", "symmetric-nat", "symmetric-client", "same-server", "network-change", "packet-loss"]


def run(*args):
    command = ["sudo", "-n", *args] if args[0] in {"ip", "tc", "iptables", "sysctl"} else list(args)
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return result.stdout.strip()


def namespace_command(namespace, *args):
    return ["sudo", "-n", "ip", "netns", "exec", namespace,
            "setpriv", "--reuid", str(os.getuid()), "--regid", str(os.getgid()),
            "--clear-groups", *args]


def wait_ready(path, child, description):
    deadline = time.monotonic() + 5
    while not path.exists():
        if child.poll() is not None or time.monotonic() > deadline:
            raise RuntimeError(description + " did not start")
        time.sleep(0.01)


class Network:
    def __init__(self):
        self.prefix = "l102" + uuid.uuid4().hex[:6]
        self.namespaces = []
        self.links = []
        self.children = []
        self.groups = []

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

    def spawn(self, namespace, *args, privileged=False):
        command = ["sudo", "-n", "ip", "netns", "exec", namespace, *args] if privileged else namespace_command(namespace, *args)
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
        errors = []

        def signal_group(group, selected):
            try:
                os.killpg(group, selected)
            except ProcessLookupError:
                pass
            except OSError:
                errors.append("process group " + str(group))

        def cleanup_command(*command):
            try:
                return subprocess.run(["sudo", "-n", *command], capture_output=True, text=True)
            except OSError:
                errors.append("cleanup command " + command[0])
                return None

        # Always attempt every resource, including partial setup and failed kills.
        for group in self.groups:
            signal_group(group, signal.SIGTERM)
        for child in self.children:
            try:
                if child.poll() is None:
                    child.terminate()
                child.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                try:
                    child.kill()
                    child.wait(timeout=2)
                except (OSError, subprocess.TimeoutExpired):
                    errors.append("child process " + str(child.pid))
        for group in self.groups:
            signal_group(group, signal.SIGKILL)
        for namespace in reversed(self.namespaces):
            pids = cleanup_command("ip", "netns", "pids", namespace)
            if pids:
                for pid in pids.stdout.split():
                    cleanup_command("kill", "-KILL", pid)
            cleanup_command("ip", "netns", "del", namespace)
        for link in reversed(self.links):
            # Moved veth links may already have disappeared with their namespace.
            cleanup_command("ip", "link", "del", link)
        namespaces = cleanup_command("ip", "netns", "list")
        links = cleanup_command("ip", "-j", "link", "show")
        if namespaces is None or namespaces.returncode or links is None or links.returncode:
            errors.append("could not verify resource cleanup")
        else:
            remaining = {line.split()[0] for line in namespaces.stdout.splitlines()}
            errors.extend(name for name in self.namespaces if name in remaining)
            remaining_links = {link["ifname"] for link in json.loads(links.stdout)}
            errors.extend(name for name in self.links if name in remaining_links)
        if errors:
            raise RuntimeError("Network cleanup incomplete: " + ", ".join(errors))

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
            nat = scenario in {"nat-both", "symmetric-nat", "symmetric-client", "same-server"} or scenario == "nat-" + role
            # Private host candidates behind NAT are not internet-routable.
            # Routing them here would create inbound conntrack entries before
            # hole punching and falsely classify the reverse flow as a reply.
            if not nat:
                run("ip", "-n", internet, "route", "add", f"10.102.{index}.0/24", "via", external_ip)
            if nat:
                if scenario == "symmetric-nat" or scenario == "symmetric-client" and role == "client":
                    for port in [49001, 49002]:
                        self.exec(router, "iptables", "-t", "nat", "-A", "POSTROUTING",
                                  "-o", external, "-d", "198.18.102.1", "-p", "udp", "--dport", str(port),
                                  "-j", "SNAT", "--to-source", f"{external_ip}:{port + 2000}")
                    # Every other UDP destination also gets a separate per-flow
                    # allocation, rather than endpoint-independent port reuse.
                    self.exec(router, "iptables", "-t", "nat", "-A", "POSTROUTING",
                              "-o", external, "-p", "udp", "-j", "MASQUERADE", "--random-fully")
                self.exec(router, "iptables", "-t", "nat", "-A", "POSTROUTING",
                          "-o", external, "-j", "MASQUERADE")
                self.exec(router, "iptables", "-A", "INPUT", "-i", external, "-p", "udp",
                          "-m", "conntrack", "--ctstate", "NEW", "-j", "DROP")
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
                wait_ready(ready, child, "Deterministic loss router")
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
        wait_ready(ready, child, "UDP diagnostic listener")

    def probe(self, binary, role, targets=("198.18.102.1:49001", "198.18.102.1:49002")):
        participant = self.participants[role]
        return json.loads(self.exec(participant["namespace"], binary, "udp-probe",
                                   participant["address"] + ":0", *targets))


async def forward(port, target, ready=None):
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
    if ready:
        Path(ready).write_text(str(os.getpid()))
    async with server:
        await server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", choices=SCENARIOS)
    parser.add_argument("--probe-only", action="store_true")
    parser.add_argument("--expect-route", choices=["direct", "relay"], default="relay")
    parser.add_argument("--expect-rust-route", choices=["direct", "relay"])
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.expect_rust_route is None:
        args.expect_rust_route = "relay" if args.scenario in {"udp-blocked", "symmetric-nat", "symmetric-client"} else "direct"
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
            network.stun_address = "198.18.102.1:3478"
            stun_namespace = network.internet
            listen_address = network.stun_address
            if args.scenario == "same-server":
                # Official STUN is a second container on the installation host.
                router = network.participants["installation"]["router"]
                stun_namespace = network.namespace("official-stun")
                local, remote = network.pair(router, stun_namespace, "s")
                run("ip", "-n", router, "link", "set", local, "master", "lan")
                network.address(stun_namespace, remote, "10.102.2.3/24")
                run("ip", "-n", stun_namespace, "route", "add", "default", "via", "10.102.2.1")
                network.exec(router, "iptables", "-t", "nat", "-A", "PREROUTING", "-d", "198.18.102.3", "-p", "udp", "--dport", "3478", "-j", "DNAT", "--to-destination", "10.102.2.3:3478")
                network.exec(router, "iptables", "-t", "nat", "-A", "POSTROUTING", "-o", "lan", "-s", "10.102.2.0/24", "-d", "10.102.2.3", "-p", "udp", "--dport", "3478", "-j", "SNAT", "--to-source", "10.102.2.1")
                network.exec(router, "iptables", "-I", "FORWARD", "1", "-d", "10.102.2.3", "-p", "udp", "--dport", "3478", "-j", "ACCEPT")
                network.stun_address = "198.18.102.3:3478"
                listen_address = "10.102.2.3:3478"
            ready = Path(directory) / "stun-ready"
            stun = network.spawn(stun_namespace, os.sys.executable, str(Path(__file__).resolve()),
                                 "stun-server", listen_address, str(ready))
            wait_ready(ready, stun, "STUN listener before host masquerading")
            report = {"scenario": args.scenario, "probe": network.probe(binary, "client"),
                      "installationProbe": network.probe(binary, "installation")}
            expected_received = 0 if args.scenario == "udp-blocked" else 8 if args.scenario == "packet-loss" else 10
            for role, probe in [("client", report["probe"]), ("installation", report["installationProbe"])]:
                expected_nat = args.scenario in {"nat-both", "symmetric-nat", "symmetric-client", "same-server"} or args.scenario == "nat-" + role
                if probe["received"] != expected_received or probe["translated"] != expected_nat:
                    raise RuntimeError(f"{role}: the observed packets do not match {args.scenario}: {probe}")
                if (args.scenario == "symmetric-nat" or args.scenario == "symmetric-client" and role == "client") and probe["mappings"] != 2:
                    raise RuntimeError(f"{role}: destination-specific NAT mappings were not observed")
            if args.scenario != "udp-blocked":
                report["stun"] = {
                    role: json.loads(network.exec(network.participants[role]["namespace"], os.sys.executable,
                        str(Path(__file__).resolve()), "stun-probe", network.stun_address))
                    for role in ["client", "installation"]
                }
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
        ready = directory / (role + "-proxy")
        child = network.spawn(participant["namespace"], python, script, "forward", "4398", "198.18.103.1", str(ready))
        wait_ready(ready, child, role + " TCP forwarder")
    proxy = subprocess.Popen([python, script, "forward", "4398", "198.18.103.1"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    network.children.append(proxy)

    def wrapper(name, role, executable, preserve=""):
        path = directory / name
        command = namespace_command(network.participants[role]["namespace"], executable)
        if preserve:
            command.insert(2, "--preserve-env=" + preserve)
        path.write_text("#!/bin/sh\nexec " + shlex.join(command) + ' "$@"\n')
        path.chmod(0o700)
        return str(path)

    env["LEO_OFFICIAL_STUN_URL"] = "stun:" + network.stun_address
    if args.scenario == "same-server":
        env["LEO_DIRECT_PUBLIC_IP"] = "198.18.102.3"
    # The real official Binding responder is independently tested on UDP. This
    # address-discovery fixture belongs before the host-facing masquerade.
    env["LEO_NETWORK_DIRECT_CLIENT"] = wrapper(
        "direct-client", "client", str(Path("target/debug/examples/network_direct_client").resolve()), "")
    env["LEO_NETWORK_INSTALLATION_BINARY"] = wrapper(
        "installation", "installation", str(Path("target/debug/leo").resolve()),
        "DATA_DIR,AGENT_HOME,WORKSPACE_ROOTS,NODE_ENV,WORKER_ENABLED,HOST,PORT,LEO_OFFICIAL_ORIGIN,LEO_INSTALLATION_CLAIM_CODE,LEO_INSTALLATION_NAME,LEO_DIRECT_ENABLED,LEO_DIRECT_STUN_URLS,LEO_DIRECT_PUBLIC_IP")
    # sudo closes inherited descriptors, including Playwright's CDP pipes (3/4).
    # Transfer those descriptors over a private Unix socket after entering the
    # namespace; browser traffic still traverses the real network topology.
    browser_wrapper = directory / "chromium"
    browser_command = [python, script, "browser-pipe", network.participants["client"]["namespace"], chromium, str(directory)]
    browser_wrapper.write_text("#!/bin/sh\nexec " + shlex.join(browser_command) + ' "$@"\n')
    browser_wrapper.chmod(0o700)
    env["LEO_NETWORK_CHROMIUM"] = str(browser_wrapper)
    env["LEO_NETWORK_RUST_CLIENT"] = wrapper("rust-client", "client", binary)
    env["LEO_NETWORK_LOCAL_CLIENT"] = wrapper("local-client", "installation", binary)
    env["LEO_NETWORK_SCENARIO"] = args.scenario
    env["LEO_NETWORK_EXPECT_ROUTE"] = args.expect_route
    env["LEO_NETWORK_EXPECT_RUST_ROUTE"] = args.expect_rust_route
    env["LEO_NETWORK_OUTPUT"] = str(args.output.resolve())
    env["LEO_NETWORK_FIXTURE_DIRECTORY"] = str(directory)
    env["LEO_NETWORK_PLAYWRIGHT_OUTPUT"] = str(args.output.parent.resolve() / (args.scenario + "-playwright"))
    participant = network.participants["client"]
    change = directory / "change-network"
    command = [python, script, "change-network", participant["namespace"], participant["link"], str(directory / "client-proxy")]
    change.write_text("#!/bin/sh\nexec " + shlex.join(command) + "\n")
    change.chmod(0o700)
    env["LEO_NETWORK_CHANGE"] = str(change)
    process = subprocess.Popen(["pnpm", "exec", "playwright", "test", "--config", "playwright.network.config.ts"], env=env, start_new_session=True)
    network.children.append(process)
    network.groups.append(process.pid)
    if process.wait():
        raise RuntimeError("Authenticated network scenario failed; see Playwright diagnostics")


def stun_server(address, ready):
    host, port = address.split(":")
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as listener:
        listener.bind((host, int(port)))
        Path(ready).touch()
        while True:
            packet, peer = listener.recvfrom(2048)
            if len(packet) < 20 or packet[:2] != b"\x00\x01" or packet[4:8] != b"\x21\x12\xa4\x42":
                continue
            mapped = struct.pack("!BBHI", 0, 1, peer[1] ^ 0x2112,
                                 int.from_bytes(socket.inet_aton(peer[0]), "big") ^ 0x2112A442)
            attribute = struct.pack("!HH", 0x0020, len(mapped)) + mapped
            response = struct.pack("!HH", 0x0101, len(attribute)) + packet[4:20] + attribute
            listener.sendto(response, peer)


def stun_probe(address):
    host, port = address.split(":")
    transaction = os.urandom(12)
    request = struct.pack("!HHI", 1, 0, 0x2112A442) + transaction
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as client:
        client.settimeout(0.3)
        for _ in range(10):
            client.sendto(request, (host, int(port)))
            try:
                response, source = client.recvfrom(2048)
            except TimeoutError:
                continue
            if source != (host, int(port)) or response[8:20] != transaction:
                continue
            kind, length = struct.unpack("!HH", response[20:24])
            if kind != 0x0020 or length != 8:
                raise RuntimeError("STUN XOR-MAPPED-ADDRESS missing")
            _, family, mapped_port, mapped_ip = struct.unpack("!BBHI", response[24:32])
            if family != 1:
                raise RuntimeError("IPv4 STUN mapping expected")
            print(json.dumps({"address": socket.inet_ntoa((mapped_ip ^ 0x2112A442).to_bytes(4, "big")),
                              "port": mapped_port ^ 0x2112, "localPort": client.getsockname()[1]}))
            return
    raise RuntimeError("STUN mapping unavailable")


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


def browser_pipe(namespace, executable, directory, arguments):
    with tempfile.TemporaryDirectory(dir=directory) as private:
        path = str(Path(private) / "control")
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(path)
            listener.listen(1)
            listener.settimeout(10)
            command = namespace_command(namespace, os.sys.executable, str(Path(__file__).resolve()),
                                        "browser-pipe-child", path, executable, *arguments)
            child = subprocess.Popen(command)
            try:
                with listener.accept()[0] as connection:
                    connection.sendmsg([b"CDP"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [3, 4]))])
                return child.wait()
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()


def browser_pipe_child(path, executable, arguments):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.connect(path)
        _, ancillary, _, _ = connection.recvmsg(3, socket.CMSG_SPACE(2 * array.array("i").itemsize))
        descriptors = array.array("i")
        for level, kind, data in ancillary:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                descriptors.frombytes(data)
        if len(descriptors) != 2:
            raise RuntimeError("Missing Playwright control pipes")
        # Duplicate first so that neither dup2 overwrites the other source or
        # the socket; closing the socket must not close Chromium's input pipe.
        sources = [os.dup(descriptor) for descriptor in descriptors]
    for source, destination in zip(sources, [3, 4]):
        os.dup2(source, destination, inheritable=True)
    for descriptor in [*sources, *descriptors]:
        if descriptor not in {3, 4}:
            os.close(descriptor)
    os.execv(executable, [executable, *arguments])


def change_network(namespace, link, ready_file):
    ready = Path(ready_file)
    # Deleting a primary IPv4 address also removes its secondary addresses.
    # Add the replacement after deleting the old address, then restore routing.
    run("ip", "-n", namespace, "addr", "del", "10.102.1.2/24", "dev", link)
    run("ip", "-n", namespace, "addr", "add", "10.102.1.9/24", "dev", link)
    run("ip", "-n", namespace, "route", "replace", "default", "via", "10.102.1.1")
    # Close actual old TCP sockets without depending on INET_DIAG_DESTROY.
    # This affects our test forwarder only; the browser and session stay alive.
    os.kill(int(ready.read_text()), signal.SIGTERM)
    ready.unlink()
    command = namespace_command(namespace, os.sys.executable, str(Path(__file__).resolve()),
                                "forward", "4398", "198.18.103.1", str(ready))
    child = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    wait_ready(ready, child, "Replacement client TCP forwarder")


if __name__ == "__main__":
    if len(os.sys.argv) == 4 and os.sys.argv[1] == "stun-server":
        stun_server(*os.sys.argv[2:4])
    elif len(os.sys.argv) == 3 and os.sys.argv[1] == "stun-probe":
        stun_probe(os.sys.argv[2])
    elif len(os.sys.argv) in {4, 5} and os.sys.argv[1] == "forward":
        asyncio.run(forward(int(os.sys.argv[2]), os.sys.argv[3], os.sys.argv[4] if len(os.sys.argv) == 5 else None))
    elif len(os.sys.argv) == 4 and os.sys.argv[1] == "loss-router":
        loss_router(os.sys.argv[2], os.sys.argv[3])
    elif len(os.sys.argv) >= 5 and os.sys.argv[1] == "browser-pipe":
        raise SystemExit(browser_pipe(*os.sys.argv[2:5], os.sys.argv[5:]))
    elif len(os.sys.argv) >= 4 and os.sys.argv[1] == "browser-pipe-child":
        browser_pipe_child(*os.sys.argv[2:4], os.sys.argv[4:])
    elif len(os.sys.argv) == 5 and os.sys.argv[1] == "change-network":
        change_network(*os.sys.argv[2:5])
    else:
        main()
