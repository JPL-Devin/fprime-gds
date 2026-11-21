#!/usr/bin/env python3
"""The fgds side: run the scenario live, or decode captured bytes. Standard library only."""
import argparse
import json
import socket
import time
import urllib.error
import urllib.request

import scenario


class Fgds:
    """A running `fgds`, reached through its HTTP API."""

    def __init__(self, binary, dictionary, out, framing):
        http, self.link_port = scenario.free_port(), scenario.free_port()
        self.api = f"http://127.0.0.1:{http}/api/v1"
        self.process = scenario.start(
            [binary, "run", "--dictionary", dictionary, "--bind", f"127.0.0.1:{http}",
             "--comm-tcp-address", f"127.0.0.1:{self.link_port}", "--framing", framing],
            open(f"{out}/fgds.log", "w"),
        )
        scenario.wait_for("fgds", lambda: self.get("/version"))
        self.shapes = scenario.Shapes(dictionary).dictionary
        self.names = {kind: {item["name"]: [p["name"] for p in item["formalParams"]] for item in self.shapes[kind]}
                      for kind in ("events", "commands")}

    def get(self, path):
        with urllib.request.urlopen(self.api + path, timeout=10) as response:
            return json.load(response)

    def send(self, name, arguments):
        request = urllib.request.Request(self.api + "/commands", json.dumps({"name": name, "args": arguments}).encode(),
                                         {"content-type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                return {"name": name, "status": response.status, "message": "success"}
        except urllib.error.HTTPError as error:
            return {"name": name, "status": error.code, "message": error.read().decode()}

    def cursor(self):
        return self.get("/cursor")["seq"]

    def records(self, kind):
        found, after = [], 0
        while after is not None:
            page = self.get(f"/records?kind={kind}&after_seq={after}&limit=10000")
            found += page["records"]
            after = page["next"]
        return found

    def pairs(self, kind, body):
        """Arguments as (name, value) pairs in declaration order."""
        return scenario.plain([[name, body["args"][name]] for name in self.names[kind][body["name"]]])

    def export(self, path, **more):
        scenario.settle(self.cursor)

        def flight_time(r):
            t = r["flight_time"]
            return [t["base"], t["context"], t["seconds"], t["microseconds"]]

        events = [
            {"id": r["id"], "name": r["body"]["name"], "severity": r["body"]["severity"], "time": flight_time(r),
             "args": self.pairs("events", r["body"]), "display_text": r["body"]["display_text"]}
            for r in self.records("fprime.event")
        ]
        channels = [
            {"id": r["id"], "name": r["body"]["name"], "time": flight_time(r),
             "value": scenario.plain(r["body"]["value"]), "display_text": r["body"]["display_text"]}
            for r in self.records("fprime.channel")
        ]
        commands = [
            {"name": r["body"]["name"], "opcode": r["id"], "args": self.pairs("commands", r["body"])}
            for r in self.records("fprime.command") if r["body"].get("outcome", "sent") == "sent"
        ]
        problems = self.records("gds.decode_error") + self.records("gds.unknown_packet") + self.records("raw.discarded")
        assert not problems, f"fgds could not decode: {problems[:3]}"
        scenario.save(path, events, channels, commands, **more)


def live(args):
    gds = Fgds(args.fgds, args.dictionary, args.out, args.framing)
    ref = None
    try:
        proxy = scenario.Proxy(gds.link_port)
        ref = scenario.start_ref(args.ref, proxy, args.out)
        scenario.wait_for("telemetry", lambda: gds.get("/records?kind=fprime.channel&limit=1")["records"])
        answers = []
        for name, arguments in scenario.COMMANDS + scenario.REJECTED:
            answers.append(gds.send(name, arguments))
            time.sleep(scenario.GAP)
        time.sleep(scenario.SETTLE)
        scenario.stop(ref)
        gds.export(f"{args.out}/live.json", answers=answers)
        open(f"{args.out}/uplink.bin", "wb").write(proxy.uplink)
        open(f"{args.out}/downlink.bin", "wb").write(proxy.downlink)
    finally:
        for process in (ref, gds.process):
            if process:
                scenario.stop(process)


def replay(args):
    capture = open(args.capture, "rb").read()
    gds = Fgds(args.fgds, args.dictionary, args.log_dir, args.framing)
    try:
        flight = scenario.wait_for("the link", lambda: socket.create_connection(("127.0.0.1", gds.link_port)))
        flight.sendall(capture)
        scenario.wait_for("the replay", lambda: gds.get("/status")["counters"].get("bytes_received") == len(capture))
        gds.export(args.out)
    finally:
        scenario.stop(gds.process)


parser = argparse.ArgumentParser()
parser.add_argument("mode", choices=["live", "replay"])
parser.add_argument("--fgds", required=True)
parser.add_argument("--dictionary", required=True)
parser.add_argument("--framing", choices=["ccsds", "fprime"], default="fprime")
parser.add_argument("--ref")
parser.add_argument("--capture")
parser.add_argument("--log-dir")
parser.add_argument("--out", required=True)
arguments = parser.parse_args()
{"live": live, "replay": replay}[arguments.mode](arguments)
