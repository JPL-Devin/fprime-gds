# Handoff

## State
- Design baseline approved by the user on 2026-11-21: `docs/design/requirements-and-architecture.md`.
- First slice built (M0 and the smallest part of M1): the Cargo workspace of section 4.10 with `fgds-dictionary`, `fgds-core`, `fgds-shell`, `fgds`, the plugins `comm-tcp`, `framing-fprime`, `storage-memory`, `events`, `channels`, `commands`, and `examples/custom-gds`.
- `scripts/check.sh` runs every check and passes locally: format, lints, tests; `fgds` against `Ref` (commands, events, channels, WebSocket replay then live); the comparison with the Python GDS, live and on replayed bytes (`tests/compare/README.md` lists the accepted differences); the example binary.
- `.github/workflows/check.yml` calls the same script. It passes on this branch.
- Where the code differs from the design, and what to change in the design before more plugins are built: `docs/design/deviations.md`. The design document itself is unchanged.

## Pass criterion (user, round 9)
- The Python GDS is the reference only where it is correct or neutral: do not copy its bugs, and do not reproduce its checks 1-to-1. A difference is a defect of fgds only if (a) data shown or stored or bytes sent to `Ref` differ for an input both should accept, (b) fgds accepts what a sensible tool refuses or refuses what it should accept, (c) a Python feature, control or option is missing or does something else. Wording, exit code values among non-zero ones, the order of reported errors and the timing of validation marks are recorded only. The head of "Differences from the Python GDS" in `docs/design/deviations.md` states it; the rows are in five groups.
- Checks follow it: `tests/cli/check.py` option matrix compares accept-or-refuse, packets and listings (not the exit code value or message of a refusal, argparse's exit 2 apart); `--hash-file ''` is refused by fgds. What is already equal text for text (argparse port, page messages) is kept, not extended.

## How to run
- `FPRIME=<fprime checkout with fprime-venv> scripts/check.sh`. The first run builds `Ref` under `target/ref` (several minutes).
- `Ref` is built twice: as it is (CCSDS framing, the main build of `scripts/check.sh` and of every `tests/*/run.sh`, under `target/ref-ccsds`) and with F Prime framing (the second profile: `scripts/build-ref.sh` switches a copy of fprime from `ComCcsds` to `ComFprime`, under `target/ref`). Both are compared with the Python GDS.
- `cargo run -p fgds -- run --dictionary <RefTopologyDictionary.json>`, then `Ref -a 127.0.0.1 -p 50000`; the API is on `http://127.0.0.1:5000/api/v1`. Records go to `logs/<start time>/gds.db`; `--database <file>` keeps one file across runs, `--storage memory` keeps nothing.
- `cargo run -p fgds -- run --replay <file>` serves an old database without a link.

## Since the first slice
- Core amendments (section 4.11 of the design, rewritten to match): decoder and encoder factories, options named `--<plugin>-<name>`, `raw.sent` records, the simple storage failure path (STO-9 and section 7 edited), a reserved queue for uplink records, sequence number reservation, read-only open. The list of changed signatures is in the "Core amendments" section of `docs/design/deviations.md`.
- Review of the slice (`rust-gds-review.md`): must-fix 1 to 5 and should-fix 6 to 22 are fixed, each with a test; decisions and what is still not built are in the "Review fixes, SQLite storage" section of `docs/design/deviations.md`.
- `plugins/storage-sqlite` is the default storage: every run in one file with its own dictionary, reopening, sequence numbers never reused, read-only and `fgds run --replay <database>`, raw link bytes in `logs/<start time>/`. `storage-memory` stays for tests and as the example of a replaced storage.
- `tests/storage/run.sh` (run by `scripts/check.sh`) kills the GDS during a `Ref` run and reopens, restarts with a changed dictionary, and replays the file read-only.
- `fgds_core::testing::conformance` has `storage_conformance_with` and `persistence_conformance` for a storage that outlives the process.
- `plugins/online`: `command-send`, `events` and `channels` read their command line with `fgds_core::argparse` and one table of the GDS options of `fprime-cli` (`gds_options`); `gds_options_hold` checks what those options name before anything is done, a `--list` included, and gives the dictionary that `--dictionary` or `-d` names, in which names are looked up and from which the listings are made. `tests/cli/check.py` has the option matrix through both command lines against `Ref`; the rows are in `docs/design/deviations.md`.
- `fixtures/vectors/format_strings.json` holds 16 formatter vectors recorded from the Python GDS (`origin: recorded`) beside the 25 asserted ones.

## Round 10 batch
- 57: `max_packet` (`crates/fgds-core/src/link.rs`) derives the largest packet from the framers; `chunk_fits` (`plugins/files/src/lib.rs`) refuses a chunk size above it in the option check and in the upload route. `tests/files/chunk.py`, both framings against `Ref`. The F´ `Ref` takes only 2023 (its 2048-byte frame buffer, unknown to the GDS): listed.
- 58: `preflight` (`crates/fgds-core/src/runtime.rs`) is the one start-up validation of `fgds run` and `fgds check`. A plugin validates its options in `reg.add_option_check`; a new plugin option needs its row in `tests/cli/options.py`.
- Charts `Load`: a channel that is not a number is named in its own line. `seqgen` and dictionaries with sections missing: table in `docs/design/deviations.md`, "Round 10".

## Round 9 batch
- Built: `send-raw`; `file-uplink`, `seqgen`, `prm-decode`, `prm-write` on the option table of `command-send` (`-d` included); `merge-dictionary`; Charts zoom, pan, `Reset Zoom`, `Lock Scale`, collapse; `--no-page`; the `comm-uart` plugin (`--comm uart`). `--comm tcp-client` and `--bind` were there and are now checked.
- Listed in `docs/design/deviations.md`, group 5: `fprime-comm-bridge` (no bridge program in the tree; about one and a half days), `udp-fast`, single framing stages, `--output-unframed-data`. Group 1: Python `send-raw --no-zmq` cannot work (cause in the row).
- No option is accepted and ignored (F3): a subcommand names the options of `fgds run` it honours (`Subcommand::honours`), the others are refused with exit 2 and left out of its help. A new subcommand or option needs a line in `EFFECT` of `tests/cli/check.py` (`effects`), which tries every long option on every subcommand and fails for one that is neither refused nor shown to change the outcome.
- `file-uplink --file-uplink-chunk-size`, `--file-uplink-cooldown`: per file, through `chunk` and `cooldown` of `POST /files/uplink`.
- Round 9 findings (after the verdicts): Charts `Load` (every selector shows its channel; an unknown name is reported and plots nothing); `events -j` (`args` is a list, 64-bit integers and non-finite floats are numbers); page `Command String` with more arguments than the command takes is refused with the text of the command line (54); the `comm-udp` plugin (`--comm udp`, 56).
- Uploaded file names (N9-1): the copy on the GDS host is named by the transfer id, the operator's name is only the destination and is sent as given. `flight_path` (`plugins/files/src/lib.rs`) refuses a destination or source over 255 UTF-8 bytes, empty, with a control character, or ending in `.`/`..`; a destination outside ASCII gives `notice` in the answer of `POST /files/uplink`, shown by the page and the command line.
- A dictionary string that is not valid UTF-8 is decoded lossy (`fgds-dictionary`, `codec.rs`): `Ref` cuts `FileReceived`'s file name at 40 bytes, possibly inside a character, and the event was dropped as `gds.decode_error` before.
- `comm-tcp` sets `TCP_NODELAY` on the link socket (both modes). Without it the kernel held a small packet back while the previous one was not acknowledged: with packets sent a few ms apart, the bytes left 19 to 33 ms after the `raw.sent` record, sometimes two or three packets in one segment, and `tests/cli/check.py` (which reads the link as `send-raw` returns) failed now and then. Measured over 20 runs x 11 packets with `tcpdump`: 9 of 220 late before, 0 of 220 after.
- Seen once in `tests/cli/check.py`, not ours: Python's `fprime-cli file-uplink --file-uplink-cooldown 0.5` did not return within the time limit of the check. A second run passed. Noted only.
- The three full passes of `scripts/check.sh` are to be run once the round-9 findings are in.

## Known gaps
- Storage: no schema migration, no integrity check at open, no database size or free space in the status, no retention (table "Not built for storage" in `docs/design/deviations.md`).
- Table "Not built in this slice" of `docs/design/deviations.md`: `cargo deny`, macOS job, text logs, `--disable-plugin` and `--framing-layers` on the command line, fuzz targets, and others.
- The UI is the page shell and the tabs of the first slice; it does not offer a choice of run.

## Rules for every session working here
- The design document is the reference. A change to a core interface (section 4.2 to 4.5) or to a requirement goes through the orchestrating session, not into a feature branch on its own.
- Ground truth (section 6.1): data seen when running the `Ref` deployment must match what the current `fprime-gds` reports, and parsers are tested against the fixtures in the Python `fprime-gds` repository (`test/fprime_gds/`).
- Work on a branch created from this one. Do not open pull requests and do not write to any other repository.
- Scope for now is a local run: no tokens, roles or sign-in (section 3.9, deferred list).

## Decisions
Section 7 of the design document.

## Deferred
Access control, the stall watchdog, the load test, project rules checked before a send, `pip install` packaging, a dedicated repository.

## Open work on the page
Controls of the Python page that fgds does not have (`docs/design/deviations.md`, "Controls of the Python page"). None changes bytes sent to `Ref`.
- Paging buttons (`First` `<` `>` `Last`), position field and scroll lock of the tables (Commanding history, Events, Channels, Uplink, Downlink, Dictionaries); sorting of the Commanding history and Dictionaries tables.
- Light/dark switch.
- Sequences: `Build` panel, editor with completion.
- Composite channel values as an expandable table.
- Dashboard tab (milestone M6).
- Advanced tab: polling intervals, `event_buffer_size`, `command_buffer_size`, `response_object_limit`, `compact_commanding`, the statistics tables and the error list with `Clear Errors` (`channels_display_last_received` is built).

## Checks of the page against the Python page
`tests/browser/run.sh` (part of `scripts/check.sh`) compares with texts and frames recorded from the Python GDS 4.4.0 page, under `fixtures/`:
- `command-messages`: the messages of the argument fields when a form is shown, before and after `Send Command` (`messages.py`).
- `command-string`: lines typed in `Command String` (`line.py`), lines that give part of the values a form holds (`held.py`), refused then taken lines each followed by the send button, with the frames (`refused.py`: 15 sequences; a line that leaves arguments out is applied only to the command the form shows and then sent as Python sends it, such a line for another command sends nothing and leaves that command's form alone; the send button clicked with no Enter (a click that makes the page read a whole line of another command than the one shown sends nothing for any argument type, the next click sends; the Python page sends there only when its button stays under the pointer, row R6-3); enum constants in another case, the box rewritten to the constant also when the form already held it; boolean texts; a name alone).
- `page-texts`: buttons, headers, placeholders, labels, hover texts (`title`, `alt`, `aria-label` of every element; that of a list entry or a table cell by its kind), other texts and the window title in 34 states (each tab without and with `Ref`, the top bar, eight command forms, a send with no `Ref`, Charts with help, a chart and a channel); a report since round 9: unlisted one-sided texts are printed as notes, and only a button or table header of the Python page that is missing and not listed fails (`texts.py`; rows T1 to T19 of `docs/design/deviations.md`; `texts.py record` reads the Python page again).
- `parity.py` runs both pages on the same bytes: rows, sorting, filters, `channels_display_last_received`.
Each script has a `record` mode to write its fixture again from a running Python page.
The address of the page names the tab as the Python page does (`#Channels`).

## Next steps
1. Review `docs/design/deviations.md` and update the design ("To change in the design").
2. Then one branch per plugin crate, per the milestones of section 6.3.
