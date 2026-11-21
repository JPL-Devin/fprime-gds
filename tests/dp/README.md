# Data product checks

`tests/dp/run.sh` is run by `scripts/check.sh` and can be run alone from the repository root.

| Step | What is compared | Ground truth |
|---|---|---|
| 1 | `cargo test -p data-products`: `fixtures.rs` (library on every product and mode), `index.rs` (watched directory), `robustness.rs` (truncated, corrupted and random inputs never panic) | `fixtures/dp/expected` |
| 2 | `fgds dp` on 18 products x 10 modes and on the 22 cases of `cli_cases.json`: exit code, stdout, stderr, files written | `fixtures/dp/expected` |
| 3 | A running GDS with a watched directory: tab and assets, list, duplicate content, symbolic link, invalid product, export, missing and changed files | `fixtures/dp/expected` for the exported JSON |
| 4 | `fgds dp` and a live `fprime-dp` on 648 damaged products (seeded), only when `FPRIME_DP` is set | `fprime-dp` of the commit in `fixtures/README.md` |

Until the plugin is in `default_plugins()`, steps 2 to 4 run `plugins/data-products/examples/fgds-dp.rs`, which is the stock GDS with the plugin added. `run.sh` uses `$FGDS` as soon as it has the `dp` subcommand.

## Comparison rules

Decoded JSON is compared twice: as parsed values (`NaN`, `Infinity` and `-Infinity`, which Python writes and strict JSON does not have, compare by name), and as bytes. Both hold for every fixture, so no value is normalised.

What is not compared, or is rewritten before comparing:

- The scratch directory in stdout and stderr is replaced by `<WORKDIR>`, as `fixtures/README.md` does.
- For an argument error (exit code 2), only the message after `: error: ` is compared. The usage text before it is written by `argparse` for `fprime-dp` and names a different program.
- `--help` output is not compared (not in the fixtures).
- Step 4 only: when `fprime-dp` ends with a Python traceback (an exception it does not catch, on some damaged inputs), the exit code and stdout are compared and the stderr text is not. When it reports a fault of the product (`Error: ...`, checksum messages), stderr is compared in full.
- The API detail route (`GET /api/v1/data-products/<hash>`) returns strict JSON, so a non-finite float is the string `"NaN"`, `"Infinity"` or `"-Infinity"` there. The export route returns the `fprime-dp` bytes.

In step 4 every damaged product is rejected by both tools (648 of 648), so that step compares failures only; decoded output is compared in steps 1 to 3.
