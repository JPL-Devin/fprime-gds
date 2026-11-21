#!/usr/bin/env python3
"""Generate the byte vector files under `vectors/` and the sequence manifest by running the Python GDS.

Usage: gen_vectors.py <fixtures dir>

Every vector comes from a test of the Python GDS (named in its `source`). `origin` is `asserted`
when the Python test compares against the same literal bytes, and `generated` when the test only
checks a round trip or a property and the bytes were produced by running the Python code here.
The generator asserts that the bytes produced by the Python code equal the asserted ones.
"""
import contextlib
import io
import json
import struct
import sys
import tempfile
import zlib
from pathlib import Path

from fprime_gds.common.communication.framing import FpFramerDeframer, TcpServerFramerDeframer
from fprime_gds.common.data_types.ch_data import ChData
from fprime_gds.common.data_types.cmd_data import CmdData
from fprime_gds.common.data_types.event_data import EventData
from fprime_gds.common.data_types.file_data import CancelPacketData, DataPacketData, EndPacketData, StartPacketData
from fprime_gds.common.data_types.pkt_data import PktData
from fprime_gds.common.decoders.ch_decoder import ChDecoder
from fprime_gds.common.decoders.event_decoder import EventDecoder
from fprime_gds.common.decoders.file_decoder import FileDecoder
from fprime_gds.common.decoders.pkt_decoder import PktDecoder
from fprime_gds.common.distributor.distributor import Distributor
from fprime_gds.common.encoders.ch_encoder import ChEncoder
from fprime_gds.common.encoders.cmd_encoder import CmdEncoder
from fprime_gds.common.encoders.event_encoder import EventEncoder
from fprime_gds.common.encoders.file_encoder import FileEncoder
from fprime_gds.common.encoders.pkt_encoder import PktEncoder
from fprime_gds.common.loaders.ch_json_loader import ChJsonLoader
from fprime_gds.common.loaders.cmd_json_loader import CmdJsonLoader
from fprime_gds.common.loaders.event_json_loader import EventJsonLoader
from fprime_gds.common.loaders.pkt_json_loader import PktJsonLoader
from fprime_gds.common.loaders.prm_json_loader import PrmJsonLoader
from fprime_gds.common.loaders.type_json_loader import TypeJsonLoader
from fprime_gds.common.models.serialize.array_type import ArrayType
from fprime_gds.common.models.serialize.bool_type import BoolType
from fprime_gds.common.models.serialize.enum_type import EnumType
from fprime_gds.common.models.serialize.numerical_types import (
    F32Type, F64Type, I8Type, I16Type, I32Type, I64Type, U8Type, U16Type, U32Type, U64Type,
)
from fprime_gds.common.models.serialize.serializable_type import SerializableType
from fprime_gds.common.models.serialize.string_type import StringType
from fprime_gds.common.models.serialize.time_type import TimeType
from fprime_gds.common.templates.ch_template import ChTemplate
from fprime_gds.common.templates.cmd_template import CmdTemplate
from fprime_gds.common.templates.event_template import EventTemplate
from fprime_gds.common.templates.pkt_template import PktTemplate
from fprime_gds.common.tools import seqgen
from fprime_gds.common.tools.params import (
    convert_json, decode_dat_to_params, params_to_csv, params_to_json, params_to_text,
)
from fprime_gds.common.utils.config_manager import ConfigManager
from fprime_gds.common.utils.cleanup import globals_cleanup
from fprime_gds.common.utils.event_severity import EventSeverity
from fprime_gds.common.utils.string_util import format_string_template, preprocess_c_style_format_str, preprocess_fpp_format_str

FIXTURES = Path(sys.argv[1]).resolve()
VECTORS = FIXTURES / "vectors"
SEQUENCES = FIXTURES / "sequences"
TESTS = "test/fprime_gds/common"
CONFIG_TYPES = [
    "FwPacketDescriptorType", "FwOpcodeType", "FwChanIdType", "FwEventIdType", "FwTlmPacketizeIdType",
    "FwTimeContextStoreType", "FwSizeStoreType",
]


def write(name, description, vectors, **extra):
    VECTORS.mkdir(parents=True, exist_ok=True)
    content = {"description": description, **extra, "vectors": vectors}
    (VECTORS / name).write_text(json.dumps(content, indent=1) + "\n")


def type_name(type_class):
    return type_class.__name__[: -len("Type")]


def describe_type(type_class):
    """Language-neutral description of an F Prime type"""
    if issubclass(type_class, EnumType):
        return {"kind": "enum", "name": type_class.__name__, "representation": type_name(type_class.REP_TYPE), "members": type_class.ENUM_DICT}
    if issubclass(type_class, StringType):
        return {"kind": "string", "name": type_class.__name__, "max_length": type_class.MAX_LENGTH}
    if issubclass(type_class, ArrayType):
        return {"kind": "array", "name": type_class.__name__, "length": type_class.LENGTH, "member": describe_type(type_class.MEMBER_TYPE)}
    if issubclass(type_class, SerializableType):
        return {"kind": "struct", "name": type_class.__name__, "members": [{"name": member[0], "type": describe_type(member[1])} for member in type_class.MEMBER_LIST]}
    if type_class is TimeType:
        return {"kind": "time"}
    return {"kind": type_name(type_class).lower() if type_class is BoolType else type_name(type_class)}


def config(msg_len=None):
    manager = ConfigManager()
    description = {name: type_name(manager.get_type(name)) for name in CONFIG_TYPES}
    description["msg_len"] = type_name(msg_len or manager.get_config("msg_len"))
    return description


def time_value(time):
    return {"base": time.timeBase.val, "base_value": time.timeBase.numeric_value, "context": time.timeContext, "seconds": time.seconds, "microseconds": time.useconds}


def error(exception):
    return {"type": type(exception).__name__, "message": str(exception)}


# ---------------------------------------------------------------------------------------------
# Type serialization
# ---------------------------------------------------------------------------------------------
def type_vectors():
    source = f"{TESTS}/models/serialize/test_types.py"
    vectors = []

    def valid(test, type_class, values):
        for value in values:
            data = type_class(value).serialize()
            decoded = type_class()
            decoded.deserialize(data, 0)
            vector = {
                "source": f"{source}::{test}", "origin": "generated", "operation": "serialize_deserialize",
                "type": describe_type(type_class), "value": value, "bytes": data.hex(),
                "max_size": type_class.getMaxSize(),
            }
            if decoded.val != value:  # F32 values that are not exactly representable
                vector["deserialized_value"] = decoded.val
            vectors.append(vector)
            # Deserialization from an offset, as the Python test does
            offset = type_class()
            offset.deserialize(b"\x00" * 3 + data, 3)
            assert offset.val == decoded.val

    def invalid(test, type_class, values):
        for value in values:
            try:
                type_class(value)
            except Exception as exception:
                vectors.append({
                    "source": f"{source}::{test}", "origin": "generated", "operation": "construct_invalid",
                    "type": describe_type(type_class), "value": value, "expected_error": error(exception),
                })
            else:
                raise AssertionError(f"{type_class} accepted {value}")

    def short(test, type_class, value):
        """Deserialization of truncated data"""
        data = type_class(value).serialize()[:-1]
        try:
            type_class().deserialize(data, 0)
        except Exception as exception:
            vectors.append({
                "source": f"{source}::{test}", "origin": "generated", "operation": "deserialize_invalid",
                "type": describe_type(type_class), "bytes": data.hex(), "expected_error": error(exception),
            })
        else:
            raise AssertionError(f"{type_class} deserialized truncated data")

    valid("test_boolean_nominal", BoolType, [True, False])
    invalid("test_boolean_off_nominal", BoolType, [0, 1, "True", 1.5])
    short("valid_values_test", BoolType, True)
    for bad in ("00", "01", "02", "7f"):  # only 0xFF is true, only 0x00 is false
        value = BoolType()
        try:
            value.deserialize(bytes.fromhex(bad), 0)
            vectors.append({"source": f"{source}::test_boolean_nominal", "origin": "generated", "operation": "deserialize", "type": describe_type(BoolType), "bytes": bad, "value": value.val})
        except Exception as exception:
            vectors.append({"source": f"{source}::test_boolean_nominal", "origin": "generated", "operation": "deserialize_invalid", "type": describe_type(BoolType), "bytes": bad, "expected_error": error(exception)})
    for type_class, size in [(I8Type, 1), (I16Type, 2), (I32Type, 4), (I64Type, 8)]:
        total = pow(2, (size * 8) - 1)
        valid("test_int_types_nominal", type_class, [0, -1, 1, -total, total - 1])
        invalid("test_int_types_off_nominal", type_class, [-total - 1, total, -total * 35, total * 35, "1", 1.5])
        short("valid_values_test", type_class, 1)
    for type_class, size in [(U8Type, 1), (U16Type, 2), (U32Type, 4), (U64Type, 8)]:
        max_int = pow(2, (size * 8)) - 1
        valid("test_uint_types_nominal", type_class, [0, 1, max_int - 1, max_int])
        invalid("test_uint_types_off_nominal", type_class, [-1, -2, max_int + 1, max_int * 35, -max_int, "1", 1.5])
        short("valid_values_test", type_class, 1)
    for type_class in (F32Type, F64Type):
        valid("test_float_types_nominal", type_class, [0.31415000557899475, 0.0, -3.141590118408203])
        invalid("test_float_types_off_nominal", type_class, ["1.0", [1.0]])
        short("valid_values_test", type_class, 1.0)

    enum_class = EnumType.construct_type("SomeEnum", {"MEMB1": 0, "MEMB2": 6, "MEMB3": 9})
    valid("test_enum_nominal", enum_class, ["MEMB1", "MEMB2", "MEMB3"])
    invalid("test_enum_off_nominal", enum_class, ["MEMB12", "MEMB22", "MEMB23", 0])
    short("valid_values_test", enum_class, "MEMB2")
    for rep_type in (U8Type, U16Type, I8Type, I16Type, U32Type, I64Type, U64Type):
        sized = EnumType.construct_type(f"SomeEnum{type_name(rep_type)}", {"MEMB1": 0, "MEMB2": 6, "MEMB3": 9}, rep_type)
        valid("test_enum_nominal", sized, ["MEMB1", "MEMB2", "MEMB3"])
    try:  # numeric value that is no member
        enum_class().deserialize(struct.pack(">i", 5), 0)
        raise AssertionError("enum accepted unknown value")
    except Exception as exception:
        vectors.append({"source": f"{source}::test_enum_off_nominal", "origin": "generated", "operation": "deserialize_invalid", "type": describe_type(enum_class), "bytes": struct.pack(">i", 5).hex(), "expected_error": error(exception)})

    string_type = StringType.construct_type("MyFancyString", max_length=10)
    valid("test_string_nominal", string_type, ["ABC123DEF4", "ABC1", "ABC123D", "", "h\u00e9llo"])
    invalid("test_string_off_nominal", string_type, ["ABC123DEF456", 1])
    short("valid_values_test", string_type, "ABC1")
    for bad in (struct.pack(">H", 11) + b"ABC123DEF45", b"\x00"):  # longer than the maximum, truncated length
        try:
            string_type().deserialize(bad, 0)
            raise AssertionError("string accepted bad data")
        except Exception as exception:
            vectors.append({"source": f"{source}::test_string_off_nominal", "origin": "generated", "operation": "deserialize_invalid", "type": describe_type(string_type), "bytes": bad.hex(), "expected_error": error(exception)})

    members = [
        [("member1", U32Type, "%d"), ("member2", U32Type, "%lu"), ("member3", I64Type, "%lld")],
        [
            ("member4", StringType.construct_type("StringMember1", max_length=10), "%s"),
            ("member5", StringType.construct_type("StringMember2", max_length=4), "%s"),
            ("member6", I64Type, "%lld"),
        ],
    ]
    values = [{"member1": 123, "member2": 456, "member3": -234}, {"member4": "345", "member5": "abc1", "member6": 213}]
    for index, (member_list, value) in enumerate(zip(members, values)):
        valid("test_serializable_basic", SerializableType.construct_type(f"BasicSerializable{index}", member_list), [value])
    picky = SerializableType.construct_type(
        "BasicInvalidSerializable",
        [("member1", U32Type, "%d"), ("member2", StringType.construct_type("StringMember1", max_length=10), "%s")],
    )
    invalid("test_serializable_basic_off_nominal", picky, [
        {"member5": 123, "member6": 456}, {"member1": "345", "member2": 123},
        {"member1": 345, "member2": "234", "member3": "Something"}, [1, "a"],
    ])
    short("valid_values_test", picky, {"member1": 1, "member2": "abc"})
    string_member = StringType.construct_type("StringMember", max_length=3)
    enum_member = EnumType.construct_type("EnumMember", {"Option1": 0, "Option2": 6, "Option3": 9})
    array_member = ArrayType.construct_type("ArrayMember", string_member, 3, "%s")
    sub_serializable = SerializableType.construct_type("AdvancedSubSerializable", [("subfield1", U32Type), ("subfield2", array_member)])
    advanced = SerializableType.construct_type("AdvancedSerializable", [
        ("field1", string_member), ("field2", U32Type), ("field3", enum_member), ("field4", array_member), ("field5", sub_serializable),
    ])
    valid("test_serializable_advanced", advanced, [{
        "field1": "abc", "field2": 123, "field3": "Option2", "field4": ["", "123", "6"],
        "field5": {"subfield1": 3234, "subfield2": ["abc", "def", "abc"]},
    }])

    arrays = [
        (("TestArray", I32Type, 2, "%d"), [32, 1]),
        (("TestArray2", U8Type, 4, "%d"), [0, 1, 2, 3]),
        (("TestArray3", StringType.construct_type("TestArrayString", max_length=18), 3, "%s"), ["one", "1234", "1"]),
        (("TestArrayF64", F64Type, 2, "%f"), [1.5, -2.25]),
        (("TestArrayBool", BoolType, 3, "%s"), [True, False, True]),
        (("TestArrayEnum", enum_member, 2, "%s"), ["Option3", "Option1"]),
    ]
    for ctor_args, value in arrays:
        valid("test_array_type", ArrayType.construct_type(*ctor_args), [value])
    picky_array = ArrayType.construct_type("TestArrayPicky", I32Type, 4, "%d")
    invalid("test_array_type_off_nominal", picky_array, [[1, 2, 3], [1, 2, 3, 4, 5, 6], ["one", "two", "three", "four"], 1])
    short("valid_values_test", picky_array, [1, 2, 3, 4])

    times = [("TB_NONE", 1, 100, 999999), ("TB_PROC_TIME", 0xFF, 1234567, 2952), ("TB_WORKSTATION_TIME", 8, 1529430215, 12),
             ("TB_SC_TIME", 231, 1344230277, 123456), ("TB_FPGA_TIME", 78, 10395, 24556), ("TB_DONT_CARE", 0xB3, 12390819, 12356)]
    for base, context, seconds, microseconds in times:
        time = TimeType(TimeType.TimeBase(base), context, seconds, microseconds)
        data = time.serialize()
        decoded = TimeType()
        decoded.deserialize(data, 0)
        assert time_value(decoded) == time_value(time)
        vectors.append({"source": f"{source}::test_time_type", "origin": "generated", "operation": "serialize_deserialize", "type": {"kind": "time"}, "value": time_value(time), "bytes": data.hex(), "max_size": TimeType.getMaxSize()})
    for base, context, seconds, microseconds in [("TB_NONE", 1, 3, -1), ("TB_WORKSTATION_TIME", 1, 700000, 1234567)]:
        try:
            TimeType(TimeType.TimeBase(base), context, seconds, microseconds)
            raise AssertionError("time accepted invalid microseconds")
        except Exception as exception:
            vectors.append({"source": f"{source}::test_time_type", "origin": "generated", "operation": "construct_invalid", "type": {"kind": "time"}, "value": {"base": base, "context": context, "seconds": seconds, "microseconds": microseconds}, "expected_error": error(exception)})
    write(
        "types.json",
        "Serialization of every F Prime type (big-endian). serialize_deserialize: value <-> bytes in both directions. "
        "construct_invalid: the value is rejected. deserialize_invalid: the bytes are rejected. deserialize: bytes -> value only.",
        vectors,
        time_base_enum=TimeType.TimeBase("TB_NONE").ENUM_DICT,
        time_base_representation=type_name(TimeType.TimeBase("TB_NONE").REP_TYPE),
    )
    return len(vectors)


# ---------------------------------------------------------------------------------------------
# Encoders and decoders
# ---------------------------------------------------------------------------------------------
TIME_1 = ("TB_WORKSTATION_TIME", 0, 1533758629, 123456)
TIME_2 = ("TB_WORKSTATION_TIME", 0, 1533758628, 123457)


def make_time(spec):
    return TimeType(TimeType.TimeBase(spec[0]), *spec[1:])


def with_msg_len(vectors, build):
    """Run `build(msg_len type)` for the two message length types the Python tests configure"""
    for msg_len in (U32Type, U16Type):
        ConfigManager().set_config("msg_len", msg_len)
        try:
            vectors.extend(build(msg_len))
        finally:
            ConfigManager()._set_defaults()


def encoder_vectors():
    vectors = []
    source = f"{TESTS}/encoders"

    def channels(msg_len):
        cases = [
            ("test_ch_encoder_1", 101, U32Type, 42, TIME_1, "0001" "00000065" "0002005b6b4ca50001e240" "0000002a"),
            ("test_ch_encoder_2", 102, U16Type, 40, TIME_2, "0001" "00000066" "0002005b6b4ca40001e241" "0028"),
        ]
        for test, identifier, type_class, value, time, body in cases:
            template = ChTemplate(identifier, "test_ch", "test_comp", type_class)
            output = ChEncoder().encode_api(ChData(type_class(value), make_time(time), template))
            expected = msg_len(len(bytes.fromhex(body))).serialize().hex() + body
            assert output.hex() == expected, test
            yield {"source": f"{source}/test_ch_encoder.py::{test}", "origin": "asserted", "operation": "encode_channel", "config": config(msg_len),
                   "input": {"id": identifier, "type": describe_type(type_class), "value": value, "time": time_value(make_time(time))}, "expected": expected}

    def events(msg_len):
        cases = [
            ("test_event_encoder_1", 101, [U32Type, U32Type], [42, 10], TIME_1, "0002" "00000065" "0002005b6b4ca50001e240" "0000002a0000000a"),
            ("test_event_encoder_2", 102, [U8Type, U16Type], [128, 40], TIME_2, "0002" "00000066" "0002005b6b4ca40001e241" "800028"),
        ]
        for test, identifier, types, values, time, body in cases:
            arguments = [(f"a{index + 1}", f"a{index + 1}", type_class) for index, type_class in enumerate(types)]
            template = EventTemplate(identifier, "test_ch", "test_comp", arguments, EventSeverity["DIAGNOSTIC"], "%d %d")
            event = EventData(tuple(type_class(value) for type_class, value in zip(types, values)), make_time(time), template)
            output = EventEncoder().encode_api(event)
            expected = msg_len(len(bytes.fromhex(body))).serialize().hex() + body
            assert output.hex() == expected, test
            yield {"source": f"{source}/test_event_encoder.py::{test}", "origin": "asserted", "operation": "encode_event", "config": config(msg_len),
                   "input": {"id": identifier, "arguments": [{"type": describe_type(t), "value": v} for t, v in zip(types, values)], "time": time_value(make_time(time))}, "expected": expected}

    def packets(msg_len):
        templates = [ChTemplate(101, "test_ch", "test_comp", U32Type), ChTemplate(102, "test_ch2", "test_comp2", U8Type), ChTemplate(103, "test_ch3", "test_comp3", U16Type)]
        values = [1356, 143, 1509]
        time = make_time(TIME_1)
        channel_data = [ChData(template.get_type_obj()(value), time, template) for template, value in zip(templates, values)]
        output = PktEncoder().encode_api(PktData(channel_data, time, PktTemplate(64, "test_pkt", templates)))
        body = "0004" "0040" "0002005b6b4ca50001e240" "0000054c8f05e5"
        expected = msg_len(len(bytes.fromhex(body))).serialize().hex() + body
        assert output.hex() == expected
        yield {"source": f"{source}/test_pkt_encoder.py::test_pkt_encoder", "origin": "asserted", "operation": "encode_packet", "config": config(msg_len),
               "input": {"id": 64, "channels": [{"id": t.get_id(), "type": describe_type(t.get_type_obj()), "value": v} for t, v in zip(templates, values)], "time": time_value(time)}, "expected": expected}

    def commands(msg_len):
        string_type = StringType.construct_type("String_128", 128)
        enum_type = EnumType.construct_type("CmdEnum", {"OFF": 0, "ON": 1}, U8Type)
        array_type = ArrayType.construct_type("CmdArray", U16Type, 3, "%d")
        struct_type = SerializableType.construct_type("CmdStruct", [("x", I16Type), ("label", StringType.construct_type("String_8", 8))])
        cases = [
            ("test_cmd_data.py::CmdDataTest::test_str_bool_command_arg", 42, [BoolType], [True]),
            ("test_cmd_data.py::CmdDataTest::test_str_numerical_command_args", 43, [U32Type, F64Type, I8Type], [12345, 98.765, 127]),
            ("test_cmd_data.py::CmdDataTest::test_str_string_command_args", 44, [string_type], ["Everything is good"]),
            ("(none: command without arguments)", 45, [], []),
            ("(none: one argument of every remaining type)", 46,
             [U8Type, U16Type, U64Type, I16Type, I32Type, I64Type, F32Type, enum_type, array_type, struct_type],
             [255, 65535, 2**64 - 1, -2, -3, -4, 1.5, "ON", [1, 2, 3], {"x": -7, "label": "abc"}]),
        ]
        for test, opcode, types, values in cases:
            template = CmdTemplate(opcode, "TEST_CMD", "PythonTests", [(f"arg{index}", None, t) for index, t in enumerate(types)])
            # CmdData takes arrays and structs as JSON text, like the command line does
            command = CmdData(tuple(json.dumps(v) if isinstance(v, (list, dict)) else v for v in values), template)
            output = CmdEncoder().encode_api(command)
            yield {"source": f"{TESTS}/data_types/{test}" if test.startswith("test") else test, "origin": "generated", "operation": "encode_command", "config": config(msg_len),
                   "input": {"opcode": opcode, "arguments": [{"type": describe_type(t), "value": v} for t, v in zip(types, values)]}, "expected": output.hex()}

    def files(msg_len):
        cases = [
            ("start", StartPacketData(0, 3, "/tmp/local.bin", "/seq/remote.bin"), {"sequence": 0, "size": 3, "source_path": "/tmp/local.bin", "destination_path": "/seq/remote.bin"}),
            ("data", DataPacketData(1, 0, b"\x01\x02\x03"), {"sequence": 1, "offset": 0, "data": "010203"}),
            ("end", EndPacketData(2, 0x55BC801D), {"sequence": 2, "hash": 0x55BC801D}),
            ("cancel", CancelPacketData(3), {"sequence": 3}),
        ]
        for kind, packet, description in cases:
            output = FileEncoder().encode_api(packet)
            yield {"source": f"{TESTS}/gds_cli/file_uplink_test.py (file content 010203; no byte assertion in the Python tests)", "origin": "generated",
                   "operation": "encode_file_packet", "config": config(msg_len), "input": {"packet_type": kind, **description}, "expected": output.hex()}

    for build in (channels, events, packets, commands, files):
        with_msg_len(vectors, lambda msg_len, build=build: list(build(msg_len)))
    write(
        "encoders.json",
        "Ground-side encoders: input object -> bytes. Telemetry encoders (channel, event, packet) produce "
        "length | descriptor | id | time | value(s). Uplink encoders (command, file packet) produce "
        "5a5a5a5a | length | descriptor | body. `config` gives the configured integer types; msg_len is the length prefix type.",
        vectors,
    )
    return len(vectors)


def decoder_vectors():
    vectors = []
    time = make_time(TIME_1)
    time_hex = time.serialize().hex()

    def decode(operation, source, decoder, data, convert, extra):
        vector = {"source": source, "origin": "generated", "operation": operation, "config": config(), **extra, "input": data.hex()}
        try:
            vector["expected"] = convert(decoder.decode_api(data))
        except Exception as exception:
            vector["expected_error"] = error(exception)
        vectors.append(vector)

    # Channels: input is the packet after the length and descriptor
    ch_templates = {101: ChTemplate(101, "test_ch", "test_comp", U32Type), 102: ChTemplate(102, "test_ch2", "test_comp2", U16Type)}
    ch_dictionary = {"dictionary": [{"id": i, "type": describe_type(t.get_type_obj())} for i, t in ch_templates.items()]}
    ch_convert = lambda items: [{"id": item.template.get_id(), "time": time_value(item.time), "value": item.val_obj.val} for item in items]
    ch_source = f"{TESTS}/encoders/test_ch_encoder.py (bytes of the encoder tests, decoded)"
    one = bytes.fromhex("00000065" + time_hex + "0000002a")
    two = bytes.fromhex("00000066" "0002005b6b4ca40001e241" "0028")
    for data in (one, two, one + two, bytes.fromhex("00000067" + time_hex + "00"), one[:-1]):
        decode("decode_channel", ch_source, ChDecoder(ch_templates), data, ch_convert, ch_dictionary)

    # Events
    def event_template(identifier, types):
        return EventTemplate(identifier, "test_ch", "test_comp", [(f"a{i + 1}", f"a{i + 1}", t) for i, t in enumerate(types)], EventSeverity["DIAGNOSTIC"], "%d %d")

    event_types = {101: [U32Type, U32Type], 102: [U8Type, U16Type]}
    event_templates = {identifier: event_template(identifier, types) for identifier, types in event_types.items()}
    event_dictionary = {"dictionary": [{"id": i, "arguments": [describe_type(t) for t in types]} for i, types in event_types.items()]}
    event_convert = lambda items: [{"id": item.template.get_id(), "time": time_value(item.time), "arguments": [arg.val for arg in item.args]} for item in items]
    event_source = f"{TESTS}/encoders/test_event_encoder.py (bytes of the encoder tests, decoded)"
    one = bytes.fromhex("00000065" + time_hex + "0000002a0000000a")
    two = bytes.fromhex("00000066" "0002005b6b4ca40001e241" "800028")
    for data in (one, two, one + two, bytes.fromhex("00000067" + time_hex), one + b"\x00\x00"):
        decode("decode_event", event_source, EventDecoder(event_templates), data, event_convert, event_dictionary)

    # Packetized telemetry
    templates = [ChTemplate(101, "test_ch", "test_comp", U32Type), ChTemplate(102, "test_ch2", "test_comp2", U8Type), ChTemplate(103, "test_ch3", "test_comp3", U16Type)]
    packet_dictionary = {"dictionary": [{"id": 64, "channels": [{"id": t.get_id(), "type": describe_type(t.get_type_obj())} for t in templates]}]}
    packet_decoder = PktDecoder({64: PktTemplate(64, "test_pkt", templates)}, {t.get_id(): t for t in templates})
    decode("decode_packet", f"{TESTS}/encoders/test_pkt_encoder.py (bytes of the encoder test, decoded)", packet_decoder,
           bytes.fromhex("0040" + time_hex + "0000054c8f05e5"), ch_convert, packet_dictionary)

    # File packets: input is the packet after the length and descriptor
    def file_convert(items):
        packet = items[0]
        fields = {"packet_type": packet.packetType.name.lower(), "sequence": packet.seqID}
        if hasattr(packet, "size"):
            fields.update(size=packet.size, source_path=bytes(packet.sourcePath).decode(), destination_path=bytes(packet.destPath).decode())
        if hasattr(packet, "offset"):
            fields.update(offset=packet.offset, data=bytes(packet.dataVar).hex())
        if hasattr(packet, "hashValue"):
            fields.update(hash=packet.hashValue[0])
        return fields

    ConfigManager()._set_defaults()
    header = len(U32Type(0).serialize()) + len(ConfigManager().get_config("msg_len")(0).serialize()) + len(ConfigManager().get_type("FwPacketDescriptorType")(0).serialize())
    file_packets = [StartPacketData(0, 3, "/tmp/local.bin", "/seq/remote.bin"), DataPacketData(1, 0, b"\x01\x02\x03"), EndPacketData(2, 0x55BC801D), CancelPacketData(3)]
    for packet in file_packets:
        decode("decode_file_packet", "(none: the Python tests hold no file packet bytes; bytes of the file encoder, decoded)", FileDecoder(),
               FileEncoder().encode_api(packet)[header:], file_convert, {})

    # Distributor: split a byte stream into length-prefixed messages
    source = f"{TESTS}/distributor/test_distributor.py"
    ConfigManager().set_config("msg_len", U16Type)
    ConfigManager().set_type("FwPacketDescriptorType", U32Type)
    try:
        stream = bytes.fromhex("000e00000004" "6162636465666768696a" "000a00000001" "414243444546" "000f00000002ff")
        leftover, messages = Distributor().parse_into_raw_msgs_api(bytearray(stream))
        assert bytes(leftover).hex() == "000f00000002ff" and [bytes(m).hex() for m in messages] == ["000e000000046162636465666768696a", "000a00000001414243444546"]
        parsed = [Distributor().parse_raw_msg_api(message) for message in messages]
        assert [(p[0], p[1], bytes(p[2]).hex()) for p in parsed] == [(14, 4, "6162636465666768696a"), (10, 1, "414243444546")]
        vectors.append({
            "source": f"{source}::test_distributor", "origin": "asserted", "operation": "split_messages",
            "config": {**config(), "FwPacketDescriptorType": "U32", "msg_len": "U16"}, "input": stream.hex(),
            "expected": {"messages": [{"raw": bytes(m).hex(), "length": p[0], "descriptor": p[1], "body": bytes(p[2]).hex()} for m, p in zip(messages, parsed)], "leftover": bytes(leftover).hex()},
        })

        class Recording:
            def __init__(self):
                self.received = []

            def data_callback(self, data):
                self.received.append(bytes(data).hex())

        distributor, recording = Distributor(), Recording()
        distributor.register("FW_PACKET_TELEM", recording)
        stream = bytes.fromhex("000600010000aabb" "00020001" "000600000002eeff" "000600000001ccdd")
        distributor.on_recv(stream)
        assert recording.received == ["ccdd"]
        vectors.append({
            "source": f"{source}::test_distributor_skips_malformed_messages", "origin": "asserted", "operation": "distribute",
            "config": {**config(), "FwPacketDescriptorType": "U32", "msg_len": "U16"}, "registered_descriptors": {"FW_PACKET_TELEM": 1},
            "input": stream.hex(), "expected": {"delivered": [{"descriptor": 1, "body": "ccdd"}]},
        })
    finally:
        ConfigManager()._set_defaults()
    write(
        "decoders.json",
        "Ground-side decoders: bytes -> objects. Decoder input is the message after the length prefix and the "
        "descriptor. `dictionary` lists the definitions the decoder was given. split_messages and distribute cover the "
        "distributor that cuts the deframed stream into length-prefixed messages and routes them by descriptor.",
        vectors,
    )
    return len(vectors)


# ---------------------------------------------------------------------------------------------
# F Prime framing
# ---------------------------------------------------------------------------------------------
def framing_vectors():
    vectors = []
    source = f"{TESTS}/communication/bridge/test_comm_bridge.py"

    def deframe_all(name, test, framer, config_value, data):
        packets, leftover, discarded = framer.deframe_all(data, no_copy=False)
        vectors.append({"name": name, "source": f"{source}::{test}", "origin": "generated", "operation": "deframe_all", "framer": config_value, "input": data.hex(),
                        "expected": {"packets": [bytes(p).hex() for p in packets], "leftover": bytes(leftover).hex(), "discarded": bytes(discarded).hex()}})

    payloads = [
        ("TestSerialToUdpFramed::test_uart_to_udp", b"telemetry-packet-payload"), ("TestSerialToUdpFramed::test_udp_to_uart", b"command-packet-payload"),
        ("TestSerialToUdpFramed::test_uart_garbage_discarded", b"good-packet"), ("TestSerialToUdpFramed::test_udp_to_uart_unexpected_source_dropped", b"allowed-command"),
        ("TestSerialToUdpFramed::test_uart_to_udp", b""), ("TestSerialToUdpFramed::test_uart_to_udp", bytes(range(256))),
    ]
    for checksum in ("crc32", "fixed"):
        framer_config = {"class": "FpFramerDeframer", "plugin_name": "fprime", "checksum": checksum, "start_word": "deadbeef", "maximum_data_size": FpFramerDeframer.MAXIMUM_DATA_SIZE}
        framer = FpFramerDeframer(checksum)
        frames = []
        for test, payload in payloads:
            framed = framer.frame(payload)
            frames.append(framed)
            packet, leftover, discarded = framer.deframe(framed)
            assert (packet, leftover, discarded) == (payload, b"", b"")
            vectors.append({"name": f"frame_{checksum}_{len(payload)}_bytes", "source": f"{source}::{test}", "origin": "generated", "operation": "frame_deframe", "framer": framer_config, "payload": payload.hex(), "framed": framed.hex()})
        telemetry, good = frames[0], frames[2]
        deframe_all(f"{checksum}_split_frame_first_part", "TestSerialToUdpFramed::test_uart_to_udp_split_frame", framer, framer_config, telemetry[: len(telemetry) // 2])
        deframe_all(f"{checksum}_split_frame_whole", "TestSerialToUdpFramed::test_uart_to_udp_split_frame", framer, framer_config, telemetry)
        deframe_all(f"{checksum}_leading_garbage", "TestSerialToUdpFramed::test_uart_garbage_discarded", framer, framer_config, b"\x00\x01garbage-without-start-word" + good)
        deframe_all(f"{checksum}_garbage_only", "TestSerialToUdpFramed::test_uart_garbage_discarded", framer, framer_config, b"\x00\x01garbage-without-start-word")
        deframe_all(f"{checksum}_three_frames_and_partial", "TestSerialToUdpFramed::test_uart_to_udp", framer, framer_config, telemetry + good + frames[1] + telemetry[:9])
        deframe_all(f"{checksum}_header_only", "TestSerialToUdpFramed::test_uart_to_udp_split_frame", framer, framer_config, telemetry[:8])
        corrupt = bytearray(telemetry)
        corrupt[-1] ^= 0xFF
        deframe_all(f"{checksum}_bad_checksum_then_good", "TestSerialToUdpFramed::test_uart_garbage_discarded", framer, framer_config, bytes(corrupt) + good)
        oversize = struct.pack(">II", 0xDEADBEEF, FpFramerDeframer.MAXIMUM_DATA_SIZE)
        deframe_all(f"{checksum}_length_at_maximum_rejected", "TestSerialToUdpFramed::test_uart_garbage_discarded", framer, framer_config, oversize + good)
    tcp = TcpServerFramerDeframer()
    tcp_config = {"class": "TcpServerFramerDeframer"}
    payload = b"command-packet-payload"
    vectors.append({"name": "tcp_server_frame", "source": "(none: not exercised by the Python tests)", "origin": "generated", "operation": "frame", "framer": tcp_config, "payload": payload.hex(), "framed": tcp.frame(payload).hex()})
    downlink = b"ZZZZ" + struct.pack(">I", len(payload)) + payload
    for name, data in (("tcp_server_deframe", downlink), ("tcp_server_deframe_garbage_two_packets_partial", b"xx" + downlink + downlink + downlink[:10])):
        packets, leftover, discarded = tcp.deframe_all(data, no_copy=False)
        vectors.append({"name": name, "source": "(none: not exercised by the Python tests)", "origin": "generated", "operation": "deframe_all", "framer": tcp_config, "input": data.hex(),
                        "expected": {"packets": [bytes(p).hex() for p in packets], "leftover": bytes(leftover).hex(), "discarded": bytes(discarded).hex()}})
    write(
        "fprime_framing.json",
        "F Prime framing: start word deadbeef | U32 length | payload | U32 checksum (CRC-32 of start word, length and "
        "payload, or the constant cafecafe for checksum `fixed`). frame_deframe: payload <-> framed, both directions. "
        "deframe_all: all packets found in input, the bytes kept for the next read (leftover) and the bytes thrown away (discarded).",
        vectors,
    )
    return len(vectors)


# ---------------------------------------------------------------------------------------------
# Parameter files
# ---------------------------------------------------------------------------------------------
def crc_header(data):
    return ((zlib.crc32(data, 0) ^ 0xFFFFFFFF) & 0xFFFFFFFF).to_bytes(4, "big") + data


def parameter_vectors():
    vectors = []
    source = f"{TESTS}/tools"
    dictionary = SEQUENCES / "resources" / "simple_dictionary.json"
    id_dict, _, _ = PrmJsonLoader(str(dictionary)).construct_dicts(str(dictionary))
    files = {"dictionary": "sequences/resources/simple_dictionary.json"}

    expected_dat = (SEQUENCES / "expected" / "simple_paramdb.dat").read_bytes()
    with tempfile.TemporaryDirectory() as work:
        for output_format in ("dat", "seq"):
            output = Path(work) / f"out.{output_format}"
            convert_json(SEQUENCES / "input" / "simple_paramdb.json", dictionary, output, output_format)
            produced = output.read_bytes()
            if output_format == "dat":
                assert produced == expected_dat
                vectors.append({"source": f"{source}/test_paramdb_gen.py::test_nominal_paramdb", "origin": "asserted", "operation": "encode_dat", **files,
                                "input_file": "sequences/input/simple_paramdb.json", "expected_file": "sequences/expected/simple_paramdb.dat", "expected": produced.hex()})
            else:
                vectors.append({"source": f"{source}/test_paramdb_gen.py::test_nominal_paramdb", "origin": "generated", "operation": "encode_seq", **files,
                                "input_file": "sequences/input/simple_paramdb.json", "expected_text": produced.decode()})
        try:
            convert_json(SEQUENCES / "input" / "simple_bad_paramdb.json", dictionary, Path(work) / "bad.dat", "dat")
            raise AssertionError("bad parameter file accepted")
        except Exception as exception:
            vectors.append({"source": f"{source}/test_paramdb_gen.py::test_failure_paramdb", "origin": "generated", "operation": "encode_dat", **files,
                            "input_file": "sequences/input/simple_bad_paramdb.json", "expected_error": error(exception)})

    params = decode_dat_to_params(expected_dat, id_dict)
    decoded_json = params_to_json(params)
    assert decoded_json == json.loads((SEQUENCES / "input" / "simple_paramdb.json").read_text())
    vectors.append({"source": f"{source}/test_prm_decode.py::test_decode_simple_paramdb", "origin": "asserted", "operation": "decode_dat", **files,
                    "input_file": "sequences/expected/simple_paramdb.dat", "input": expected_dat.hex(),
                    "expected": {"json": decoded_json, "text": params_to_text(params), "csv": params_to_csv(params)}})
    cases = [
        ("test_decode_invalid_delimiter", crc_header(b"\xFF\x00\x00\x00\x12\x00\x00\x11\x01test")),
        ("test_decode_unknown_param_id", crc_header(b"\xA5\x00\x00\x00\x08\xFF\xFF\xFF\xFF\x00\x00\x00\x00")),
        ("test_decode_incomplete_data", crc_header(b"\xA5\x00\x00")),
        ("test_decode_empty_file", crc_header(b"")),
        ("test_decode_file_too_small", b"\x00\x00"),
    ]
    valid = bytearray(crc_header(b"\xA5\x00\x00\x00\x08\x00\x00\x00\x01\x00\x00\x00\x64"))
    valid[0] ^= 0xFF
    cases.append(("test_corrupted_crc_valid_data", bytes(valid)))
    for test, data in cases:
        vector = {"source": f"{source}/test_prm_decode.py::{test}", "origin": "generated", "operation": "decode_dat", **files, "input": data.hex()}
        try:
            decoded = decode_dat_to_params(data, id_dict)
            vector["expected"] = {"json": params_to_json(decoded), "text": params_to_text(decoded), "csv": params_to_csv(decoded)}
        except Exception as exception:
            vector["expected_error"] = error(exception)
        vectors.append(vector)
    write(
        "params.json",
        "Parameter database files (.dat): U32 CRC | records of a5 | U32 record size | U32 parameter id | value. "
        "encode_dat: JSON parameter file -> .dat bytes. encode_seq: JSON parameter file -> command sequence text. "
        "decode_dat: .dat bytes -> JSON, text and CSV renderings, or the error.",
        vectors,
    )
    return len(vectors)


# ---------------------------------------------------------------------------------------------
# Sequences
# ---------------------------------------------------------------------------------------------
# Sequences compiled with simple_dictionary.json by test_seq_parser.py; every other one uses enhanced_dictionary.json
SIMPLE_DICTIONARY_SEQUENCES = {
    "empty_file", "simple_sequence", "valid_relative", "valid_absolute", "valid_arguments", "valid_delimiters", "valid_comments",
    "simple_bad_sequence", "invalid_time_format", "invalid_missing_command", "invalid_time_descriptor", "multiple_errors",
    "invalid_array_unclosed", "invalid_array_missing_comma", "invalid_object_unclosed", "invalid_object_missing_colon",
}
PYTHON_INPUT_DIR = "<fprime-gds>/test/fprime_gds/common/tools/input"


def sequence_manifest():
    """Compile every sequence with the current Python compiler and record what it produces"""
    entries = []
    for sequence in sorted((SEQUENCES / "input").glob("*.seq")):
        name = "simple_dictionary.json" if sequence.stem in SIMPLE_DICTIONARY_SEQUENCES else "enhanced_dictionary.json"
        entries.append(compile_sequence(sequence, name, cont=sequence.stem == "multiple_errors"))
    used = {entry["expected_file"] for entry in entries}
    unused = sorted(f"expected/{path.name}" for path in (SEQUENCES / "expected").iterdir() if f"expected/{path.name}" not in used and path.suffix != ".dat")
    assert not unused, unused
    (SEQUENCES / "manifest.json").write_text(json.dumps({
        "description": "One entry per input sequence, compiled by the Python sequence compiler (generateSequence, timebase 0xffff) "
                       "with the dictionary the Python test uses. result binary: the output is byte-identical to `expected_file`. "
                       "result error: `python_error` is the full message of the Python compiler, with the input directory written as "
                       f"{PYTHON_INPUT_DIR} like the Python tests do; the text of `expected_file` is contained in it. "
                       "`continue_on_error` is the `cont` argument (collect all errors instead of stopping at the first).",
        "timebase": 0xFFFF,
        "sequences": entries,
    }, indent=1) + "\n")
    return len(entries)


def compile_sequence(sequence, dictionary_name, cont):
    expected_dir = SEQUENCES / "expected"
    entry = {"input": f"input/{sequence.name}", "dictionary": f"resources/{dictionary_name}", "continue_on_error": cont}
    with tempfile.TemporaryDirectory() as work:
        output = Path(work) / "out.bin"
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                seqgen.generateSequence(str(sequence), str(output), str(SEQUENCES / "resources" / dictionary_name), 0xFFFF, cont=cont)
        except Exception as exception:
            message = str(exception).replace(str(sequence.parent), PYTHON_INPUT_DIR)
            entry.update(result="error", error_type=type(exception).__name__, python_error=message)
            candidate = expected_dir / f"{sequence.stem}_error.txt"
            if candidate.exists():
                assert candidate.read_text().strip() in message, sequence
                entry["expected_file"] = f"expected/{candidate.name}"
            else:
                assert sequence.stem == "multiple_errors" and "Encountered syntax error" in message
                entry["expected_file"] = None
            return entry
        produced = output.read_bytes()
        candidate = expected_dir / ("simple_expected.bin" if sequence.stem == "simple_sequence" else f"{sequence.stem}.bin")
        assert candidate.read_bytes() == produced, sequence
        entry.update(result="binary", size=len(produced), expected_file=f"expected/{candidate.name}")
        return entry


# ---------------------------------------------------------------------------------------------
# Format strings
# ---------------------------------------------------------------------------------------------
FORMAT_CASES = [
    ("c", "test_format_with_no_issue", "Opcode 0x%04X dispatched to port %d and value %f", (181, 8, 1.234), "Opcode 0x00B5 dispatched to port 8 and value 1.234000"),
    ("c", "test_format_value_with_string_input_as_other_types", "Opcode 0x%04X dispatched to port %u and value %.2f", (181, "8", 1.234), "Opcode 0x00B5 dispatched to port 8 and value 1.23"),
    ("c", "test_format_with_format_spec", "Opcode 0x%04X dispatched to port %04d and value %0.02f", (181, 8, 1.234), "Opcode 0x00B5 dispatched to port 0008 and value 1.23"),
    ("c", "test_format_bad_case", "Opcode 0x%04X dispatched to port %04d and value %0.02f", ("181", "8", "0.123"), None),
    ("c", "test_format_decimal_with_width_flag", "Decimals: %d %ld", (1977, 650000), "Decimals: 1977 650000"),
    ("c", "test_format_preceding_with_blanks", "Preceding with blanks: %10d", (1977,), "Preceding with blanks:       1977"),
    ("c", "test_format_preceding_with_zeros", "Preceding with zeros: %010d", (1977,), "Preceding with zeros: 0000001977"),
    ("c", "test_format_some_different_radices", "Some different radices: %d %x %o %#x %#o", (100, 100, 100, 100, 100), "Some different radices: 100 64 144 0x64 0o144"),
    ("c", "test_format_floats", "floats: %4.2f %+.0e %E", (3.1416, 3.1416, 3.1416), "floats: 3.14 +3e+00 3.141600E+00"),
    ("c", "test_format_asterisk_width", "Width trick: %*d", (5, 10), None),
    ("c", "test_format_regular_string", "%s", ("A string",), "A string"),
    ("c", "test_format_percent_sign", "%.2f%%", (1.23456,), "1.23%"),
    ("c", "test_format_single_value", "%.2f%%", 1.23456, "1.23%"),
    ("c", "test_format_list_value", "%.2f%%, %.2f%%", [1.23456, 1.23456], "1.23%, 1.23%"),
    ("c", "test_format_unsigned_flag_with_length_flag", "Something %lu something %llu something else %lu", (123456, 123457, 123458), "Something 123456 something 123457 something else 123458"),
    ("fpp", "test_fpp_format_with_no_issue", "Opcode 0x{04X} dispatched to port {} and value {f}", (181, 8, 1.234), "Opcode 0x00B5 dispatched to port 8 and value 1.234000"),
    ("fpp", "test_fpp_format_value_with_string_input_as_other_types", "Opcode 0x{04x} dispatched to port {} and value {.2f}", (181, "8", 1.234), "Opcode 0x00b5 dispatched to port 8 and value 1.23"),
    ("fpp", "test_fpp_format_with_format_spec", "Opcode 0x{04X} dispatched to port {04d} and value {0.02f}", (181, 8, 1.234), "Opcode 0x00B5 dispatched to port 0008 and value 1.23"),
    ("fpp", "test_fpp_format_bad_case", "Opcode 0x{04X} dispatched to port {04d} and value {0.02f}", ("181", "8", "0.123"), None),
    ("fpp", "test_fpp_format_decimal_with_width_flag", "Decimals: {} {}", (1977, 650000), "Decimals: 1977 650000"),
    ("fpp", "test_fpp_format_preceding_with_blanks", "Preceding with blanks: {10d}", (1977,), "Preceding with blanks:       1977"),
    ("fpp", "test_fpp_format_preceding_with_zeros", "Preceding with zeros: {010d}", (1977,), "Preceding with zeros: 0000001977"),
    ("fpp", "test_fpp_format_with_escape_sequence", "Some different radices: {} 0x{x} {{ i'm escaped }}", (100, 100, 100, 100, 100), "Some different radices: 100 0x64 { i'm escaped }"),
    ("fpp", "test_fpp_format_regular_string", "{}", ["A string"], "A string"),
    ("fpp", "test_fpp_format_list_value", "{.2f}%, {.2f}%", [1.23456, 1.23456], "1.23%, 1.23%"),
]


def format_vectors():
    vectors = []
    for style, test, template, values, expected in FORMAT_CASES:
        preprocess = preprocess_c_style_format_str if style == "c" else preprocess_fpp_format_str
        vector = {"source": f"{TESTS}/utils/test_string_util.py::TestFormatString::{test}", "origin": "asserted", "operation": "format",
                  "style": style, "template": template, "values": list(values) if isinstance(values, (list, tuple)) else values}
        try:
            actual = format_string_template(preprocess(template), values)
            assert actual == expected, test
            vector["expected"] = actual
        except ValueError as exception:
            assert expected is None, test
            vector["expected_error"] = error(exception)
        vectors.append(vector)
    write(
        "format_strings.json",
        "Event and channel format strings: template (C printf style or FPP style) and values -> text. `values` that is "
        "not a list is a single value.",
        vectors,
    )
    return len(vectors)


# ---------------------------------------------------------------------------------------------
# Dictionary
# ---------------------------------------------------------------------------------------------
def dictionary_expectations():
    """What the Python JSON dictionary loaders build from RefTopologyDictionary.json"""
    path = str(FIXTURES / "dictionary" / "RefTopologyDictionary.json")
    raw = json.loads(Path(path).read_text())
    globals_cleanup()
    try:
        _, commands, versions = CmdJsonLoader(path).construct_dicts(None)
        _, events, _ = EventJsonLoader(path).construct_dicts(None)
        _, channels, _ = ChJsonLoader(path).construct_dicts(None)
        _, parameters, _ = PrmJsonLoader(path).construct_dicts(None)
        _, types, _ = TypeJsonLoader(path).construct_dicts(None)
        assert len(commands) == len(raw["commands"]) and len(events) == len(raw["events"]) and len(channels) == len(raw["telemetryChannels"])
        assert list(versions) == ["TestVersion", "TestVersion"]
        packet_sets = {}
        for packet_set in raw.get("telemetryPacketSets", []):
            _, packets, _ = PktJsonLoader(path).construct_dicts(packet_set["name"], channels)
            packet_sets[packet_set["name"]] = [
                {"name": name, "id": packet.get_id(), "channels": [channel.get_full_name() for channel in packet.get_ch_list()]}
                for name, packet in sorted(packets.items(), key=lambda item: item[1].get_id())
            ]
        assert packet_sets["PacketSet1"][[p["name"] for p in packet_sets["PacketSet1"]].index("SigGen1")]["id"] == 15
        expected = {
            "description": "What the Python JSON dictionary loaders build from RefTopologyDictionary.json. Types use the same "
                           "description as vectors/types.json.",
            "source": f"{TESTS}/loaders/test_json_loader.py",
            "versions": {"framework": versions[0], "project": versions[1]},
            "config_types": {name: describe_type(type_class) for name, type_class in sorted(types.items())},
            "commands": [
                {"name": name, "opcode": command.get_op_code(), "description": command.get_description(),
                 "arguments": [{"name": argument[0], "type": describe_type(argument[2])} for argument in command.get_args()]}
                for name, command in sorted(commands.items(), key=lambda item: item[1].get_op_code())
            ],
            "events": [
                {"name": name, "id": event.get_id(), "severity": event.get_severity().name, "format": event.get_format_str(),
                 "description": event.get_description(),
                 "arguments": [{"name": argument[0], "type": describe_type(argument[2])} for argument in event.get_args()]}
                for name, event in sorted(events.items(), key=lambda item: item[1].get_id())
            ],
            "channels": [
                {"name": name, "id": channel.get_id(), "description": channel.get_ch_desc(), "format": channel.get_format_str(),
                 "type": describe_type(channel.get_type_obj())}
                for name, channel in sorted(channels.items(), key=lambda item: item[1].get_id())
            ],
            "parameters": [
                {"name": name, "id": parameter.get_id(), "type": describe_type(parameter.prm_type_obj), "default": parameter.prm_default_val}
                for name, parameter in sorted(parameters.items(), key=lambda item: item[1].get_id())
            ],
            "packet_sets": packet_sets,
        }
        (FIXTURES / "dictionary" / "RefTopologyDictionary.expected.json").write_text(json.dumps(expected, indent=1) + "\n")
        return {key: len(expected[key]) for key in ("commands", "events", "channels", "parameters", "packet_sets")}
    finally:
        globals_cleanup()


if __name__ == "__main__":
    counts = {
        "dictionary/RefTopologyDictionary.expected.json": dictionary_expectations(),
        "format_strings.json": format_vectors(),
        "types.json": type_vectors(),
        "encoders.json": encoder_vectors(),
        "decoders.json": decoder_vectors(),
        "fprime_framing.json": framing_vectors(),
        "params.json": parameter_vectors(),
        "sequences/manifest.json": sequence_manifest(),
    }
    for name, count in counts.items():
        print(f"{name}: {count}")
