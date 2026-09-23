"""Rust-backed variant of the fprime-dp data product CLI.

Mirrors fprime_gds.executables.data_products but delegates decoding and
validation to the fprime_dp_rust extension module (see rust/fprime-dp-rust).
"""

import argparse
import json
import sys
from pathlib import Path

try:
    import fprime_dp_rust
except ImportError:  # pragma: no cover
    fprime_dp_rust = None


def main():
    if fprime_dp_rust is None:
        print(
            "fprime_dp_rust extension not installed. "
            "Build it with: pip install ./rust/fprime-dp-rust (or `maturin develop`)",
            file=sys.stderr,
        )
        return 1

    root_parser = argparse.ArgumentParser(description='Data Product CLI (Rust implementation)')
    subcommands_parser = root_parser.add_subparsers(dest='command')

    decode_parser = subcommands_parser.add_parser('decode', help='Decode a data product binary into a human-readable format')
    decode_parser.add_argument("-b", "--bin-file", required=True, help="Path to input data product binary file (.fdp)")
    decode_parser.add_argument("-d", "--dictionary", required=True, help="Path to F Prime JSON Dictionary")
    decode_parser.add_argument("-o", "--output", required=False, help="Path to output JSON file (defaults to <binFilename>.json)")
    decode_parser.add_argument("-z", "--disable-decompression", action='store_true', help="Disable automatic decompression of data products")

    validate_parser = subcommands_parser.add_parser('validate', help='Validate a data product')
    validate_parser.add_argument("-b", "--bin-file", required=True, help="Path to input data product binary file (.fdp)")
    validate_parser.add_argument("-d", "--dictionary", required=False, help="Path to F Prime JSON Dictionary")
    validate_parser.add_argument("-s", "--header-size", type=int, default=0, help="Use the provided value as the header size for the data product")
    validate_parser.add_argument("-g", "--guess-size", action="store_true", help="Guess at the header size")
    validate_parser.add_argument("-v", "--verbose", action="store_true", help="Verbose output")

    args = root_parser.parse_args()

    if args.command == "decode":
        output = args.output if args.output else str(Path(args.bin_file).with_suffix('.json'))
        print(f"Decoding {args.bin_file}...")
        try:
            result_json = fprime_dp_rust.decode_to_json(
                args.dictionary, args.bin_file, args.disable_decompression
            )
        except ValueError as e:
            print(f"Error: {e}", file=sys.stderr)
            return 1
        with open(output, 'w') as f:
            json.dump(json.loads(result_json), f, indent=2)
        print("Decoding complete!")

    elif args.command == "validate":
        success, stdout_lines, stderr_lines = fprime_dp_rust.validate(
            args.bin_file,
            args.dictionary if args.header_size <= 0 else None,
            args.header_size if args.header_size > 0 else None,
            args.verbose,
        )
        for line in stdout_lines:
            print(line)
        for line in stderr_lines:
            print(line, file=sys.stderr)
        return 0 if success else 1

    return 0


# For debugging
if __name__ == "__main__":
    sys.exit(main())
