# Checks of the Python client and of the online subcommands

`tests/cli/run.sh` (run by `scripts/check.sh`, which sets `FGDS`, `REF_BIN`, `DICTIONARY`, `VENV` and `OUT`) does five things.

1. `python/tests/unit`: the parts of `fgds_client` that need no GDS.
2. `ref_int.py`: starts `fgds` and `Ref`, runs the `Ref` integration tests of F Prime (the files of
   `$FPRIME/TestDeploymentsProject/Ref/test/int`, unchanged) through the `fprime_test_api` fixture of `fgds_client`.
3. `check.py`: the subcommands of `plugins/online` and `file-uplink` against `Ref`: what they print, where, and
   the exit code (a command sent; an unknown command, a literal beyond the type and a missing argument refused;
   the listings; `events -t` with no event; an uplink confirmed, refused by `Ref`, and without a destination).
4. `options.py`: `fgds check` and `fgds run` refuse the same plugin option values.
5. `visible.py`: no text output carries a control character of the data.

The `fgds` binary used is `$FGDS`, the stock binary, against the stock `Ref` (CCSDS framing).

## Ref integration tests

| Test | Result |
| --- | --- |
| `test_is_streaming`, `test_send_command`, `test_send_command_args`, `test_send_and_assert_no_op`, `test_bd_cycles_ascending`, `test_active_logger_filter`, `test_signal_generation`, `test_system_resources` | pass |
| `test_cmd_parameter.py`: `test_prm_db_sandbox`, `test_send_parameter` | pass |
| `test_seqgen`, `test_cmd_seq` | skipped: sequences |
| `test_file_uplink` | skipped: file transfer |

`dp_integration_test.py` (data products) is not copied.

A test that fails is run once more, alone, only when `ref.log` has events printed during that test whose
flight times are in no received packet (`Ref` drops events of a burst now and then, with the Python GDS
too); `ref_int.py` prints how many and the first one. Any other failure stands.
