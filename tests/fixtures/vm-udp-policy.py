"""Runs inside a real yolo guest; no live installation credentials are used."""
import json
import os
import socket
import struct
import subprocess
import sys
import time

peer = sys.argv[1]
stun_host = sys.argv[2] if len(sys.argv) > 2 else "stun.l.google.com"
stun_port = int(sys.argv[3]) if len(sys.argv) > 3 else 19302
route = json.loads(subprocess.check_output(["ip", "-j", "route", "get", peer]))[0]
guest = route["prefsrc"]
gateway = route["gateway"]


def echo(sock):
    sock.settimeout(2)
    sock.sendto(b"probe", (peer, 2222))
    assert sock.recv(1024) == b"network-ok"


# Keep a replied tuple alive, so exhaustion must not interrupt existing traffic.
established = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
established.bind(("0.0.0.0", 40000))
echo(established)
print("udp.established", flush=True)

# Send Ethernet frames to the gateway to exercise the host firewall even for
# loopback/multicast destinations which the guest would otherwise route locally.
neighbor = json.loads(subprocess.check_output(["ip", "-j", "neigh", "show", gateway]))[0]
destination_mac = bytes.fromhex(neighbor["lladdr"].replace(":", ""))
with open("/sys/class/net/eth0/address", encoding="ascii") as source:
    source_mac = bytes.fromhex(source.read().strip().replace(":", ""))
raw = socket.socket(socket.AF_PACKET, socket.SOCK_RAW)
raw.bind(("eth0", 0))


def send(source, destination, port):
    udp = struct.pack("!HHHH", port, 2222, 13, 0) + b"probe"
    header = struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(udp), 0, 0, 64, 17, 0,
                         socket.inet_aton(source), socket.inet_aton(destination))
    words = struct.unpack("!10H", header)
    checksum = sum(words)
    while checksum >> 16:
        checksum = (checksum & 0xffff) + (checksum >> 16)
    header = header[:10] + struct.pack("!H", (~checksum) & 0xffff) + header[12:]
    raw.send(destination_mac + source_mac + b"\x08\x00" + header + udp)


# Every denied range is observed in the controller's actual drop counters.
private = ["0.1.2.3", "10.23.0.1", "100.64.0.1", "127.0.0.2", "169.254.169.254",
           "172.16.0.1", "192.0.0.1", "192.0.2.1", "192.88.99.1", "192.168.1.1",
           "198.18.0.1", "198.51.100.1", "203.0.113.1", "224.0.0.1"]
for destination in private:
    send(guest, destination, 41000)
send("10.99.0.2", peer, 41001)
send(guest, gateway, 41002)
send(guest, "93.184.216.2", 41003)  # public address of the controller
print("udp.fences.sent", flush=True)

# The peer sends directly to this listening socket through a routed path, with
# no prior outbound UDP tuple. The controller also checks its inbound DROP count.
inbound = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
inbound.bind(("0.0.0.0", 42000))
inbound.settimeout(5)
print("udp.inbound.ready " + guest, flush=True)
try:
    inbound.recv(1024)
    raise AssertionError("Unsolicited inbound UDP reached the guest")
except socket.timeout:
    print("udp.inbound.denied", flush=True)

# Only the isolated peer receives this bounded burst, never a public third party.
start = time.monotonic()
for port in range(43000, 44000):
    send(guest, peer, port)
print("udp.burst.sent " + str(time.monotonic() - start), flush=True)
echo(established)
print("udp.established.after-burst", flush=True)
time.sleep(3)
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as fresh:
    echo(fresh)
print("udp.refill.passed", flush=True)

# Required for qualification: a genuine Internet STUN Binding Success Response,
# matching cookie and transaction, with a mapped-address attribute.
transaction = os.urandom(12)
request = struct.pack("!HHI", 1, 0, 0x2112a442) + transaction
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as stun:
    stun.settimeout(5)
    stun.sendto(request, (stun_host, stun_port))
    data, remote = stun.recvfrom(4096)
message_type, length, cookie = struct.unpack("!HHI", data[:8])
assert message_type == 0x0101 and cookie == 0x2112a442
assert data[8:20] == transaction and len(data) == 20 + length
attributes = []
offset = 20
while offset < len(data):
    kind, size = struct.unpack("!HH", data[offset:offset + 4])
    assert offset + 4 + size <= len(data)
    attributes.append(kind)
    offset += 4 + ((size + 3) // 4) * 4
assert 0x0020 in attributes or 0x0001 in attributes
print("udp.stun.public " + json.dumps({"server": remote, "bytes": len(data), "type": message_type}), flush=True)
print("udp.policy.passed", flush=True)
# Retain the VM until its controller has recorded firewall evidence.
while True:
    time.sleep(1)
