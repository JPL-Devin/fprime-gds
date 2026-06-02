import argparse
import json as js
from pathlib import Path

from fprime_gds.common.loaders.prm_json_loader import PrmJsonLoader
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.common.tools.params import (
    convert_json,
    decode_dat_to_params,
    params_to_json,
    params_to_text,
    params_to_csv,
)


def main():
    root_parser = argparse.ArgumentParser(description="F Prime Parameter Database CLI")
    subcommands_parser = root_parser.add_subparsers(dest="command")

    # encode subcommand with dat/seq sub-subcommands
    encode_parser = subcommands_parser.add_parser(
        "encode", help="Encode parameter JSON files into binary .dat or .seq files"
    )
    encode_subparsers = encode_parser.add_subparsers(dest="format", required=True)

    dat_parser = encode_subparsers.add_parser(
        "dat", help="Compiles .json files into param DB .dat files"
    )
    dat_parser.add_argument(
        "json_file", type=Path, help="The .json file to turn into a .dat file"
    )
    dat_parser.add_argument(
        "-d", "--dictionary", type=Path, required=True,
        help="Path to F Prime JSON Dictionary",
    )
    dat_parser.add_argument(
        "--defaults", action="store_true",
        help="Include default parameter values in the output",
    )
    dat_parser.add_argument(
        "-o", "--output", type=Path, help="Path to output file"
    )

    seq_parser = encode_subparsers.add_parser(
        "seq", help="Converts .json files into command sequence .seq files"
    )
    seq_parser.add_argument(
        "json_file", type=Path, help="The .json file to turn into a .seq file"
    )
    seq_parser.add_argument(
        "-d", "--dictionary", type=Path, required=True,
        help="Path to F Prime JSON Dictionary",
    )
    seq_parser.add_argument(
        "--defaults", action="store_true",
        help="Include default parameter values in the output",
    )
    seq_parser.add_argument(
        "--save", action="store_true",
        help="Include PRM_SAVE commands in the output",
    )
    seq_parser.add_argument(
        "-o", "--output", type=Path, help="Path to output file"
    )

    # decode subcommand
    decode_parser = subcommands_parser.add_parser(
        "decode", help="Decode binary parameter database .dat files into readable formats"
    )
    decode_parser.add_argument(
        "dat_file", type=Path, help="The .dat file to decode"
    )
    decode_parser.add_argument(
        "-d", "--dictionary", type=Path, required=True,
        help="Path to F Prime JSON Dictionary",
    )
    decode_parser.add_argument(
        "-f", "--format", type=str, choices=["json", "text", "csv"],
        default="json", help="Output format (default: json)",
    )
    decode_parser.add_argument(
        "-o", "--output", type=Path, help="Path to output file"
    )

    args = root_parser.parse_args()

    if args.command is None:
        root_parser.print_help()
        return 1

    # Validate inputs before loading the dictionary
    if not args.dictionary.exists():
        print("Unable to find", args.dictionary)
        return 1

    if args.command == "encode":
        if args.json_file is None or not args.json_file.exists():
            print("Unable to find", args.json_file)
            return 1
    elif args.command == "decode":
        if args.dat_file is None or not args.dat_file.exists():
            print("Unable to find", args.dat_file)
            return 1

    # Load dictionary into ConfigManager
    Dictionaries.load_dictionaries_into_config(str(args.dictionary.resolve()))

    if args.command == "encode":
        output_format = args.format
        if args.output is None:
            output_path = args.json_file.with_suffix("." + output_format)
        else:
            output_path = args.output

        if not hasattr(args, "save"):
            args.save = False

        convert_json(
            args.json_file, args.dictionary, output_path,
            output_format, args.defaults, args.save,
        )

    elif args.command == "decode":

        output_format = args.format
        if args.output is None:
            output_path = args.dat_file.with_suffix("." + output_format)
        else:
            output_path = args.output

        print("Decoding", args.dat_file, "to", output_path,
              "(format: ." + output_format + ")")
        output_path.parent.mkdir(parents=True, exist_ok=True)

        dict_parser = PrmJsonLoader(str(args.dictionary.resolve()))
        id_dict, name_dict, versions = dict_parser.construct_dicts(
            str(args.dictionary.resolve())
        )

        dat_bytes = args.dat_file.read_bytes()
        params = decode_dat_to_params(dat_bytes, id_dict)

        if output_format == "json":
            output_data = params_to_json(params)
            output_content = js.dumps(output_data, indent=4)
        elif output_format == "text":
            output_content = params_to_text(params)
        elif output_format == "csv":
            output_content = params_to_csv(params)
        else:
            raise RuntimeError("Invalid output format " + str(output_format))

        print("Done, writing to", output_path.resolve())
        output_path.write_text(output_content)

    return 0


# For debugging
if __name__ == "__main__":
    import sys
    sys.exit(main())
