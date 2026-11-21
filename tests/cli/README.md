# Checks of the Python client and of the online subcommands

`tests/cli/run.sh` (run by `scripts/check.sh`, which sets `FGDS`, `REF_BIN`, `DICTIONARY`, `VENV` and `OUT`) does three things.

1. `python/tests/unit`: the parts of `fgds_client` that need no GDS.
2. `ref_int.py`: starts `fgds` and `Ref`, runs the `Ref` integration tests of F Prime (copies in
   `python/tests/ref_int`, changed in their imports only) through the `fprime_test_api` fixture of `fgds_client`.
3. `check.py`: the subcommands of `plugins/online` against `fprime-cli` of the Python GDS.

The `fgds` binary used is `$FGDS`, the stock binary, against the stock `Ref` (CCSDS framing).

## Ref integration tests

| Test | Result |
| --- | --- |
| `test_is_streaming`, `test_send_command`, `test_send_command_args`, `test_send_and_assert_no_op`, `test_bd_cycles_ascending`, `test_active_logger_filter`, `test_signal_generation`, `test_system_resources` | pass |
| `test_cmd_parameter.py`: `test_prm_db_sandbox`, `test_send_parameter` | pass |
| `test_seqgen`, `test_cmd_seq` | skipped: sequences |
| `test_file_uplink` | skipped: file transfer |

`dp_integration_test.py` (data products) is not copied.

## What `check.py` compares

| Part | `fprime-cli` | `fgds` | Required |
| --- | --- | --- | --- |
| Definitions | `events --list`, `channels --list`, `command-send --list`, each with no filter, `-i`, `-c`, `-s`, `-c` and `-s` | `events --list`, `channels --list`, `commands`, same filters | same items in the same order: identifier, name, severity, description; for commands opcode, argument names, types and descriptions |
| Live | `command-send` of the commands of `tests/compare/scenario.py`, to `Ref` | the same | the command packets on the link are equal byte for byte |
| Replay | `events -t 0 --json`, `channels -t 0 --json` while the bytes captured from `Ref` are fed to the Python GDS | `events --json`, `channels --json`, the same bytes fed to `fgds` | every item `fprime-cli` printed is printed by `fgds` with the same identifier, name, severity, time, arguments and text (channels: identifier, name, time, text of the value) |

Also checked for `fgds` alone: an unknown command is refused with near names, a wrong argument count or type is
refused, `--dry-run` prints the packet and records nothing, and following printed exactly what `--history` lists.

## Accepted differences

1. **`fprime-cli` prints fewer items when following.** It polls, and after each item only takes an item with a
   later time, so items that share a time tag or arrive out of time order are skipped (about 10 of 125 channel
   samples in a run). For the same reason it sometimes prints an item twice (seen with CCSDS framing, where
   a frame carries packets of two seconds: three samples were printed twice in one run). `fgds` prints
   every record once. The check requires each item of `fprime-cli` to be among those of `fgds`, not equality.
2. **Order when following.** `fprime-cli` orders by time, `fgds` by arrival. The order is not compared.
3. **`fprime-cli events --json` stops on an event with an enumeration argument** (`not JSON serializable`).
   `patch/sitecustomize.py` is put on its path so that argument values are written by their value; without it
   no event after `PrmFileReadError` could be compared.
4. **Definition lists in JSON.** `fprime-cli --list --json` is not valid JSON for commands, so the command list
   is compared from the text form of both. For events and channels `--list --json` is compared.
5. **Readable time** (`time`): `fprime-cli` prints local time without offset, `fgds` prints UTC with offset.
   The flight time tag (`raw_time`) is compared instead.
6. **Layout and options.** `fgds` has `commands` (in `fprime-cli`: `command-send --list`), `--url` instead of
   the dictionary and ZeroMQ options, `--history`/`--no-follow`, and `--dry-run`. JSON of `fgds` has an extra
   `value` for channels. Without `-t`, `fgds` follows until interrupted, as `fprime-cli -t 0` does.

Not covered: sending raw bytes and file uplink from the command line (no `fprime-cli` counterpart was built).
