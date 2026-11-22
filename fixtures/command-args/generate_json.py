#!/usr/bin/env python3
"""What the Python GDS does with a number written in the JSON text of a struct argument, and with the same
number given as the text of a scalar argument.

Each line of json-table.json is {"type", "member", "text", "bytes"}: for a member, `text` is the JSON text
given for `Ref.ScalarStruct` (the argument of `Ref.typeDemo.SEND_SCALARS`) to `CmdData.convert_arg_value`,
every other member being 0; with "member" null, `text` is the text of a scalar argument of the type. "bytes"
is what the Python GDS serializes, in hexadecimal, or null when it refuses. Run with the Python of the
F Prime virtual environment:

    python fixtures/command-args/generate_json.py > fixtures/command-args/json-table.json
"""
import json
import sys

from fprime_gds.common.data_types.cmd_data import CmdData
from fprime_gds.common.models.serialize import numerical_types
from fprime_gds.common.models.serialize.serializable_type import SerializableType

MEMBERS = ["i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64"]
TYPES = {name: getattr(numerical_types, name.upper() + "Type") for name in MEMBERS}
STRUCT = SerializableType.construct_type("Ref.ScalarStruct", [(name, TYPES[name], "{}", "") for name in MEMBERS])

BOUNDS = [2**7, 2**8, 2**15, 2**16, 2**31, 2**32, 2**53, 2**63, 2**64, 2**127, 2**128, 10**38, 10**39, 2**1023,
          2**1024, 2**1024 - 2**970, 2**1024 - 2**970 - 1, 10**308, 10**309, 10**400]
INTEGERS = sorted({sign * (bound + step) for bound in BOUNDS for step in (-1, 0, 1) for sign in (1, -1)} | {0, 1, 5})
FLOATS = ["0.0", "5.0", "1.5", "-1.5", "1e2", "1E2", "3.4e38", "3.4028235e38", "3.4028236e38", "3.5e38", "1e39",
          "1e308", "1.7976931348623157e308", "1.7976931348623159e308", "1e309", "1e400", "1e-400", "5e-324",
          "9223372036854775808.0", "18446744073709551616.0", str(10**39) + ".0", str(10**309) + ".0"]
FLOATS += ["-" + text for text in FLOATS if not text.startswith("-")]
OTHERS = ["NaN", "Infinity", "-Infinity", "+Infinity", "-NaN", "nan", "inf", "true", "false", "null", '"5"', '"1.5"',
          '"abc"', "[5]", "{}", '{"\\u0000int": "5"}', '{"\\u0000int": "18446744073709551616"}', "05", "+5", "5.",
          ".5", "0x10", "1_0", "-0", "-0.0", "- 5"]


def sent(convert):
    try:
        instance = convert()
        instance.val = instance.val  # the check the Python GDS makes before it serializes
        return instance.serialize().hex()
    except Exception:  # every refusal: the Python GDS sends nothing
        return None


def member(name, literal):
    text = "{" + ", ".join(f'"{m}": {literal if m == name else 0}' for m in MEMBERS) + "}"

    def convert():
        instance = STRUCT()
        CmdData.convert_arg_value(text, instance)
        return instance
    return {"type": name.upper(), "member": name, "text": text, "bytes": sent(convert)}


def scalar(name, text):
    def convert():
        instance = TYPES[name]()
        CmdData.convert_arg_value(text, instance)
        return instance
    return {"type": name.upper(), "member": None, "text": text, "bytes": sent(convert)}


for name in MEMBERS:
    for literal in [str(i) for i in INTEGERS] + FLOATS + OTHERS:
        sys.stdout.write(json.dumps(member(name, literal)) + "\n")
    for text in [str(i) for i in INTEGERS] + FLOATS:
        sys.stdout.write(json.dumps(scalar(name, text)) + "\n")
