# Checks of the Python client and of the online subcommands

`tests/cli/run.sh` (run by `scripts/check.sh`, which sets `FGDS`, `REF_BIN`, `DICTIONARY`, `VENV` and `OUT`) does three things.

1. `python/tests/unit`: the parts of `fgds_client` that need no GDS.
2. `ref_int.py`: starts `fgds` and `Ref`, runs the `Ref` integration tests of F Prime (the files of
   `$FPRIME/TestDeploymentsProject/Ref/test/int`, unchanged) through the `fprime_test_api` fixture of `fgds_client`.
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
| Definitions | `events --list`, `channels --list`, `command-send --list`, text and `--json`, each with no filter, `-i`, `-c`, `-s`, filters that match nothing, `-c` and `-s`; `-i abc` | the same subcommands and filters | the exit code and the printed text are equal, byte for byte (for `-i abc`: the last line after the program name) |
| Live | `command-send` of the commands of `tests/compare/scenario.py`, to `Ref` | the same | the command packets on the link are equal byte for byte |
| Replay | `events -t 0 --json`, `channels -t 0 --json` while the bytes captured from `Ref` are fed to the Python GDS | `events --json`, `channels --json`, the same bytes fed to `fgds` | every item `fprime-cli` printed is printed by `fgds` with the same identifier, name, severity, time, arguments and text (channels: identifier, name, time, text of the value) |

Also checked for `fgds`: an unknown command gives the text and exit code 0 of `fprime-cli` with the near names of
Python `difflib`; no command name and an argument such as `-3.4e38` end with the error and exit code 2 of
`fprime-cli`; a wrong argument count or type is refused; `--dry-run` prints the packet and records nothing;
following printed exactly what `--history` lists.

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
4. **Command list in JSON.** `fprime-cli command-send --list --json` prints a Python `repr` (single quotes,
   `<class 'abc.Ref.Choice'>`), which no JSON reader accepts. `fgds` prints JSON with the same keys and values,
   types by name. Only the text form is compared.
5. **Readable time** (`time`): both print it without a zone; `fprime-cli` in the local time of the machine,
   `fgds` in UTC. Equal where the machine runs in UTC, as here.
6. **Refused command lines.** The error line is the one of `fprime-cli` after the program name (`fgds: error: ...`
   against `fprime-cli events: error: ...`), with the same exit code; the usage text before it is not printed.
   An argument that both refuse (out of range, wrong type) gives exit code 1 on both: a Python traceback there,
   one line here.
7. **Layout and options.** `fgds` has `commands` (in `fprime-cli`: `command-send --list`), `--url` instead of
   the dictionary and ZeroMQ options, `--history`/`--no-follow`, and `--dry-run`. Without `-t`, `fgds` follows
   until interrupted, as `fprime-cli -t 0` does.

Not covered: sending raw bytes and file uplink from the command line (no `fprime-cli` counterpart was built).
