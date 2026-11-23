# Handoff

## State
- Design baseline approved by the user on 2026-11-21: `docs/design/requirements-and-architecture.md`.
- First slice built (M0 and the smallest part of M1): the Cargo workspace of section 4.10 with `fgds-dictionary`, `fgds-core`, `fgds-shell`, `fgds`, the plugins `comm-tcp`, `framing-fprime`, `storage-memory`, `events`, `channels`, `commands`, and `examples/custom-gds`.
- `scripts/check.sh` runs every check and passes locally: format, lints, tests; `fgds` against `Ref` (commands, events, channels, WebSocket replay then live); the comparison with the Python GDS, live and on replayed bytes (`tests/compare/README.md` lists the accepted differences); the example binary.
- `.github/workflows/check.yml` calls the same script. It passes on this branch.
- Where the code differs from the design, and what to change in the design before more plugins are built: `docs/design/deviations.md`. The design document itself is unchanged.

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
- `fixtures/vectors/format_strings.json` holds 16 formatter vectors recorded from the Python GDS (`origin: recorded`) beside the 25 asserted ones.

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
- Charts: zoom, pan, `Reset Zoom`, `Lock Scale`, collapse.
- Sequences: `Build` panel, editor with completion.
- Composite channel values as an expandable table.
- Dashboard tab (milestone M6).
- Advanced tab: polling intervals, `event_buffer_size`, `command_buffer_size`, `response_object_limit`, `compact_commanding`, the statistics tables and the error list with `Clear Errors` (`channels_display_last_received` is built).

## Checks of the page against the Python page
`tests/browser/run.sh` (part of `scripts/check.sh`) compares with texts and frames recorded from the Python GDS 4.4.0 page, under `fixtures/`:
- `command-messages`: the messages of the argument fields when a form is shown, before and after `Send Command` (`messages.py`).
- `command-string`: lines typed in `Command String` (`line.py`), lines that give part of the values a form holds (`held.py`), refused then taken lines each followed by the send button, with the frames (`refused.py`: 15 sequences; a line that leaves arguments out is applied only to the command the form shows and then sent as Python sends it, such a line for another command sends nothing and leaves that command's form alone; the send button clicked with no Enter (a click that makes the page read a whole line of another command than the one shown sends nothing for any argument type, the next click sends; the Python page sends there only when its button stays under the pointer, row R6-3); enum constants in another case, the box rewritten to the constant also when the form already held it; boolean texts; a name alone).
- `page-texts`: buttons, headers, placeholders, labels, hover texts (`title`, `alt`, `aria-label` of every element; that of a list entry or a table cell by its kind), other texts and the window title in 34 states (each tab without and with `Ref`, the top bar, eight command forms, a send with no `Ref`, Charts with help, a chart and a channel); each one-sided text is under a row T1 to T19 of `docs/design/deviations.md` (`texts.py`; `texts.py record` reads the Python page again).
- `parity.py` runs both pages on the same bytes: rows, sorting, filters, `channels_display_last_received`.
Each script has a `record` mode to write its fixture again from a running Python page.
The address of the page names the tab as the Python page does (`#Channels`).

## Next steps
1. Review `docs/design/deviations.md` and update the design ("To change in the design").
2. Then one branch per plugin crate, per the milestones of section 6.3.
