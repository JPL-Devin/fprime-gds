"""HTTP and WebSocket access to the `fgds` API, standard library only."""

import base64
import json
import os
import socket
import struct
import time
import urllib.error
import urllib.parse
import urllib.request

API_VERSION = 1
DEFAULT_URL = "http://127.0.0.1:5000"


class ApiError(Exception):
    """An error answer of the API: HTTP status, error code and message."""

    def __init__(self, status, code, message):
        super().__init__(f"{status} {code}: {message}")
        self.status, self.code, self.message = status, code, message


class VersionMismatch(Exception):
    """The server speaks another API version than this client (SCR-2)."""


class Lagged(Exception):
    """The server closed the stream because this client fell behind; resume after `last_seq`."""

    def __init__(self, last_seq):
        super().__init__(f"stream lagged after record {last_seq}")
        self.last_seq = last_seq


class Client:
    """One `fgds` server. `url` is its address, with or without the `/api/v1` suffix."""

    def __init__(self, url=DEFAULT_URL, name="python", timeout=10.0, check_version=True):
        url = url.rstrip("/")
        if not url.endswith(f"/api/v{API_VERSION}"):
            url = url.split("/api/")[0] + f"/api/v{API_VERSION}"
        self.api, self.name, self.timeout = url, name, timeout
        if check_version:
            self.check_version()

    # Generic routes (SCR-1).

    def get(self, path, **params):
        return self._call("GET", path, params, None)

    def post(self, path, body=None, **params):
        return self._call("POST", path, params, {} if body is None else body)

    def _call(self, method, path, params, body):
        url = self.api + "/" + path.lstrip("/")
        pairs = []
        for key, value in params.items():
            if value is None:
                continue
            for one in value if isinstance(value, (list, tuple)) else [value]:
                pairs.append((key, "true" if one is True else "false" if one is False else str(one)))
        if pairs:
            url += "?" + urllib.parse.urlencode(pairs)
        data = None if body is None else body if isinstance(body, bytes) else json.dumps(body).encode()
        headers = {"content-type": "application/octet-stream" if isinstance(body, bytes) else "application/json",
                   "x-fgds-client": self.name}
        request = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as answer:
                text = answer.read()
        except urllib.error.HTTPError as error:
            text = error.read()
            try:
                detail = json.loads(text)["error"]
                code, message = detail["code"], detail["message"]
            except (ValueError, KeyError, TypeError):
                code, message = "http", text.decode(errors="replace")
            raise ApiError(error.code, code, message) from None
        return json.loads(text) if text else None

    def upload(self, path, destination=None, wait=60):
        """Queue a file for uplink (files plugin); the queue entry is returned.

        The start packet carries `path` as its source path, as with the Python GDS. A file of the same
        name still in the queue is waited for (`wait` seconds): the Python GDS queues it behind.
        """
        path = str(path)
        query = {"name": os.path.basename(path), "source": path}
        if destination is not None:
            query["destination"] = str(destination)
        with open(path, "rb") as file:
            content = file.read()
        deadline = time.monotonic() + wait
        while True:
            try:
                return self._call("POST", "files/uplink", query, content)
            except ApiError as error:
                if error.status != 409 or time.monotonic() > deadline:
                    raise
                time.sleep(0.05)

    def version(self):
        return self.get("version")

    def check_version(self):
        """Refuse a server of another API version, naming both versions (SCR-2)."""
        try:
            server = self.version().get("api")
        except ApiError as error:
            if error.status != 404:
                raise
            server = "unknown"
        if server != API_VERSION:
            raise VersionMismatch(
                f"fgds_client speaks API version {API_VERSION}, the server at {self.api} speaks version {server}"
            )

    def status(self):
        return self.get("status")

    def dictionary(self):
        return self.get("dictionary")

    def cursor(self):
        """Sequence number of the newest record: reading after it is "from now" (SCR-4)."""
        return int(self.get("cursor")["seq"])

    # Records of any kind (SCR-1).

    def records(self, kind=None, after_seq=None, filters=(), **query):
        """Yield the stored records matching the query, oldest first, across pages.

        `kind` is one record kind or a list; `filters` are `field:op:value` strings;
        other keywords (`id`, `run`, `before_seq`, `from`, `to`, `text`, `limit`) are
        passed as given. With `limit`, at most one page of that size is returned.
        """
        kinds = ",".join(kind) if isinstance(kind, (list, tuple)) else kind
        while True:
            page = self.get("records", kind=kinds, after_seq=after_seq, filter=list(filters), **query)
            yield from page["records"]
            after_seq = page.get("next")
            if after_seq is None or "limit" in query:
                return

    def latest(self, kind=None, filters=(), **query):
        """Current state: the latest matching record per identifier."""
        kinds = ",".join(kind) if isinstance(kind, (list, tuple)) else kind
        return self.get("records/latest", kind=kinds, filter=list(filters), **query)["records"]

    def stream(self, kind=None, after_seq=None):
        """Yield records after `after_seq` (history, then live) with no gap and no repeat.

        Without `after_seq` only records created from now on are delivered. The
        returned `Stream` is iterated; it ends when the server or `close()` closes
        it and raises `Lagged` when the server dropped this client for being too slow.
        """
        kinds = ",".join(kind) if isinstance(kind, (list, tuple)) else kind
        params = [(k, v) for k, v in (("kind", kinds), ("after_seq", after_seq)) if v is not None]
        return Stream(WebSocket(self.api + "/stream", params, self.timeout))

    # Commands (SCR-1).

    def send_command(self, name, args=None, dry_run=False):
        """Send a command by name with positional (list) or named (dict) arguments.

        Returns `{"seq", "entity"}` of the command record; with `dry_run`, nothing
        is sent and `{"dry_run", "packet" (bytes), "command"}` is returned.
        """
        body = {"name": name, "args": [] if args is None else args}
        answer = self.post("commands", body, dry_run=True if dry_run else None)
        if dry_run:
            answer["packet"] = base64.b64decode(answer["packet"])
        return answer

    def commands(self, **query):
        """The commands sent through this server (all clients), oldest first."""
        return list(self.records("fprime.command", **query))

    def events(self, **query):
        return list(self.records("fprime.event", **query))

    def channels(self, **query):
        return list(self.records("fprime.channel", **query))

    def definitions(self, what):
        """Dictionary definitions: `what` is `commands`, `events` or `channels`."""
        return self.get(f"{what}/definitions")


class Stream:
    """The records of one stream connection; `close()` may be called from another thread."""

    def __init__(self, socket_):
        self.socket = socket_

    def __iter__(self):
        for text in self.socket:
            message = json.loads(text)
            if message.get("type") == "record":
                yield message["record"]
            elif message.get("type") == "lagged":
                raise Lagged(int(message["last_seq"]))

    def close(self):
        self.socket.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class WebSocket:
    """A minimal RFC 6455 client for the text frames of the `fgds` stream."""

    def __init__(self, url, params, timeout):
        parts = urllib.parse.urlsplit(url)
        path = parts.path + ("?" + urllib.parse.urlencode(params) if params else "")
        port = parts.port or 80
        self.sock = socket.create_connection((parts.hostname, port), timeout=timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall(
            (
                f"GET {path} HTTP/1.1\r\nHost: {parts.hostname}:{port}\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        self.buffer = b""
        while b"\r\n\r\n" not in self.buffer:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("stream closed during the WebSocket handshake")
            self.buffer += chunk
        head, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
        status = head.split(b"\r\n", 1)[0].decode(errors="replace")
        if " 101 " not in status + " ":
            length = 0
            for line in head.split(b"\r\n")[1:]:
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":")[1])
            while len(self.buffer) < length:
                chunk = self.sock.recv(4096)
                if not chunk:
                    break
                self.buffer += chunk
            self.sock.close()
            try:
                detail = json.loads(self.buffer)["error"]
                raise ApiError(int(status.split()[1]), detail["code"], detail["message"])
            except (ValueError, KeyError, TypeError, IndexError):
                raise ConnectionError(f"WebSocket upgrade refused: {status}") from None
        self.sock.settimeout(None)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()

    def _read(self, count):
        while len(self.buffer) < count:
            chunk = self.sock.recv(65536)
            if not chunk:
                return None
            self.buffer += chunk
        data, self.buffer = self.buffer[:count], self.buffer[count:]
        return data

    def _send(self, opcode, payload):
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(bytes([0x80 | opcode, 0x80 | len(payload)]) + mask + masked)

    def __iter__(self):
        """Yield each text message until the connection closes."""
        message = b""
        while True:
            try:
                head = self._read(2)
                if head is None:
                    return
                opcode, length = head[0] & 0x0F, head[1] & 0x7F
                if length == 126:
                    length = struct.unpack(">H", self._read(2))[0]
                elif length == 127:
                    length = struct.unpack(">Q", self._read(8))[0]
                payload = self._read(length)
            except (OSError, TypeError, struct.error):
                return
            if payload is None or opcode == 0x8:
                return
            if opcode == 0x9:
                self._send(0xA, payload[:125])
            elif opcode in (0x0, 0x1, 0x2):
                message += payload
                if head[0] & 0x80:
                    yield message.decode()
                    message = b""
