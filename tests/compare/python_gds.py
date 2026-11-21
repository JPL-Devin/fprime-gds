#!/usr/bin/env python3
"""The Python GDS side: run the scenario live, or decode captured bytes. Needs the fprime virtual environment."""
import argparse
import shutil
import time

from fprime_gds.common.communication.framing import FpFramerDeframer
from fprime_gds.common.decoders.ch_decoder import ChDecoder
from fprime_gds.common.decoders.event_decoder import EventDecoder
from fprime_gds.common.decoders.pkt_decoder import PktDecoder
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.executables.cli import ConfigDrivenParser, StandardPipelineParser

import scenario


def flight_time(t):
    return [int(t.timeBase.numeric_value if hasattr(t.timeBase, "numeric_value") else t.timeBase.val_int()),
            int(t.timeContext), int(t.seconds), int(t.useconds)]


def event(e):
    return {
        "id": e.id, "name": e.template.get_full_name(), "severity": e.template.get_severity().name,
        "time": flight_time(e.time), "args": scenario.plain([a.val for a in e.args]),
        "display_text": e.display_text,
    }


def channel(c):
    return {
        "id": c.id, "name": c.template.get_full_name(), "time": flight_time(c.time),
        "value": scenario.plain(c.val_obj.val), "display_text": str(c.display_text),
    }


def command(c):
    return {"name": c.template.get_full_name(), "opcode": c.id, "args": scenario.plain([a.val for a in c.get_args()])}


def live(args):
    gds_port, tts_port = scenario.free_port(), scenario.free_port()
    shared = ["--dictionary", args.dictionary, "--no-zmq", "--tts-port", str(tts_port), "--tts-addr", "127.0.0.1",
              "--file-storage-directory", f"{args.out}/files"]
    gds = scenario.start(
        [shutil.which("fprime-gds"), "-n", "-g", "none", "--ip-address", "127.0.0.1", "--ip-port", str(gds_port),
         "--framing-selection", "fprime", "--logs", f"{args.out}/gds-logs", *shared],
        open(f"{args.out}/gds.log", "w"),
    )
    ref = pipeline = None
    try:
        scenario.wait_for("the Python GDS link port", lambda: scenario.socket.create_connection(("127.0.0.1", gds_port)).close() or True)
        proxy = scenario.Proxy(gds_port)
        parsed, _, _ = ConfigDrivenParser.parse_known_args(
            [StandardPipelineParser], arguments=[*shared, "--logs", f"{args.out}/client-logs"], client=True)
        pipeline = StandardPipelineParser.pipeline_factory(parsed)
        ref = scenario.start_ref(args.ref, proxy, args.out)
        histories = pipeline.histories
        channels = []
        scenario.wait_for("telemetry", lambda: channels.extend(histories.channels.retrieve()) or channels)
        for name, arguments in scenario.COMMANDS:
            pipeline.send_command(name, [str(a) for a in arguments])
            time.sleep(scenario.GAP)
        time.sleep(scenario.SETTLE)
        channels.extend(histories.channels.retrieve_new() if hasattr(histories.channels, "retrieve_new") else [])
        scenario.save(
            f"{args.out}/live.json",
            [event(e) for e in histories.events.retrieve()],
            [channel(c) for c in channels],
            [command(c) for c in histories.commands.retrieve()],
        )
        open(f"{args.out}/uplink.bin", "wb").write(proxy.uplink)
        open(f"{args.out}/downlink.bin", "wb").write(proxy.downlink)
    finally:
        if pipeline:
            pipeline.disconnect()
        for process in (ref, gds):
            if process:
                scenario.stop(process)


def replay(args):
    dictionaries = Dictionaries.load_dictionaries_into_config(args.dictionary)
    events, channels = EventDecoder(dictionaries.event_id), ChDecoder(dictionaries.channel_id)
    packetized = PktDecoder(dictionaries.packet, dictionaries.channel_id) if dictionaries.packet else None
    found, rest, discarded = FpFramerDeframer().deframe_all(open(args.capture, "rb").read(), no_copy=True)
    assert not rest and not discarded, "the capture does not end on a frame boundary"
    out_events, out_channels = [], []
    for packet in found:
        kind, body = int.from_bytes(packet[:2], "big"), packet[2:]
        if kind == 2:
            out_events += [event(e) for e in events.decode_api(body)]
        elif kind == 1:
            out_channels += [channel(c) for c in channels.decode_api(body)]
        elif kind == 4 and packetized:
            out_channels += [channel(c) for c in packetized.decode_api(body).get_chs()]
    scenario.save(args.out, out_events, out_channels, [])


parser = argparse.ArgumentParser()
parser.add_argument("mode", choices=["live", "replay"])
parser.add_argument("--dictionary", required=True)
parser.add_argument("--ref")
parser.add_argument("--capture")
parser.add_argument("--out", required=True)
arguments = parser.parse_args()
{"live": live, "replay": replay}[arguments.mode](arguments)
