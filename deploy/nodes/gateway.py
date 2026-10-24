#!/usr/bin/env python3
"""Loopback origin for an execution node that reaches its manager through Beacon.

Each local connection becomes one WebSocket to Beacon, which pairs it with a
connection opened by the installation. Bytes are copied unchanged: the node and
manager keep authenticating every request with their own codes and tokens. The
gateway holds no credential, so reaching it grants nothing beyond what Beacon
already accepts from anyone.
"""
import asyncio
import base64
import hashlib
import json
import os
import re
import signal
import ssl
import struct
import sys
import tempfile
import urllib.parse
from pathlib import Path

ROOT = Path(os.environ.get('CAIRN_GATEWAY_ROOT', '/var/lib/cairn-gateway'))
PORT = 4319
CHUNK = 65536
MAX_MESSAGE = 1 << 20
MAX_CONNECTIONS = 128
HANDSHAKE_TIMEOUT = 15
# Beacon pings every 15 seconds; silence beyond this means the path is gone.
SILENCE = 60
WEBSOCKET_GUID = b'258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
INSTALLATION_ID = re.compile('[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}')


class Refused(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


def beacon_origin(value):
    parsed = urllib.parse.urlsplit(value)
    loopback = parsed.hostname in ('localhost', '127.0.0.1', '::1')
    if (not parsed.hostname or parsed.username or parsed.password or parsed.path not in ('', '/')
            or parsed.query or parsed.fragment
            or not (parsed.scheme == 'https' or parsed.scheme == 'http' and loopback)):
        raise ValueError('Use the HTTPS Beacon origin (HTTP only on loopback for tests).')
    return value.rstrip('/')


def settings():
    value = json.loads((ROOT / 'config.json').read_text())
    if not INSTALLATION_ID.fullmatch(value.get('installationId', '')):
        raise ValueError('Invalid installation ID in the gateway configuration.')
    port = value.get('port', PORT)
    if not isinstance(port, int) or not 0 < port < 65536:
        raise ValueError('Invalid gateway port.')
    return beacon_origin(value['beacon']), value['installationId'], port


def configure(beacon, installation):
    beacon = beacon_origin(beacon)
    installation = installation.lower()
    if not INSTALLATION_ID.fullmatch(installation):
        raise ValueError('Copy the complete command from Nodes → Add a machine.')
    path = ROOT / 'config.json'
    if path.exists():
        current = json.loads(path.read_text())
        if current.get('installationId') != installation:
            raise ValueError('This machine already runs nodes for another installation. Uninstall them before adding it to this one.')
        if current.get('beacon') == beacon:
            return False
    ROOT.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(dir=ROOT)
    with os.fdopen(descriptor, 'w') as file:
        json.dump({'beacon': beacon, 'installationId': installation}, file)
    os.replace(temporary, path)
    return True


def mask(payload, key):
    # Integer XOR keeps whole-message masking in C rather than per byte.
    size = len(payload)
    if not size:
        return b''
    repeated = (key * (size // 4 + 1))[:size]
    return (int.from_bytes(payload, 'little') ^ int.from_bytes(repeated, 'little')).to_bytes(size, 'little')


def encode(opcode, payload=b''):
    key = os.urandom(4)
    size = len(payload)
    if size < 126:
        header = struct.pack('!BB', 0x80 | opcode, 0x80 | size)
    elif size < 65536:
        header = struct.pack('!BBH', 0x80 | opcode, 0x80 | 126, size)
    else:
        header = struct.pack('!BBQ', 0x80 | opcode, 0x80 | 127, size)
    return header + key + mask(payload, key)


async def read_frame(reader):
    first, second = await reader.readexactly(2)
    opcode = first & 0x0F
    size = second & 0x7F
    if second & 0x80:
        raise ValueError('Beacon must not mask its frames.')
    if size == 126:
        (size,) = struct.unpack('!H', await reader.readexactly(2))
    elif size == 127:
        (size,) = struct.unpack('!Q', await reader.readexactly(8))
    if size > MAX_MESSAGE:
        raise ValueError('Beacon frame is too large.')
    return opcode, await reader.readexactly(size)


async def open_channel(beacon, installation):
    origin = urllib.parse.urlsplit(beacon)
    secure = origin.scheme == 'https'
    port = origin.port or (443 if secure else 80)
    context = ssl.create_default_context() if secure else None
    reader, writer = await asyncio.wait_for(
        asyncio.open_connection(origin.hostname, port, ssl=context, server_hostname=origin.hostname if secure else None),
        HANDSHAKE_TIMEOUT)
    try:
        key = base64.b64encode(os.urandom(16)).decode()
        writer.write((
            f'GET /api/relay/{installation}/nodes/connect HTTP/1.1\r\n'
            f'Host: {origin.netloc}\r\n'
            'Upgrade: websocket\r\nConnection: Upgrade\r\n'
            f'Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n'
            'User-Agent: cairn-node-gateway\r\n\r\n').encode())
        await writer.drain()
        head = await asyncio.wait_for(reader.readuntil(b'\r\n\r\n'), HANDSHAKE_TIMEOUT)
        lines = head.decode('latin-1').split('\r\n')
        status = int(lines[0].split(' ')[1])
        headers = {}
        for line in lines[1:]:
            name, _, value = line.partition(':')
            headers[name.strip().lower()] = value.strip()
        if status != 101:
            message = 'Beacon refused the node connection.'
            try:
                length = min(int(headers.get('content-length', '0')), 4096)
                body = await asyncio.wait_for(reader.readexactly(length), 5)
                message = json.loads(body).get('error') or message
            except (ValueError, asyncio.IncompleteReadError, asyncio.TimeoutError, AttributeError):
                pass
            raise Refused(status, message)
        expected = base64.b64encode(hashlib.sha1(key.encode() + WEBSOCKET_GUID).digest()).decode()
        if headers.get('sec-websocket-accept') != expected:
            raise Refused(502, 'Beacon answered with an invalid WebSocket handshake.')
    except BaseException:
        writer.close()
        raise
    return reader, writer


def failure(status, message):
    body = json.dumps({'error': message}).encode()
    reason = {429: 'Too Many Requests', 503: 'Service Unavailable'}.get(status, 'Bad Gateway')
    return (f'HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n'
            f'Content-Length: {len(body)}\r\nConnection: close\r\n\r\n').encode() + body


class Channel:
    def __init__(self, reader, writer):
        self.reader = reader
        self.writer = writer
        self.lock = asyncio.Lock()

    async def send(self, opcode, payload=b''):
        async with self.lock:
            self.writer.write(encode(opcode, payload))
            await self.writer.drain()

    async def upload(self, local):
        while True:
            data = await local.read(CHUNK)
            if not data:
                await self.send(0x8)
                return
            await self.send(0x2, data)

    async def download(self, local):
        while True:
            opcode, payload = await asyncio.wait_for(read_frame(self.reader), SILENCE)
            # The channel is a byte stream: message boundaries carry no meaning.
            if opcode in (0x0, 0x2):
                local.write(payload)
                await local.drain()
            elif opcode == 0x9:
                await self.send(0xA, payload)
            elif opcode == 0x8:
                return
            elif opcode != 0xA:
                raise ValueError('Unexpected Beacon frame.')


async def serve(local_reader, local_writer, beacon, installation, slots):
    try:
        if slots.locked():
            local_writer.write(failure(503, 'Too many local node connections.'))
            return
        async with slots:
            try:
                remote_reader, remote_writer = await open_channel(beacon, installation)
            except Refused as error:
                local_writer.write(failure(error.status if error.status in (429, 503) else 502, str(error)))
                return
            except (OSError, asyncio.TimeoutError, ValueError, IndexError, asyncio.IncompleteReadError, asyncio.LimitOverrunError):
                local_writer.write(failure(502, 'Cannot reach Beacon. Check outbound HTTPS connectivity.'))
                return
            channel = Channel(remote_reader, remote_writer)
            tasks = [asyncio.create_task(channel.upload(local_reader)),
                     asyncio.create_task(channel.download(local_writer))]
            try:
                await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
                remote_writer.close()
    except (OSError, asyncio.CancelledError):
        pass
    finally:
        try:
            await local_writer.drain()
        except OSError:
            pass
        local_writer.close()


async def run():
    beacon, installation, port = settings()
    slots = asyncio.Semaphore(MAX_CONNECTIONS)

    def handler(reader, writer):
        return serve(reader, writer, beacon, installation, slots)

    tcp = await asyncio.start_server(handler, '127.0.0.1', port)
    # Firecracker nodes run in a container with their own loopback; they reach
    # the gateway through this socket, mounted at the same path.
    directory = ROOT / 'socket'
    directory.mkdir(mode=0o700, exist_ok=True)
    path = directory / 'gateway.sock'
    path.unlink(missing_ok=True)
    unix = await asyncio.start_unix_server(handler, str(path))
    os.chmod(path, 0o600)
    stop = asyncio.Event()
    for number in (signal.SIGTERM, signal.SIGINT):
        asyncio.get_running_loop().add_signal_handler(number, stop.set)
    print(f'Cairn node gateway listening on http://127.0.0.1:{port}', flush=True)
    await stop.wait()
    tcp.close()
    unix.close()
    path.unlink(missing_ok=True)


def main():
    try:
        if sys.argv[1:2] == ['configure'] and len(sys.argv) == 4:
            changed = configure(sys.argv[2], sys.argv[3])
            print('changed' if changed else 'unchanged')
        elif sys.argv[1:] == ['run']:
            asyncio.run(run())
        else:
            raise ValueError('Usage: gateway.py configure BEACON INSTALLATION | run')
    except (OSError, ValueError, KeyError) as error:
        print(f'Cairn node gateway: {error}', file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    main()
