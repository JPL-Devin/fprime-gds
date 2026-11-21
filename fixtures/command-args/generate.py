#!/usr/bin/env python3
"""What the Python GDS does with a command argument given as text.

Each line of table.json is one text given to `CmdData.convert_arg_value` for one type, then serialized
as the Python GDS serializes it: {"type", "text", "bytes"} with the hexadecimal bytes sent, or null when
the Python GDS refuses the text. Run with the Python of the F Prime virtual environment:

    python fixtures/command-args/generate.py > fixtures/command-args/table.json
"""
import json
import sys

from fprime_gds.common.data_types.cmd_data import CmdData
from fprime_gds.common.models.serialize import bool_type, numerical_types

TYPES = {
    "U8": numerical_types.U8Type, "U16": numerical_types.U16Type, "U32": numerical_types.U32Type,
    "U64": numerical_types.U64Type, "I8": numerical_types.I8Type, "I16": numerical_types.I16Type,
    "I32": numerical_types.I32Type, "I64": numerical_types.I64Type, "F32": numerical_types.F32Type,
    "F64": numerical_types.F64Type, "bool": bool_type.BoolType,
}
INTEGERS = [
    "0", "7", "-0", "+7", "-7", "007", "00", "0_0", "-00", "0b101", "0B101", "0o17", "0O17", "017", "0x10", "0X1f", "-0x10",
    "+0x10", "0x", "0b", "0b102", "0o18", "0xg", " 4 ", "\t4\n", "4 4", "", " ", "1_000", "1__000", "_1", "1_", "0x_ff",
    "0x__ff", "0b_1", "1.0", "1.5", "1e3", "abc", "0x1.8", "--1", "+-1", "- 1", "١٢", "１２", "true", "0x7F", "0x80",
    "0xFF", "0x100", "127", "128", "-128", "-129", "255", "256", "-1", "32767", "32768", "-32768", "-32769", "65535",
    "65536", "2147483647", "2147483648", "-2147483648", "-2147483649", "4294967295", "4294967296",
    "0xFFFFFFFF", "0x100000000", "9223372036854775807", "9223372036854775808", "-9223372036854775808",
    "-9223372036854775809", "18446744073709551615", "18446744073709551616", "0xFFFFFFFFFFFFFFFF",
    "0x10000000000000000", "-0x8000000000000000", "0b11111111", "0b100000000", "0o377", "0o400",
    "340282366920938463463374607431768211456",
]
FLOATS = [
    "0", "-0", "1", "1.5", "-1.5", "+1.5", ".5", "5.", ".", "1e3", "1E3", "1e+3", "1e-3", "1e", "e3", "-3.4e38", "3.4e38",
    "3.4028235e38", "3.4028236e38", "3.40282356779733661637539395458142568448e38", "3.5e38", "1e39", "-1e39",
    "1e308", "1.7976931348623157e308", "1e309", "-1e309", "1e-46", "1e-400", "inf", "-inf", "+inf", "Inf", "INF",
    "infinity", "Infinity", "-Infinity", "infinit", "nan", "NaN", "-nan", "+nan", "NAN", " 1.5 ", "\t1.5\n", "1 .5",
    "", " ", "abc", "1.5f", "0x10", "0x1p3", "1_000.5", "1__0", "_1", "1_", "1_.5", "1._5", "1e1_0", "1e_1", "١.٥",
    "-0.001", "0.1", "1,5", "--1", "+-1", "1.1888198852539062", "true",
]
BOOLS = ["true", "false", "True", "FALSE", "yes", "no", "YES", "No", " true ", "\tno\n", "1", "0", "", "y", "n", "on",
         "off", "t", "tru e"]

for name, texts in [(n, INTEGERS) for n in TYPES if n[0] in "UI"] + [("F32", FLOATS), ("F64", FLOATS), ("bool", BOOLS)]:
    for text in texts:
        try:
            instance = TYPES[name]()
            CmdData.convert_arg_value(text, instance)
            instance.val = instance.val  # the check the Python GDS makes before it serializes
            sent = instance.serialize().hex()
        except Exception:  # every refusal: the Python GDS sends nothing
            sent = None
        sys.stdout.write(json.dumps({"type": name, "text": text, "bytes": sent}, ensure_ascii=False) + "\n")
