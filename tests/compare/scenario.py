"""What both ground systems are asked to do, and the pieces both sides share."""
import base64
import json
import math
import os
import signal
import socket
import struct
import subprocess
import threading
import time
import zlib

# Sent in this order, GAP seconds apart; then the run lasts SETTLE seconds more.
COMMANDS = [
    ("CdhCore.cmdDisp.CMD_NO_OP", []),
    ("CdhCore.cmdDisp.CMD_NO_OP_STRING", ["hello"]),
    ("CdhCore.cmdDisp.CMD_TEST_CMD_1", [1, 2.5, 3]),
]
GAP = 0.5
START_WORD = bytes.fromhex("deadbeef")
SETTLE = 4.0


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(what, condition, timeout=30.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            value = condition()
            if value:
                return value
        except OSError:
            pass
        time.sleep(0.1)
    raise TimeoutError(f"timed out waiting for {what}")


def start(command, log, **options):
    """Start a process in its own group, so that `stop` also ends its children."""
    return subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True, **options)


def descendants(pid):
    children = subprocess.run(["ps", "-o", "pid=", "--ppid", str(pid)], capture_output=True, text=True).stdout.split()
    return [int(child) for child in children] + [d for child in children for d in descendants(int(child))]


def stop(process):
    """End the process and everything it started; the Python GDS leaves children behind otherwise."""
    for pid in [process.pid, *descendants(process.pid)]:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)


class Proxy:
    """Sits between Ref (a TCP client) and a GDS (a TCP server) and keeps the bytes of both directions."""

    def __init__(self, gds_port):
        self.downlink, self.uplink = bytearray(), bytearray()
        self.gds_port = gds_port
        self.server = socket.create_server(("127.0.0.1", 0))
        self.port = self.server.getsockname()[1]
        threading.Thread(target=self._run, daemon=True).start()

    def _run(self):
        ref, _ = self.server.accept()
        gds = socket.create_connection(("127.0.0.1", self.gds_port))
        threading.Thread(target=self._pump, args=(gds, ref, self.uplink), daemon=True).start()
        self._pump(ref, gds, self.downlink)

    @staticmethod
    def _pump(source, sink, log):
        try:
            while data := source.recv(4096):
                log.extend(data)
                sink.sendall(data)
        except OSError:
            pass


def start_ref(ref, proxy, out):
    return start([ref, "-a", "127.0.0.1", "-p", str(proxy.port)], open(f"{out}/ref.log", "w"), cwd=out)


def packets(stream):
    """The packets of a stream of F Prime frames: start word, length, packet, CRC-32.

    Bytes between frames are skipped: the Python GDS sends a keep-alive text there.
    """
    found, at = [], 0
    while (at := stream.find(START_WORD, at)) >= 0 and at + 12 <= len(stream):
        length = struct.unpack_from(">I", stream, at + 4)[0]
        end = at + 8 + length
        assert zlib.crc32(stream[at:end]) == struct.unpack_from(">I", stream, end)[0], f"bad CRC at byte {at}"
        found.append(bytes(stream[at + 8:end]))
        at = end + 4
    return found


def plain(value):
    """A value in the common form: integers as text (64-bit safe), non-finite floats by name."""
    if isinstance(value, bool) or value is None or isinstance(value, str):
        return value
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if math.isnan(value):
            return "NaN"
        if math.isinf(value):
            return "Infinity" if value > 0 else "-Infinity"
        return value
    if isinstance(value, dict):
        return {key: plain(item) for key, item in value.items()}
    return [plain(item) for item in value]


def save(path, events, channels, commands):
    with open(path, "w") as file:
        json.dump({"events": events, "channels": channels, "commands": commands}, file, indent=1)


class Stream:
    """`WS /api/v1/stream`, read with the standard library: text messages from the server only."""

    def __init__(self, port, query):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=15)
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall(
            f"GET /api/v1/stream?{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n".encode()
        )
        self.file = self.socket.makefile("rb")
        status = self.file.readline()
        assert b"101" in status, status
        while self.file.readline() != b"\r\n":
            pass

    def next(self):
        while True:
            first, second = self.file.read(2)
            length = second & 0x7F
            if length == 126:
                length = struct.unpack(">H", self.file.read(2))[0]
            elif length == 127:
                length = struct.unpack(">Q", self.file.read(8))[0]
            payload = self.file.read(length)
            if first & 0x0F == 1:
                message = json.loads(payload)
                if message["type"] == "live":
                    self.live = message["seq"]
                    continue
                assert message["type"] == "record", message
                return message["record"]

    def until(self, wanted):
        """Records up to and including the first one that `wanted` accepts."""
        found = []
        while not found or not wanted(found[-1]):
            found.append(self.next())
        return found
