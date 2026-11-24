# Handoff

## State
- Design baseline approved by the user on 2026-11-21: `docs/design/requirements-and-architecture.md`.
- First slice built (M0 and the smallest part of M1): the Cargo workspace of section 4.10 with `fgds-dictionary`, `fgds-core`, `fgds-shell`, `fgds`, the plugins `comm-tcp`, `framing-fprime`, `storage-memory`, `events`, `channels`, `commands`, and `examples/custom-gds`.
- `scripts/check.sh` runs every check: format, lints, tests; `fgds` against `Ref` (commands, events, channels, WebSocket replay then live); the example binary; every `tests/*/run.sh`. With `COMPARE=1` it also compares with the Python GDS the bytes sent to flight and the data decoded, live and on replayed bytes (`tests/compare/README.md`), and the binaries of `seqgen` and `prm-write` (`tests/seq`).
- `.github/workflows/check.yml` calls the same script with `COMPARE=1`. It failed on `4778afbd` (run 45389134110) in `tests/files/repeat.py`, which required stock `Ref` to crash when file packets arrive in one piece; `Ref` did not. That assertion is removed: the check now requires that a transfer shown as finished has the whole file on `Ref`. CI has not been seen green since; look at the run of the latest push.
- Where the code differs from the design, and what to change in the design before more plugins are built: `docs/design/deviations.md`. The design document itself is unchanged.

## Checkpoint, end of round 10
- Head of the code: `26607e26` on `devin/1795279000-rust-gds-integration` (round 9 was `4778afbd`). No pull request.
- Verified: one full `FPRIME=~/repos/fprime scripts/check.sh` on `67c69df3`, about 48 minutes. Every part passed; the script ended with exit 1 at "nothing left behind" only because `tests/browser/ui.py` was being edited in the tree (the G3 page check, now `26607e26`). That check was then run alone (`tests/browser/ui.py`, all passed) and the tree is clean with no process left. The full script was not run again on `26607e26`.
- In round 10 (all in "Round 10 batch" below): chunk size against the frame (57) and the F´ flight frame limit; `fgds check` validates as `fgds run` (58); control characters shown visibly in text output; upload source path, 255-byte limit and backslash in names (G1 to G3, page and command line); `seqgen` argument count (F10-2); a refused upload does not block the others (F10-4); Charts wheel zoom text in `deviations.md` (F10-5); shortened and repeated options (G4, to be removed with `argparse.rs`).
- Not verified: UDP against flight software that speaks UDP itself; UART on a real serial device; the Python retry in `tests/cli` (runs only when Python misses a handshake); a deployment with a frame accumulator smaller than 2048 bytes.
- One test of `ref_integration_test.py` (`test_seqgen_complex_args`) failed once in that run because `Ref` did not send an event it printed, and passed on the retry the check makes for that state.

## Cleanup pass
Scope and order are in two reviews made on `4778afbd`, kept by the requesting session: `fgds-python-isms-review.md` (code that exists only to equal the Python GDS; its section 4 order, its section 3 kept) and `fgds-rust-review.md` (3 MUST FIX, 8 SHOULD). The user wants a solid, maintainable GDS, similar to the Python one, not identical. Python comparisons stay only for bytes sent to flight and data decoded. A message that reads sensibly stays; only wording that names Python or its classes is replaced. Wire formats, binary file formats and the dictionary format stay byte-equal for valid input; the byte fixtures are the guard.
- Step 1, done: SEQ-1, decision 7, section 6.1 and MNT-11 reworded in the design.
- Step 2, done. Copied behaviours:
  - U1 (argument count of a sequence statement) was already fixed.
  - U2: a finite literal beyond the float type is refused (`fgds-dictionary` `input.rs`, `fgds-offline` `value.rs`, sequences `parser.rs`); `inf` and `nan` by name are taken. Digits are ASCII only.
  - U3: `command-send` of an unknown command writes to standard error and ends with 1. `events -t` with no event ends with 1. `prm-write` refusals go to standard error.
  - U4: sequence times are computed in integers (microseconds kept); day 366 of a year that has none is refused.
  - U5 to U7 (page): flight time with six digits; flight text shown as it is; `Command String` read by a small parser that keeps numbers as typed (64-bit integers exact); a Boolean has no value until chosen; an empty string is a value; counters are not capped; events are filtered on their text.
  - U8: an upload with no destination and no directory is refused (route, `file-uplink`, `files upload`); `file-uplink` no longer names `/tmp/<user>` as the storage directory.
  - U9: `prm-decode` CSV quotes fields; `file-uplink` prints the state and the reason of a failed transfer.
- Step 2, MUST FIX of the robustness review, each with a test:
  - M1: one queue in sequence order for every record (`crates/fgds-core/src/pipeline.rs`; `a_subscriber_misses_no_record_when_uplink_records_join_a_backlog`).
  - M2: a downlink is written to `<name>.part` and takes its name when it has finished, `<name>.<n>` if the name is taken; a failed or canceled one stays `.part` (`plugins/files/src/downlink.rs`).
  - M3: an uplink is `finished` on the `FileReceived` event of its destination, `failed` with the event as the reason on a refusal of the flight software (`FileOpenError` and the others of `REFUSALS`), and `sent` when nothing confirms it in time (`plugins/files/src/uplink.rs`). The event carries the name cut at the size of its string: `names` takes a cut name of 36 bytes or more as the start of the destination.
- Step 2, SHOULD: S3 (invalid plugin options refused; was already so on this head, now tested), S4 (the listener is bound and every option checked before storage opens; `a_refused_option_of_run_leaves_no_run_directory`), S6 (`scripts/check.sh` without the Python GDS; `COMPARE=1` for the comparison). The checks that pinned the wording, the option reading and the page texts of the Python GDS are deleted: `tests/cli/check.py` is now a short check of the online subcommands against `Ref`; `tests/browser` keeps `ui.py`; `fixtures/command-messages`, `command-string`, `page-texts` are gone.
- Step 3, done (audit ranks 2, 3, 5, 10, 12):
  - Rank 2: `clap` alone. `crates/fgds-core/src/argparse.rs`, `gds_options.rs`, `Subcommand::argparse`, the option tables of the plugins and `fixtures/command-line` are gone. A long option is given in full; the options of `fprime-cli` that did nothing (`--no-zmq`, `--tts-port`, `--hash-file`, `--file-storage-directory` and the others) are refused as unknown. `-d`, `--dictionary`, `-i`, `-c`, `-s`, `-j`, `-t`, `--list` are kept. `honours` stays: a subcommand refuses the options of `fgds run` it does not use.
  - Rank 3: both `pyjson.rs` are gone; `serde_json` reads the JSON text of an array or struct argument (`typed_json` in `crates/fgds-dictionary/src/codec.rs`). An integer is exact over all of I64 and U64. Refused now, with a message naming what was expected and found: a literal beyond 64 bits, `NaN` and `Infinity` (no JSON), `-0` for an integer member. `-0` for a float member is the negative zero.
  - Rank 5: one float writer, `fgds_dictionary::float_text` (shortest text that reads back; `NaN`, `Infinity`, `-Infinity`), and `float_json` (those three as strings: every file written is valid JSON). The JSON writers of `fgds-offline`, `dictionary-tab` and `data-products` are `serde_json`. One formatter is kept, `Spaced` in `crates/fgds-offline/src/value.rs` (`, ` and `: `, ASCII only): a sequence file carries that text in a string argument, so its bytes go to flight and must equal those of `fprime-seqgen` (`tests/seq/differential.py` with `COMPARE=1` found the difference). A float in such a text is written by `serde_json` (`1e16`, Python `1e+16`). `fixtures/float-text` is gone; `tests/dp/fixtures.rs` compares the decoded values, not the bytes of the JSON file.
  - Rank 10: a Boolean is shown `true` / `false`, an array or a struct as JSON (`crates/fgds-dictionary/src/text.rs`); white space around an argument is ASCII white space.
  - Rank 12: required and exclusive arguments of `dp` are `clap` declarations; the message of a `.dat` file that ends early no longer quotes `struct.unpack_from`.
  - Not taken, with the reason: `complete_types` (`crates/fgds-offline/src/lib.rs`) stays, `fixtures/sequences/resources/enhanced_dictionary.json` needs it. `python_path` (`plugins/data-products/src/cli.rs`) stays, it only tidies `./` and `//` in a printed name. The Python wording in `crates/fgds-offline/src/convert.rs` and `value.rs` (`python_str`, `str_repr`, `Type <class 'int'>`, the `Int` of any size) belongs to rank 4; `plugins/online/src/items.rs` builds its JSON line by hand (rank 9); `Json` of data products stays an enum of its own (rank 7).
  - `docs/design/deviations.md` still has rows on the options and wordings removed in this step; it is rewritten in step 5 (rank 11).
- Steps 4 and 5 wait for the user's confirmation: one encoder, data products on the shared dictionary types, hand-written sequence parser; simpler Commanding page, CLI output from the record, parity harness reduced to bytes and data, `deviations.md` as a short list for someone coming from fprime-gds.

## Pass criterion (user, round 9)
- The Python GDS is the reference only where it is correct or neutral: do not copy its bugs, and do not reproduce its checks 1-to-1. A difference is a defect of fgds only if (a) data shown or stored or bytes sent to `Ref` differ for an input both should accept, (b) fgds accepts what a sensible tool refuses or refuses what it should accept, (c) a Python feature, control or option is missing or does something else. Wording, exit code values among non-zero ones, the order of reported errors and the timing of validation marks are recorded only. The head of "Differences from the Python GDS" in `docs/design/deviations.md` states it; the rows are in five groups.

## How to run
- `FPRIME=<fprime checkout with fprime-venv> scripts/check.sh`. The first run builds `Ref` under `target/ref` (several minutes). `COMPARE=1` adds the comparison with the Python GDS, which must be installed in that virtual environment.
- `Ref` is built twice: as it is (CCSDS framing, the main build of `scripts/check.sh` and of every `tests/*/run.sh`, under `target/ref-ccsds`) and with F Prime framing (the second profile: `scripts/build-ref.sh` switches a copy of fprime from `ComCcsds` to `ComFprime`, under `target/ref`). Both are compared with the Python GDS.
- `cargo run -p fgds -- run --dictionary <RefTopologyDictionary.json>`, then `Ref -a 127.0.0.1 -p 50000`; the API is on `http://127.0.0.1:5000/api/v1`. Records go to `logs/<start time>/gds.db`; `--database <file>` keeps one file across runs, `--storage memory` keeps nothing.
- `cargo run -p fgds -- run --replay <file>` serves an old database without a link.

## Since the first slice
- Core amendments (section 4.11 of the design, rewritten to match): decoder and encoder factories, options named `--<plugin>-<name>`, `raw.sent` records, the simple storage failure path (STO-9 and section 7 edited), a reserved queue for uplink records, sequence number reservation, read-only open. The list of changed signatures is in the "Core amendments" section of `docs/design/deviations.md`.
- Review of the slice (`rust-gds-review.md`): must-fix 1 to 5 and should-fix 6 to 22 are fixed, each with a test; decisions and what is still not built are in the "Review fixes, SQLite storage" section of `docs/design/deviations.md`.
- `plugins/storage-sqlite` is the default storage: every run in one file with its own dictionary, reopening, sequence numbers never reused, read-only and `fgds run --replay <database>`, raw link bytes in `logs/<start time>/`. `storage-memory` stays for tests and as the example of a replaced storage.
- `tests/storage/run.sh` (run by `scripts/check.sh`) kills the GDS during a `Ref` run and reopens, restarts with a changed dictionary, and replays the file read-only.
- `fgds_core::testing::conformance` has `storage_conformance_with` and `persistence_conformance` for a storage that outlives the process.
- `plugins/online`: `command-send`, `events` and `channels` are `clap` commands; `--dictionary` or `-d` names the dictionary in which names are looked up and from which the listings are made. `tests/cli/check.py` checks them against `Ref`.
- `fixtures/vectors/format_strings.json` holds 16 formatter vectors recorded from the Python GDS (`origin: recorded`) beside the 25 asserted ones.

## Round 10 batch
- 57: `max_packet` (`crates/fgds-core/src/link.rs`) derives the largest packet from the framers of the send direction; `FramingLayer::limit` says what sets it. `chunk_fits` (`plugins/files/src/lib.rs`) refuses a chunk size above it in the option check and in the upload route, and knows no framing. F´ framing refuses any frame above `--framing-fprime-flight-frame-size` (whole frame bytes; default from the dictionary constant `ComFprimeConfig.BuffMgr.frameAccumulatorSize`, else 2048). `tests/files/chunk.py`, both framings against `Ref`.
- Control characters: `fgds_core::visible` (`crates/fgds-core/src/text.rs`) is applied to every text written by a subcommand (`out!`/`err!` of each plugin, `Item::printed` in `plugins/online`), to the standard error of `fgds` (`crates/fgds/src/cli.rs`) and to the text logs. `-j`, storage and the API stay exact; filters read the decoded text. A new text output needs its line in `tests/cli/visible.py`. Rules in `docs/design/deviations.md`, "Round 10".
- 58: `preflight` (`crates/fgds-core/src/runtime.rs`) is the one start-up validation of `fgds run` and `fgds check`. A plugin validates its options in `reg.add_option_check`; a new plugin option needs its row in `tests/cli/options.py`.
- Charts `Load`: a channel that is not a number is named in its own line. `seqgen` and dictionaries with sections missing: table in `docs/design/deviations.md`, "Round 10".
- File names of an uplink (G1 to G3): the start packet carries `<store>/fprime-uplink/<name>` (`store` of `POST /files/uplink`, sent by `fgds file-uplink` as `fprime-cli` names it) or the name in the uplink directory of the GDS; the route has no `source`, the operator's directory is never sent, and the 255-byte limit is on the sent path (`plugins/files/src/routes.rs`, `upload_name` and `source_path` in `plugins/files/src/lib.rs`). `tests/cli/check.py` and `tests/files/check.py` read the source path of the start packets.
- Options (G4): every subcommand takes the last of a repeated option (`args_override_self` in `crates/fgds/src/cli.rs`).
- Third web verifier of round 10: a sequence statement with a wrong number of arguments is refused with line and command (`record` in `plugins/sequences/src/lib.rs`, also the Sequences tab; `tests/seq/differential.py` sets these statements aside and reverses `visible` before comparing messages); on the Uplink tab a file the GDS refuses (422, 413) is listed as refused and the others are sent (`upload` in `plugins/files/ui/uplink.js`); the start packet source path is checked on every link of `fgds run` (`capabilities` in `tests/cli/check.py`).

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
- SHOULD items of `fgds-rust-review.md` not taken:
  - S1: a storage that hangs throttles live delivery (each batch waits the commit time-out) and the queue grows without bound.
  - S2: a history query with a field or text filter has no row limit, and all reads share one connection.
  - S7: the link listens on `0.0.0.0:50000`; the adapter writes `sitting well` into the command link every 0.5 s; downlinked files go to `/tmp/<user>/fprime-downlink` (a plugin is not told the run directory: this needs a field in `PluginConfig`, a core interface).
  - S8: `charts`, `sequences` and the uplink handler do not declare the kinds and routes they need.
- The uplink and downlink handlers have unit tests of their decisions only (`seen`, `keep`); the transfers are covered by `tests/files` against `Ref`.

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

## Checks of the page
`tests/browser/run.sh` (part of `scripts/check.sh`) runs `ui.py`: the page of fgds in a browser, against `Ref`.
## Next steps
1. Review `docs/design/deviations.md` and update the design ("To change in the design").
2. Then one branch per plugin crate, per the milestones of section 6.3.
