#!/usr/bin/env python3
"""The Python GDS side. Needs the fprime virtual environment.

live       `fprime-gds` with its GUI server, driven and read through its REST endpoints only.
replay     captured bytes through the Python deframers and decoders.
synthetic  a capture built with the Python encoders: every event and channel of the dictionary
           with boundary values, and what the Python decoders make of it.
"""
import argparse
import json
import math
import shutil
import time
import urllib.error
import urllib.request

from fprime_gds.common.communication.framing import FpFramerDeframer
from fprime_gds.common.decoders.ch_decoder import ChDecoder
from fprime_gds.common.decoders.event_decoder import EventDecoder
from fprime_gds.common.decoders.pkt_decoder import PktDecoder
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.common.communication.ccsds.chain import SpacePacketSpaceDataLinkFramerDeframer
from fprime_gds.common.data_types.ch_data import ChData
from fprime_gds.common.data_types.event_data import EventData
from fprime_gds.common.encoders.ch_encoder import ChEncoder
from fprime_gds.common.encoders.event_encoder import EventEncoder
from fprime_gds.common.models.serialize.array_type import ArrayType
from fprime_gds.common.models.serialize.bool_type import BoolType
from fprime_gds.common.models.serialize.enum_type import EnumType
from fprime_gds.common.models.serialize.numerical_types import FloatType, IntegerType
from fprime_gds.common.models.serialize.serializable_type import SerializableType
from fprime_gds.common.models.serialize.string_type import StringType
from fprime_gds.common.models.serialize.time_type import TimeType

import scenario


def flight_time(t):
    return [int(t.timeBase.numeric_value if hasattr(t.timeBase, "numeric_value") else t.timeBase.val_int()),
            int(t.timeContext), int(t.seconds), int(t.useconds)]


def event(e):
    return {
        "id": e.id, "name": e.template.get_full_name(), "severity": e.template.get_severity().name,
        "time": flight_time(e.time),
        "args": scenario.plain([[spec[0], a.val] for spec, a in zip(e.template.get_args(), e.args)]),
        "display_text": e.display_text,
    }


def channel(c):
    return {
        "id": c.id, "name": c.template.get_full_name(), "time": flight_time(c.time),
        "value": scenario.plain(c.val_obj.val), "display_text": str(c.display_text),
    }


class Rest:
    """The REST endpoints of the Python GDS's GUI server: what its GUI shows."""

    def __init__(self, port):
        self.url, self.seen = f"http://127.0.0.1:{port}", {"commands": [], "events": [], "channels": []}

    def call(self, path, body=None):
        request = urllib.request.Request(self.url + path, method="PUT" if body else "GET",
                                         data=json.dumps(body).encode() if body else None,
                                         headers={"content-type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.load(error)

    def poll(self):
        """New items of the three histories; a session sees what arrived after its first request."""
        for kind, items in self.seen.items():
            answer = self.call(f"/{kind}?session=compare")[1]
            assert not answer["errors"], answer["errors"]
            items += answer["history"]
        return sum(len(items) for items in self.seen.values())

    def dictionary(self, kind):
        return {entry["id"]: entry for entry in self.call(f"/dictionary/{kind}")[1]["dictionary"].values()}

    def send(self, name, arguments):
        status, answer = self.call(f"/commands/{name}", {"key": "0xfeedcafe", "arguments": [scenario.text(a) for a in arguments]})
        return {"name": name, "status": status, "message": answer.get("message", "")}

    def export(self):
        def time_of(item):
            return [item["time"][key] for key in ("base", "context", "seconds", "microseconds")]

        events, channels, commands = (self.dictionary(kind) for kind in ("events", "channels", "commands"))
        return (
            [{"id": e["id"], "name": events[e["id"]]["full_name"], "severity": events[e["id"]]["severity"]["value"].split(".")[-1],
              "time": time_of(e), "display_text": e["display_text"]} for e in self.seen["events"]],
            [{"id": c["id"], "name": channels[c["id"]]["full_name"], "time": time_of(c),
              "value": scenario.plain(c["val"]), "display_text": str(c["display_text"])} for c in self.seen["channels"]],
            [{"name": commands[c["id"]]["full_name"], "opcode": c["id"],
              "args": scenario.plain([[spec["name"], value] for spec, value in zip(commands[c["id"]]["args"], c["args"])])}
             for c in self.seen["commands"]],
        )


def live(args):
    gds_port, tts_port, gui_port = scenario.free_port(), scenario.free_port(), scenario.free_port()
    framing = "fprime" if args.framing == "fprime" else "space-packet-space-data-link"
    gds = scenario.start(
        [shutil.which("fprime-gds"), "-n", "-g", "html", "--gui-port", str(gui_port), "--skip-browser-open",
         "--ip-address", "127.0.0.1", "--ip-port", str(gds_port), "--framing-selection", framing,
         "--dictionary", args.dictionary, "--no-zmq", "--tts-port", str(tts_port), "--tts-addr", "127.0.0.1",
         "--file-storage-directory", f"{args.out}/files", "--logs", f"{args.out}/gds-logs"],
        open(f"{args.out}/gds.log", "w"),
    )
    ref = None
    try:
        rest = Rest(gui_port)
        scenario.wait_for("the Python GDS link port", lambda: scenario.socket.create_connection(("127.0.0.1", gds_port)).close() or True)
        scenario.wait_for("the Python GDS REST server", lambda: rest.poll() >= 0, timeout=60)
        proxy = scenario.Proxy(gds_port)
        ref = scenario.start_ref(args.ref, proxy, args.out)
        scenario.wait_for("telemetry", lambda: rest.poll() and rest.seen["channels"])
        answers = []
        for name, arguments in scenario.COMMANDS + scenario.REJECTED:
            answers.append(rest.send(name, arguments))
            time.sleep(scenario.GAP)
            rest.poll()
        time.sleep(scenario.SETTLE)
        scenario.stop(ref)
        scenario.settle(rest.poll)
        scenario.save(f"{args.out}/live.json", *rest.export(), answers=answers)
        open(f"{args.out}/uplink.bin", "wb").write(proxy.uplink)
        open(f"{args.out}/downlink.bin", "wb").write(proxy.downlink)
    finally:
        for process in (ref, gds):
            if process:
                scenario.stop(process)


def decode(dictionaries, found, path):
    events, channels = EventDecoder(dictionaries.event_id), ChDecoder(dictionaries.channel_id)
    packetized = PktDecoder(dictionaries.packet, dictionaries.channel_id) if dictionaries.packet else None
    out_events, out_channels = [], []
    for packet in found:
        kind, body = int.from_bytes(packet[:2], "big"), packet[2:]
        if kind == 2:
            out_events += [event(e) for e in events.decode_api(body)]
        elif kind == 1:
            out_channels += [channel(c) for c in channels.decode_api(body)]
        elif kind == 4 and packetized:
            out_channels += [channel(c) for c in packetized.decode_api(body).get_chs()]
    scenario.save(path, out_events, out_channels, [])


def replay(args):
    dictionaries = Dictionaries.load_dictionaries_into_config(args.dictionary)
    deframer = FpFramerDeframer() if args.framing == "fprime" else SpacePacketSpaceDataLinkFramerDeframer(scid=None, vcid=1, frame_size=None)
    found, rest, discarded = deframer.deframe_all(open(args.capture, "rb").read(), no_copy=True)
    assert not rest and not discarded, "the capture does not end on a frame boundary"
    decode(dictionaries, found, args.out)


FLOATS = {32: [0.0, -1.5, 3.4028234663852886e38, math.nan, math.inf, -math.inf, 1.401298464324817e-45],
          64: [0.0, -1.5, 1.7976931348623157e308, math.nan, math.inf, -math.inf, 5e-324]}
VARIANTS = 7


def boundary(kind, variant):
    """A value of the type class `kind`: the `variant`-th of its boundary values."""
    if issubclass(kind, BoolType):
        return variant % 2 == 0
    if issubclass(kind, EnumType):
        names = list(kind.ENUM_DICT)
        return names[[0, -1, len(names) // 2][variant % 3]]
    if issubclass(kind, FloatType):
        return FLOATS[kind.get_bits()][variant % 7]
    if issubclass(kind, IntegerType):
        bits = kind.get_bits()
        if kind.__name__.startswith("U"):
            return [0, 2 ** bits - 1, 1, 2 ** (bits - 1)][variant % 4]
        return [-(2 ** (bits - 1)), 2 ** (bits - 1) - 1, -1, 0][variant % 4]
    if issubclass(kind, StringType):
        size = min(getattr(kind, "MAX_LENGTH", None) or 40, 200)
        return ["", "x" * size, "a {} %d \"b\"", "x"][variant % 4]
    if issubclass(kind, ArrayType):
        return [boundary(kind.MEMBER_TYPE, variant + index) for index in range(kind.LENGTH)]
    if issubclass(kind, SerializableType):
        return {member[0]: boundary(member[1], variant + index) for index, member in enumerate(kind.MEMBER_LIST)}
    raise TypeError(f"no boundary value for {kind}")


def synthetic(args):
    """Every event and channel, VARIANTS times, in F Prime frames; decoded back by the Python decoders."""
    dictionaries = Dictionaries.load_dictionaries_into_config(args.dictionary)
    framer, events, channels = FpFramerDeframer(), EventEncoder(), ChEncoder()
    stream, found = bytearray(), []
    for variant in range(VARIANTS):
        stamp = TimeType(2, 0, 1700000000 + variant, [0, 999999, 1][variant % 3])
        for template in dictionaries.event_id.values():
            values = tuple(spec[2](boundary(spec[2], variant + index)) for index, spec in enumerate(template.get_args()))
            found.append(events.encode_api(EventData(values, stamp, template))[4:])
        for template in dictionaries.channel_id.values():
            kind = template.get_type_obj()
            found.append(channels.encode_api(ChData(kind(boundary(kind, variant)), stamp, template))[4:])
    for packet in found:
        stream += framer.frame(packet)
    open(args.capture, "wb").write(stream)
    decode(dictionaries, found, args.out)


parser = argparse.ArgumentParser()
parser.add_argument("mode", choices=["live", "replay", "synthetic"])
parser.add_argument("--dictionary", required=True)
parser.add_argument("--framing", choices=["ccsds", "fprime"], default="ccsds")
parser.add_argument("--ref")
parser.add_argument("--capture")
parser.add_argument("--out", required=True)
arguments = parser.parse_args()
{"live": live, "replay": replay, "synthetic": synthetic}[arguments.mode](arguments)
