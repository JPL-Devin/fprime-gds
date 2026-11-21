"""pytest plugin: record every framer/deframer call the Python CCSDS framing tests make.

Loaded with `pytest -p ccsds_recorder` (this directory on PYTHONPATH). The output file is named by
the environment variable CCSDS_RECORDER_OUTPUT. Only calls made directly by a test are recorded:
calls a framer makes to another framer (a chain to its members, `deframe_all` to `deframe`) are
part of the outer call. Only tests that pass are written.
"""
import functools
import io
import json
import logging
import os
import sys

import pytest

from fprime_gds.common.communication.ccsds.chain import (
    ChainedFramerDeframer,
    RawSpaceDataLinkFramerDeframer,
    RawSpacePacketFramerDeframer,
    SpacePacketSpaceDataLinkFramerDeframer,
)
from fprime_gds.common.communication.ccsds.space_data_link import SpaceDataLinkFramerDeframer
from fprime_gds.common.communication.ccsds.space_packet import SpacePacketFramerDeframer
from fprime_gds.common.communication.ccsds.space_packet_splitter import SpacePacketSplitterFramerDeframer
from fprime_gds.common.communication.ccsds.tm_frame_aggregator import TmFrameAggregatorFramerDeframer
from fprime_gds.common.communication.framing import FramerDeframer

RECORDED_CLASSES = [
    SpacePacketFramerDeframer,
    SpacePacketSplitterFramerDeframer,
    SpaceDataLinkFramerDeframer,
    TmFrameAggregatorFramerDeframer,
    RawSpacePacketFramerDeframer,
    RawSpaceDataLinkFramerDeframer,
    SpacePacketSpaceDataLinkFramerDeframer,
]
CONFIG_ATTRIBUTES = ["scid", "vcid", "frame_size"]


class Recorder:
    def __init__(self):
        self.depth = 0
        self.current = None
        self.instances = {}
        self.cases = []


RECORDER = Recorder()


class Tee(io.TextIOBase):
    """Write to the wrapped stream and keep a copy"""

    def __init__(self, stream):
        self.stream = stream
        self.copy = io.StringIO()

    def write(self, text):
        self.copy.write(text)
        return self.stream.write(text)

    def flush(self):
        self.stream.flush()


class ListHandler(logging.Handler):
    def __init__(self):
        super().__init__(level=logging.WARNING)
        self.messages = []

    def emit(self, record):
        self.messages.append(record.getMessage())


def to_hex(data):
    return None if data is None else bytes(data).hex()


def describe(instance):
    description = {"class": type(instance).__name__}
    if hasattr(type(instance), "get_name"):
        description["plugin_name"] = type(instance).get_name()
    for attribute in CONFIG_ATTRIBUTES:
        if hasattr(instance, attribute):
            description[attribute] = getattr(instance, attribute)
    if isinstance(instance, SpacePacketFramerDeframer):
        description["apid_enum"] = dict(sorted(type(instance.apid_obj).ENUM_DICT.items(), key=lambda item: item[1]))
    if isinstance(instance, ChainedFramerDeframer):
        description["frame_order"] = [describe(member) for member in instance.framers]
    return description


def convert_result(operation, result):
    if operation == "frame":
        return to_hex(result)
    packets, leftover, discarded = result
    if operation == "deframe":
        return {"packet": to_hex(packets), "leftover": to_hex(leftover), "discarded": to_hex(discarded)}
    return {"packets": [to_hex(packet) for packet in packets], "leftover": to_hex(leftover), "discarded": to_hex(discarded)}


def record(operation, original):
    @functools.wraps(original)
    def wrapper(self, data, *args, **kwargs):
        if RECORDER.depth or RECORDER.current is None or type(self) not in RECORDED_CLASSES:
            return original(self, data, *args, **kwargs)
        case = RECORDER.current
        if id(self) not in RECORDER.instances:
            RECORDER.instances[id(self)] = (len(case["instances"]), self)
            case["instances"].append(describe(self))
        step = {"instance": RECORDER.instances[id(self)][0], "operation": operation, "input": to_hex(data)}
        handler, tee, stderr = ListHandler(), Tee(sys.stderr), sys.stderr
        logging.getLogger("framing").addHandler(handler)
        sys.stderr = tee
        RECORDER.depth += 1
        try:
            result = original(self, data, *args, **kwargs)
            step["expected"] = convert_result(operation, result)
            return result
        except Exception as exception:
            step["expected_error"] = {"type": type(exception).__name__, "message": str(exception)}
            raise
        finally:
            RECORDER.depth -= 1
            sys.stderr = stderr
            logging.getLogger("framing").removeHandler(handler)
            step["warnings"] = handler.messages + [line for line in tee.copy.getvalue().splitlines() if line]
            case["steps"].append(step)

    return wrapper


def pytest_configure(config):
    patched = set()
    for cls in [FramerDeframer, ChainedFramerDeframer, *RECORDED_CLASSES]:
        for operation in ("frame", "deframe", "deframe_all"):
            function = cls.__dict__.get(operation)
            if function is not None and (cls, operation) not in patched:
                setattr(cls, operation, record(operation, function))
                patched.add((cls, operation))


DESCRIPTION = (
    "CCSDS Space Packet and TM/TC Space Data Link framing. One case per Python test; `instances` are the "
    "framer objects the test created (with their configuration) and `steps` are the calls the test made on "
    "them, in order. State carries from one step to the next on the same instance. frame: input payload -> "
    "expected frame. deframe: input -> one packet (null when none), the bytes kept for the next call "
    "(leftover) and the bytes thrown away (discarded). deframe_all: same with every packet found. `warnings` "
    "are the messages the Python code logged during the step."
)


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_call(item):
    name = item.nodeid.split("/")[-1]
    RECORDER.current = {"name": name, "instances": [], "steps": []}
    RECORDER.instances = {}
    outcome = yield
    case, RECORDER.current = RECORDER.current, None
    if outcome.excinfo is None and case["steps"]:
        RECORDER.cases.append(case)


def pytest_sessionfinish(session):
    output = os.environ.get("CCSDS_RECORDER_OUTPUT")
    if output:
        RECORDER.cases.sort(key=lambda case: case["name"])
        with open(output, "w") as file_handle:
            json.dump({"description": DESCRIPTION, "cases": RECORDER.cases}, file_handle, indent=1)
            file_handle.write("\n")
