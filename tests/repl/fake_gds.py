"""A stand-in for the GDS, for what the real one does not do on request: answer late, stop and
start again with another dictionary, send text with control characters. It speaks the part of
the API `fgds repl` and `fgds tui` use. Standard library only.
"""
import base64
import hashlib
import json
import socket
import struct
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

KINDS = ("fprime.event", "fprime.channel", "fprime.command")


def dictionary(*commands):
    return {"commands": [{"name": name, "opcode": 16 + at, "formalParams": []} for at, name in enumerate(commands)],
            "events": [], "telemetryChannels": [], "typeDefinitions": []}


def event(name, text, severity="ACTIVITY_HI"):
    return {"kind": "fprime.event", "flight_time": {"base": 2, "context": 0, "seconds": 1, "microseconds": 0},
            "body": {"name": name, "severity": severity, "display_text": text}}


def channel(name, text):
    return {"kind": "fprime.channel", "flight_time": {"base": 2, "context": 0, "seconds": 1, "microseconds": 0},
            "body": {"name": name, "display_text": text}}


class Fake:
    """`posts` holds (path, body) of every POST, as it arrives. `delay` is how long `/commands` waits
    before it answers. `refuse`, when set, is the message of a 400 answer to `/commands`."""

    def __init__(self, port, dictionary, version="one", kinds=KINDS, first_seq=1):
        self.port, self.dictionary, self.version, self.kinds = port, dictionary, version, list(kinds)
        self.posts, self.delay, self.refuse, self.streams, self.opened = [], 0.0, None, [], 0
        # `fail`: a path and the (status, message) it answers with; `no_stream`: `/stream` answers 404.
        self.fail, self.no_stream = {}, False
        self.records, self.seq, self.lock = [], first_seq - 1, threading.Lock()
        fake = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def answer(self, body, status=200):
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(data)))
                self.send_header("connection", "close")
                self.end_headers()
                try:
                    self.wfile.write(data)
                except OSError:  # the client left without its answer
                    pass
                self.close_connection = True

            def do_GET(self):
                url = urlparse(self.path)
                query = parse_qs(url.query)
                path = url.path.removeprefix("/api/v1")
                if path in fake.fail:
                    status, message = fake.fail[path]
                    self.answer({"error": {"code": "x", "message": message}}, status)
                elif path == "/stream" and fake.no_stream:
                    self.answer({"error": {"code": "x", "message": "no stream here"}}, 404)
                elif path == "/version":
                    self.answer({"api": 1, "gds": "fake", "dictionary": fake.version, "plugins": []})
                elif path == "/dictionary":
                    self.answer(fake.dictionary)
                elif path == "/kinds":
                    self.answer([{"id": kind} for kind in fake.kinds])
                elif path == "/records":
                    after = int(query.get("after_seq", ["-1"])[0])
                    self.answer({"records": [r for r in fake.records if int(r["seq"]) > after][:1], "next": None})
                elif path == "/stream":
                    if not set(query.get("kind", [""])[0].split(",")) <= set(fake.kinds):
                        return self.answer({"error": {"message": "unknown kind"}}, 400)
                    accept = base64.b64encode(hashlib.sha1(
                        (self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
                    self.send_response(101)
                    self.send_header("Upgrade", "websocket")
                    self.send_header("Connection", "Upgrade")
                    self.send_header("Sec-WebSocket-Accept", accept)
                    self.end_headers()
                    with fake.lock:
                        fake.streams.append(self.connection)
                        fake.opened += 1
                    try:  # until the client or `stop` closes it
                        while self.connection.recv(4096):
                            pass
                    except OSError:
                        pass
                    with fake.lock:
                        if self.connection in fake.streams:
                            fake.streams.remove(self.connection)
                    self.close_connection = True
                else:
                    self.answer({"error": {"message": "no such path"}}, 404)

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["content-length"])))
                path = urlparse(self.path).path.removeprefix("/api/v1")
                fake.posts.append((path, body))
                if path == "/commands":
                    time.sleep(fake.delay)
                    if fake.refuse:
                        return self.answer({"error": {"message": fake.refuse}}, 400)
                    record = fake.emit({"kind": "fprime.command", "raw": "",
                                        "body": {"name": body["name"], "args": body["args"], "opcode": 16, "outcome": "sent",
                                                 "client": self.headers.get("x-fgds-client", "?")}})
                    return self.answer({"seq": record["seq"]})
                self.answer({})

        ThreadingHTTPServer.allow_reuse_address = True
        self.server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.port}"

    def emit(self, record):
        """Records `record` and sends it to every open stream."""
        with self.lock:
            self.seq += 1
            record = {"seq": str(self.seq), "time": "2026-01-01T00:00:00.000000Z", **record}
            self.records.append(record)
            data = json.dumps({"type": "record", "record": record}).encode()
            size = bytes([len(data)]) if len(data) < 126 else b"\x7e" + struct.pack(">H", len(data))
            for stream in list(self.streams):
                try:
                    stream.sendall(b"\x81" + size + data)
                except OSError:
                    self.streams.remove(stream)
        return record

    def stop(self):
        """Stops listening and closes every stream, as a GDS that ends does."""
        self.server.shutdown()
        self.server.server_close()
        with self.lock:
            for stream in self.streams:
                try:
                    stream.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
            self.streams.clear()
