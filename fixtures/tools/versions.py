#!/usr/bin/env python3
"""Print the versions the fixtures were generated with. Usage: versions.py <python gds checkout>"""
import platform
import subprocess
import sys
from importlib.metadata import distributions

commit = subprocess.run(["git", "-C", sys.argv[1], "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
print(f"fprime-gds commit: {commit}")
print(f"python: {platform.python_version()}")
for name, number in sorted({d.metadata["Name"].lower(): d.version for d in distributions()}.items()):
    if name != "fprime-gds":  # installed from the checkout above
        print(f"{name}: {number}")
