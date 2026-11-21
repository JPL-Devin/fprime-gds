"""What both ground systems are asked to do, and the pieces both sides share."""
import atexit
import binascii
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
# Arguments are JSON values, as fgds takes them; `text` gives the form the Python GDS takes.
COMMANDS = [
    ("CdhCore.cmdDisp.CMD_NO_OP", []),
    ("CdhCore.cmdDisp.CMD_NO_OP_STRING", ["hello"]),
    ("CdhCore.cmdDisp.CMD_TEST_CMD_1", [1, 2.5, 3]),
    ("Ref.typeDemo.CHOICE", ["RED"]),
    ("Ref.typeDemo.CHOICES", [["ONE", "BLUE"]]),
    ("Ref.typeDemo.EXTRA_CHOICES", [[["ONE", "TWO"], ["RED", "BLUE"]]]),
    ("Ref.typeDemo.CHOICE_PAIR", [{"firstChoice": "RED", "secondChoice": "TWO"}]),
    ("Ref.typeDemo.GLUTTON_OF_CHOICE", [{
        "tooManyChoices": [["BLUE", "RED"], ["TWO", "ONE"]], "separateChoice": "TWO",
        "choicePair": {"firstChoice": "ONE", "secondChoice": "BLUE"}, "choiceAsMemberArray": [0, 255]}]),
    ("Ref.typeDemo.CHOICES_WITH_FRIENDS", [2, ["TWO", "RED"], 3]),
    ("Ref.typeDemo.SEND_SCALARS", [{
        "i8": -128, "i16": -32768, "i32": -2147483648, "i64": -9223372036854775808,
        "u8": 255, "u16": 65535, "u32": 4294967295, "u64": 18446744073709551615, "f32": 1.5, "f64": -2.25}]),
    ("Ref.typeDemo.DUMP_FLOATS", []),
    ("Ref.SG1.Settings", [5, 2.5, 0.25, "SQUARE"]),
    ("CdhCore.health.HLTH_ENABLE", ["ENABLED"]),
    ("FileHandling.fileManager.RemoveFile", ["/no/such/directory/file", True]),
    # Fails on board: the file does not exist.
    ("FileHandling.fileManager.FileSize", ["/no/such/directory/file"]),
]
# Each must be refused by the GDS: nothing on the link, no command recorded.
REJECTED = [
    ("Ref.typeDemo.CHOICE", ["PURPLE"]),
    ("Ref.typeDemo.NO_SUCH_COMMAND", []),
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


STARTED = []


def start(command, log, **options):
    """Start a process in its own group; `stop` ends it and what it started.

    Whatever is still running when the script ends, however it ends, is stopped then.
    """
    process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True, **options)
    STARTED.append(process)
    return process


def descendants(pid):
    children = subprocess.run(["ps", "-o", "pid=", "--ppid", str(pid)], capture_output=True, text=True).stdout.split()
    return [int(child) for child in children] + [d for child in children for d in descendants(int(child))]


def alive(pid):
    try:
        with open(f"/proc/{pid}/stat") as file:
            return file.read().rsplit(")", 1)[1].split()[0] != "Z"
    except OSError:
        return False


def stop(process, patience=10.0):
    """End the process and everything it started, and wait until all of it is gone.

    The Python GDS starts children (`comm`, the GUI server) that outlive it: the process,
    its group and its descendants are asked to end, then killed if they have not.
    """
    if process in STARTED:
        STARTED.remove(process)
    pids = [process.pid, *descendants(process.pid)]

    def signal_all(number):
        try:
            os.killpg(process.pid, number)
        except (ProcessLookupError, PermissionError):
            pass
        for pid in pids:
            try:
                os.kill(pid, number)
            except ProcessLookupError:
                pass

    def gone():
        process.poll()
        return not any(alive(pid) for pid in pids)

    signal_all(signal.SIGTERM)
    end = time.monotonic() + patience
    while not gone() and time.monotonic() < end:
        time.sleep(0.05)
    if not gone():
        signal_all(signal.SIGKILL)
        while not gone() and time.monotonic() < end + 5:
            time.sleep(0.05)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass


def stop_all():
    for process in list(STARTED):
        stop(process, patience=3.0)


def _ended(number, _frame):
    raise SystemExit(128 + number)


atexit.register(stop_all)
if threading.current_thread() is threading.main_thread():
    for _number in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(_number, _ended)


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


def text(argument):
    """A command argument as the Python GDS takes it: text, JSON text for arrays and structs."""
    if isinstance(argument, bool):
        return "true" if argument else "false"
    return json.dumps(argument) if isinstance(argument, (list, dict)) else str(argument)


def settle(cursor, quiet=1.0, timeout=60.0):
    """Wait until `cursor()` has kept one value for `quiet` seconds; return that value."""
    end, last, since = time.monotonic() + timeout, cursor(), time.monotonic()
    while time.monotonic() < end:
        time.sleep(0.1)
        now = cursor()
        if now != last:
            last, since = now, time.monotonic()
        elif time.monotonic() - since >= quiet:
            return last
    raise TimeoutError("the record cursor keeps moving")


def whole_frames(stream, framing, frame_size=1024):
    """The stream up to the end of its last whole frame: a capture stops anywhere."""
    if framing == "ccsds":
        return bytes(stream[:len(stream) // frame_size * frame_size])
    end = at = 0
    while (at := stream.find(START_WORD, at)) >= 0 and at + 12 <= len(stream):
        at += 8 + struct.unpack_from(">I", stream, at + 4)[0] + 4
        if at <= len(stream):
            end = at
    return bytes(stream[:end])


def tc_frames(stream, scid):
    """The TC frames of an uplink stream, whole: header (5 bytes), data, CRC-16.

    Bytes between frames are skipped, as in `packets`.
    """
    found, at, start = [], 0, struct.pack(">H", 0x2000 | scid & 0x3FF)
    while (at := stream.find(start, at)) >= 0 and at + 7 <= len(stream):
        end = at + (struct.unpack_from(">H", stream, at + 2)[0] & 0x3FF) + 1
        if end <= len(stream) and binascii.crc_hqx(bytes(stream[at:end - 2]), 0xFFFF) == struct.unpack_from(">H", stream, end - 2)[0]:
            found.append(bytes(stream[at:end]))
            at = end
        else:
            at += 1
    return found


def uplink(stream, framing, scid):
    """What the GDS put on the link, as hex text: command packets (F Prime framing) or whole TC frames."""
    return [item.hex() for item in (tc_frames(stream, scid) if framing == "ccsds" else packets(stream))]


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


def save(path, events, channels, commands, **more):
    with open(path, "w") as file:
        json.dump({"events": events, "channels": channels, "commands": commands, **more}, file, indent=1)


class Shapes:
    """The dictionary, for coverage: which type shapes each event, channel and command carries."""

    NAMES = ("enum", "array", "struct", "string", "64-bit", "float")

    def __init__(self, path):
        with open(path) as file:
            self.dictionary = json.load(file)
        self.types = {t["qualifiedName"]: t for t in self.dictionary["typeDefinitions"]}
        self.constants = {c["qualifiedName"]: c["value"] for c in self.dictionary.get("constants", [])}
        self.events = {e["id"]: self.of(*[p["type"] for p in e["formalParams"]]) for e in self.dictionary["events"]}
        self.channels = {c["id"]: self.of(c["type"]) for c in self.dictionary["telemetryChannels"]}
        self.commands = {c["name"]: self.of(*[p["type"] for p in c["formalParams"]]) for c in self.dictionary["commands"]}

    def of(self, *types):
        found = set()
        for ty in types:
            if ty["kind"] == "qualifiedIdentifier":
                definition = self.types[ty["name"]]
                if definition["kind"] == "alias":
                    found |= self.of(definition["underlyingType"])
                elif definition["kind"] == "enum":
                    found |= {"enum"} | self.of(definition["representationType"])
                elif definition["kind"] == "array":
                    found |= {"array"} | self.of(definition["elementType"])
                else:
                    found |= {"struct"} | self.of(*[m["type"] for m in definition["members"].values()])
                    found |= {"array"} if any(m.get("size") for m in definition["members"].values()) else set()
            elif ty["kind"] == "string":
                found.add("string")
            elif ty["kind"] == "float":
                found.add("float")
            elif ty["kind"] == "integer" and ty["size"] == 64:
                found.add("64-bit")
        return found

    def coverage(self, what, table, seen, problems):
        """Print how much of `table` the identifiers `seen` cover; a shape never seen is a problem."""
        shapes = set().union(*[table[key] for key in seen if key in table])
        missing = [name for name in self.NAMES if name not in shapes]
        print(f"  coverage: {len(set(seen) & set(table))} of {len(table)} {what}; "
              f"shapes seen: {', '.join(n for n in self.NAMES if n in shapes) or 'none'}"
              + (f"; NOT SEEN: {', '.join(missing)}" if missing else ""))
        if missing:
            problems.append(f"{what}: no sample with {', '.join(missing)}")


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
