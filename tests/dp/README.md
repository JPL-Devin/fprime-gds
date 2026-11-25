# Data product checks

`tests/dp/run.sh` is run by `scripts/check.sh` and can be run alone from the repository root.

| Step | What is compared | Ground truth |
|---|---|---|
| 1 | `cargo test -p data-products`: `fixtures.rs` (library on every product and mode), `index.rs` (watched directory), `robustness.rs` (truncated, corrupted and random inputs never panic) | `fixtures/dp/expected` |
| 2 | `fgds dp` on 18 products x 10 modes and on the 22 cases of `cli_cases.json`: exit code, decoded values, files written | `fixtures/dp/expected` |
| 3 | A running GDS with a watched directory: tab and assets, list, duplicate content, symbolic link, invalid product, export, missing and changed files | `fixtures/dp/expected` for the exported JSON |

Steps 2 and 3 run `$FGDS`, the stock binary.

## Comparison rules

`fgds dp decode` writes the decoded values: a scalar is its value, an array a list, a struct an object of its members. `fprime-dp` writes each value with a description of its type (`{"value": ..., "type": ...}`, `{"values": [...], "value_type": ...}`, the format and description of each struct member, the Python class of a record). The checks take the values out of the `fprime-dp` document and compare them with the `fgds` document; the name of a record type is not compared (`U32` here, `U32Type` there).

What else is not compared:

- The text of an error: both tools must refuse the same products, each in its words. `fgds` checks the data checksum before it reads the records.
- `--help` output and the text of a refused command line.
- The API detail route (`GET /api/v1/data-products/<hash>`) gives integers beyond 2^53 as text; the export route gives the file `fgds dp decode` writes.
