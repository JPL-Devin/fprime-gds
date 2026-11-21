"""Writes table.json: doubles (as their 64 bits in hexadecimal) and the text Python gives for them with repr().
Half of them are F32 values widened to F64, as the Python GDS holds an F32. Run: python3 generate.py > table.json"""
import json
import random
import struct

random.seed(7)
values = [0.0, -0.0, 1.0, -1.0, 0.1, 1e16, 1e-4, 9.999e-5, 9999999999999998.0, 1e22, 1e23, 5e-324, 1.7976931348623157e308,
          123456789.0, 1.5e-7, 2.5, 100.0, float("inf"), float("-inf"), float("nan")]
# F32 values whose 17-digit text is a tie (finding: 1.1888198852539062).
values += [struct.unpack(">f", struct.pack(">f", v))[0] for v in (1.1888198852539062, 3.4e38, -3.4e38, 0.1, 1.1, 9.25)]
for _ in range(3000):
    values.append(struct.unpack(">f", struct.pack(">I", random.getrandbits(32)))[0])
for _ in range(1500):
    values.append(struct.unpack(">f", struct.pack(">f", random.uniform(-10, 10)))[0])
for _ in range(1500):
    values.append(struct.unpack(">d", struct.pack(">Q", random.getrandbits(64)))[0])
for _ in range(500):
    values.append(random.uniform(-1e6, 1e6))
for value in values:
    print(json.dumps({"bits": struct.pack(">d", value).hex(), "text": repr(value)}))
