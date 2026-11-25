# Deviations from the design

Where the code differs from `requirements-and-architecture.md`, and what someone coming from the Python
`fprime-gds` should know. Nothing here changes a requirement or a decision of section 7.

## Coming from fprime-gds

`fgds` does the work of `fprime-gds` and its tools (`fprime-cli`, `fprime-seqgen`, `fprime-prm-write`,
`fprime-prm-decode`, `fprime-dp`) for the same flight software. It is not a copy: where the two differ, the list
below says how. What is kept equal, and checked against the Python GDS by `COMPARE=1 scripts/check.sh`: the bytes
sent to the flight software (frames, command packets, file packets), the binary files (sequence, parameter
database), and the data decoded from the bytes received (events, channels, files, data products).

**Running it**

- One process, `fgds run`, with an HTTP API and a web page; no ZeroMQ, no separate `comm` process. The tools are
  subcommands: `fgds command-send`, `events`, `channels`, `commands`, `file-uplink`, `seqgen`, `prm-write`,
  `prm-decode`, `dp decode`, `dp validate`. The online ones reach a running GDS with `--url`.
- The options are those of `fgds <subcommand> --help`. The options of `fprime-cli` that choose its transport
  (`--no-zmq`, `--zmq-transport`, `--tts-port`, `--tts-addr`) and the hidden options of `fprime-gds` do not exist.
- `fgds repl` is an interactive prompt on a running GDS ([repl.md](../repl.md)); `fgds tui` is the same prompt on
  a screen of three panes ([tui.md](../tui.md)). `fprime-cli` has neither.
- Records are stored (SQLite): a page that is loaded again, or a second window, shows the history.
- Not built: the Dashboard tab, the polling and buffer settings of the Advanced tab, `fprime-comm-bridge`, the
  `udp-fast` adapter, `--communication-selection none`.

**Stricter than the Python tools**

- A command with the wrong number of arguments, a string longer than its type in bytes, a float beyond F32, a
  number with a fraction for an integer: refused, nothing is sent. `command-send` with the link down is an error.
- A sequence with a day the year does not have, a float beyond a double, or two values run together is refused.
- `dp decode` checks the data checksum before it reads the records.
- A command the GDS refuses has a history row with the outcome `rejected` and the reason.
- A file uplink is `finished` when the flight software says it received the file, not when the last packet left.
- Event text is shown as text: HTML in a string is never interpreted.

**Output that differs**

- Messages, usage texts and exit codes are those of `fgds` and `clap`. A refusal is one line, not a traceback.
- Readable times on the command line are UTC.
- `events --json` and `channels --json` print the record of the API; `commands --json`, `events --list --json`
  and `channels --list --json` print the definition of the dictionary. The severity of an event is its name
  (`ACTIVITY_HI`).
- Arrays and structs are written as JSON (`["ONE","BLUE"]`, `{"x":1}`) wherever a value is shown.
- `dp decode` writes the values: a scalar is its value, an array a list, a struct an object. `fprime-dp` wraps
  each in a description of its type; `record_type` (the Python class) is not written, `record_type_name` is
  the FPP name (`U32`).
- `prm-decode` writes the values in the shape `prm-write` reads, for arrays and structs too; type names are FPP
  names (`U32`, `string`).
- `seqgen` messages are `<file>:<line>:<column>: syntax error: unexpected ...`.
- The Commanding page sends what is typed and shows the answer of the GDS under the form; an input left empty
  is marked. The history shows the value sent.
- The page has its own layout and classes: text buttons on charts, a plain text area for sequences, one log per
  record kind, hexadecimal identifiers in the Dictionaries tab, a Data Products tab, a ground time column.

**Seen in the checks, not differences of fgds**

- `Ref` now and then prints an event it does not send; a `Ref/test/int` test that waits for it fails with
  either GDS.
- The uplinker of the Python GDS can miss the handshake of a start packet, and stock `Ref` can abort under its
  file uplink; `tests/files` starts that run again (up to 5 times).

## Interfaces of section 4

| # | The design says | The code does | Why |
|---|---|---|---|
| 1 | `Encoder` has `request_kind` and `encode` (4.3) | Adds `record_kind()`; `encode` returns `Encoded { packet, id, body }` | The uplink service must publish a record of a registered kind for every send (4.5: request `command`, record `fprime.command`). Nothing in the design links the two names |
| 2 | `reg.add_*` shown without a result; `Adapter` and `Handler` registered as objects | `add_*` return `Result` and the conflict names both plugins (CUS-10). Adapters and handlers are registered as factories: `Fn(&PluginConfig) -> Result<Box<dyn Adapter>>`, `Fn(&PluginConfig) -> Box<dyn Handler>` | A conflict must be reported at registration; `register` does no I/O and has no configuration yet, and an adapter is rebuilt after a panic (4.4) |
| 3 | `Manifest`: "name, version, options, record kinds required" | `options: Vec<OptionSpec { name, help, default, secret }>`; each becomes a command-line option and a configuration key | The design names the content but gives no type |
| 4 | `Uplink::send_packet(bytes, client)` (4.3 `Context`) | `send_packet(bytes)`, and no record is published; `dry_run` is a separate method | No kind exists for a raw send in this slice (`raw` encoder is not built). See "To change in the design" |
| 5 | `POST /api/v1/raw` goes through an encoder and is recorded (4.5, 4.7) | Sends the packet framed, answers the byte count, records nothing | Same as 4 |
| 6 | `RunInfo` fields not listed | Has `first_seq`, the first number the pipeline may assign | STO-14 needs storage to tell the pipeline where numbering continues |
| 7 | `KindSpec.text_fields`: "fields searched" | They are JSON pointers into the body (`/display_text`) | Text search must not depend on a field being declared filterable |
| 8 | Storage failure (4.2, STO-9): 2 s commit time-out, memory buffer of 100,000 records, retry with back-off, buffer written on recovery | A failed `append` sets storage to `failed`; its records are broadcast at once with `stored = false` and are not kept. A commit that does not finish in 2 s counts as failed (seen once on a loaded machine: `storage failed: records are delivered unstored error=commit did not finish in 2000 ms`, then `storage recovered; these records were not stored first_seq=702 last_seq=737`). The next successful append publishes `gds.storage_gap {first_seq, last_seq}` and `gds.storage_recovered`. The records of such a gap are shown live by the page and the followers and are absent once the store is opened again or a page reloads. No buffer or retry | Memory storage cannot fail; the full path belongs with `storage-sqlite` (M1). The record kinds and the `stored` flag are as designed |
| 9 | Hand-off queue with a part reserved for the uplink service; commits of at most 100 ms or 500 records (4.2) | One queue of 1,000; batches of at most 500, no timer (a batch is whatever is waiting); no reserved part | No commit is slow enough in memory to need them; to add with SQLite |
| 10 | A handler that overflows its queue resumes from storage (4.2) | Implemented in `RecordStream::next`; the WebSocket uses `next_strict`, which ends with the last `seq` (API-6) | Not a deviation in behaviour: two methods instead of one |
| 11 | `ApiClient`: "HTTP and WebSocket client to a running GDS" | A struct holding the base URL | No online subcommand exists yet (M3); `reqwest` is not linked into the binary until one does |
| 12 | `Routes.access: Vec<RouteAccess>` | Present, not enforced | Access control is deferred (decision 2) |
| 13 | `UiBundle.files: &'static dyn EmbeddedFiles` with rust-embed | `EmbeddedFiles` is a one-method trait; `StaticFiles(&[(path, bytes)])` built with `include_bytes!` implements it | Three small files; rust-embed can implement the same trait later |
| 14 | Memory storage is the plugin `storage-memory` | The type lives in `fgds_core::testing` (the fake context needs it without a plugin dependency); the plugin registers it under `memory` | Avoids a dependency from the core's test doubles on a plugin crate |
| 15 | `GET /api/v1/dictionary?run=`, `records?run=` | `run` is accepted; memory storage holds the current run only | One run per process without a database |
| 16 | Start order (4.4) includes child applications; read-only mode; the `panic = "abort"` warning | Not built | Not in this slice |
| 17 | Record API filters "plus field filters" (4.7) | `filter=<field>:<op>:<value>` (ops `eq ne lt le gt ge contains`), also `before_seq`, `from`, `to` | The design gives no syntax |
| 18 | Stream messages not specified | `{"type":"record","record":{..}}`, and `{"type":"lagged","last_seq":".."}` before closing a slow client | API-6 needs the last `seq` to be sent |

## Dependencies outside section 5

| Crate | Where | Why |
|---|---|---|
| `tracing-subscriber` | `fgds` | `tracing` alone prints nothing; this writes the log |
| `rustyline` (features `with-file-history`, `custom-bindings`) | `repl` only | Line editing, history and Tab completion of `fgds repl`; the binding keeps characters that would not be shown as they are out of the line |
| `ratatui` (with its `crossterm` backend) | `repl` only | The panes and the keys of `fgds tui` |
| `nix` (feature `term`) | `repl` only | Reads and puts back the terminal modes when `fgds repl` ends on a signal; already in `Cargo.lock` through `rustyline` |
| `unicode-width` | `repl` only | The number of columns of a character, for folding and for the cursor; already in `Cargo.lock` through `rustyline` and `ratatui` |
| `futures-util` | `fgds-shell` tests only | Reading a WebSocket in tests with `tokio-tungstenite` |

The four crates of `repl` take `Cargo.lock` from 209 to 295 packages, 86 more (3 of them, `radix_trie`, `nibble_vec` and `endian-type`, for the key bindings of `rustyline`), of which 26 are `windows*` and `winapi*` targets; none is used by another crate, but `repl` is in the stock plugin list, so the stock binary carries them.
They are accepted (MNT-5) and now listed in section 5 of the design, without a cargo feature; their licences are MIT, Apache-2.0 or BSL-1.0 as read by the reviewer of `c381880f`. `fgds repl` (`src/prompt.rs`, `rustyline`, `nix`) and `fgds tui` (`src/tui.rs`, `ratatui`) share the rest of the crate and do not use each other: either is removed by deleting its file, its registration line in `src/lib.rs` and its dependencies.

`reqwest` and `tokio-tungstenite` are in section 5; here they are development dependencies of `fgds-shell` only.
`serde_json` uses its `preserve_order` feature so arguments keep their declaration order.

## Decisions taken without asking

1. **`Ref` was built with F Prime framing** in the first slice. Superseded: `Ref` is now also built as it is
   (section "CCSDS framing"); the F Prime framing variant stays as the second configuration of the checks.
2. **Kind names** are `fprime.event`, `fprime.channel`, `fprime.command`, as in the examples of 4.2 and 4.5.
   Telemetry packets (type 4) are decoded by the `channels` plugin into one `fprime.channel` record per channel,
   as the Python GDS does.
3. **Several items per packet.** A telemetry packet (type 1) from `Ref` holds several channel samples, and the
   Python event decoder accepts several events per packet. Both decoders return one record per item; `raw` of
   each record is its own slice of the packet.
4. **Display text is produced in the decoder** and stored in the body (`display_text`), so that old runs
   show what was shown (STO-5).
   Format strings of array elements and struct members are not applied yet; the Python GDS applies them.
5. **Formatting** is `rustfmt` at 120 columns (`rustfmt.toml`), to keep the trait-heavy code readable.
6. **Comparison harness** uses the Python `StandardPipeline` directly rather than the integration test API,
   and the HTTP API directly rather than the Python client (M3). Accepted differences: `tests/compare/README.md`.
7. **The browser test** of M0 (the example tab appears in a browser) is replaced by a check that `/ui/tabs`
   lists the tab and that the page shell and the tab module are served. Playwright is not set up.

## To change in the design before other plugins are built

1. Add `record_kind` to `Encoder` (or have `Encoded` carry the kind), and say that `Encoded` holds the packet,
   the identifier and the record body.
2. Define the kind and body of a raw send (`raw.sent`?) so `send_packet` and `POST /api/v1/raw` are recorded;
   file uplink (M3) depends on it.
3. State that adapters and handlers are registered as factories, and that `add_*` return `Result`.
4. Give `Manifest` and `OptionSpec` their fields, and say how an option name maps to the command line, the
   configuration file and `PluginConfig`.
5. Add `first_seq` to `RunInfo`, and say that `text_fields` are JSON pointers.
6. Specify the filter syntax of the record API and the messages of the stream.
7. Say where display text is produced (decoder, stored).
8. Say that a decoder may return several records for one packet and how `raw` is split.
9. `Decoder::decode` gets no access to configuration or status; a decoder that needs an option (CCSDS
   application identifiers) has to be built by a factory, like adapters.
10. `Routes.router` is `axum::Router<Context>`: every route of a plugin shares one `Context`. This worked for
    three plugins and the example; keep it.

## Core amendments (after the review of the first slice)

Section 4.11 of the design was rewritten to match; this lists what was decided on the way.

- The storage failure path is the simple form: time-out, `stored = false`, gap and recovery records. STO-9 asks for a buffer of 100,000 records written when storage recovers; that is not built, so records published while storage is failed are lost from the database. This changes STO-9 and is the user's decision to confirm.
- The gap is reported when storage accepts a batch again, not when records are dropped; while storage is failed the gap so far is in `/api/v1/status` (`storage.gap`).
- `Storage::reserve` exists and the pipeline calls it, but no storage implements it yet (`storage-sqlite` will). The raw link log (`<logs>/<run>/raw.log`) is written only when `logs` is set; the default is unset until `storage-sqlite` needs the directory.
- `--read-only` is a core option: storage is opened without starting a run, the link and the handlers are not started, and publishing is refused. Memory storage has nothing to open read-only and says so.
- `raw` of `fprime.event` no longer includes the packet type, and `raw` of a channel of a telemetry packet is rebuilt as identifier, time tag, value, so one rule decodes every record of a kind.
- Encoders are factories too, so `add_encoder` takes the request kind as an argument: the kind must be claimed at registration, before the encoder exists.
- The option of `comm-tcp` is `address`: `--comm-tcp-address`, `[plugins.comm-tcp] address`.
- Plugin options are global arguments like the core options, so a plugin subcommand accepts them.
- `fixtures/vectors/format_strings.json` was not extended with the Python tool; the new cases (negative `x`/`o`, `g`, non-finite values, element and member format strings) are unit tests whose expected text was taken from the review and from `formatted_val` of the Python GDS.
- Not done from the review list: nothing else was skipped; the items above are the reduced ones.

## `fgds repl` and `fgds tui`

What is left as it is in `fgds repl` and `fgds tui`:

- **Section 5 and a cargo feature for `fgds tui` (finding 10).** Decided at the re-review of `c381880f`: `rustyline`, `ratatui`, `crossterm`, `nix` and `unicode-width` are accepted in section 5 of the design without a cargo feature.
- **`cargo deny` is not in `scripts/check.sh` (re-review, finding 4).** It is not installed in the check environment and `cargo install cargo-deny` needs the network; once installed, `cargo deny --offline check licenses` still stops, because it asks for the crates of every target (`windows-sys` and others are not downloaded on Linux). Run once by hand with the network and the list MIT, Apache-2.0, BSL-1.0, it rejects 21 packages; those looked at are of the base, not of `repl` (the `icu_*` family, Unicode-3.0, through `reqwest`; `matchit` through `axum`), so a `deny.toml` needs a licence list for the whole workspace, which is a decision of the integration.
- **Terminals other than xterm** are not tested. Screens below 3 rows or 20 columns are drawn without a panic (unit test and `tests/tui/faults.py`) and are of no use.
- **A typed line of `fgds repl` longer than the terminal is wide** still leaves its first rows on the screen when an event is printed above it ([repl.md](../repl.md#limits)).
- **Signals outside the handled set** (SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGALRM, SIGUSR1, SIGUSR2), such as SIGVTALRM, SIGPROF and SIGXCPU, end the process without putting the terminal back (third review, finding 3: accepted, documented in [repl.md](../repl.md) and [tui.md](../tui.md)).
- **A terminal without bracketed paste**: a paste of several lines into `fgds repl` runs its first line and the rest is discarded by the editor, without a word. With bracketed paste nothing pasted runs before Enter.
- **SIGKILL and SIGSTOP** cannot be handled: the terminal is left as it was at that moment (`reset` puts it back).
- **Names of the dictionary that hold a character the escaping rule changes** are listed escaped and are not offered by Tab completion, so that completion never puts in the line a text that is not the name.

## Not built

Not built for storage:

| Requirement | What is missing |
|---|---|
| STO-6 | No migration: there is one schema version. A newer one is refused by name; nothing is copied to `<file>.bak` |
| STO-15 | No integrity check at open. A file that is not a database is refused by name and left as it is |
| STO-16 | Database size and free disk space are not in the status |
| STO-7 | The UI does not offer a choice of run; the API does |
| 4.6 | No retention or size limit; the database grows until the disk is full, which is then a storage failure (STO-9) |

Others:

| Requirement | What is missing | Milestone |
|---|---|---|
| MNT-5 | `cargo deny` is not run by `scripts/check.sh` | M2 |
| MNT-9 | No macOS job in the workflow | M2 |
| LNK-17, LNK-18 | Text logs per kind and the GDS log file | M2 |
| SEC-5 | Remote address of the client on sent-command records (a free-text client name is stored) | M3, with tokens |
| CUS-9, 4.3 | `--disable-plugin` and `--framing-layers` exist in the configuration file only | M2 |
| 4.4 | Startup does not log the configuration, dictionary version and plugin list | M2 |
| LNK-22 | The `tcp-server` listener stays open while the link is disabled | M2 |
| LNK-14 | A second interrupt does not shorten the stop | M2 |
| MNT-6 | No fuzz target for the dictionary loader and the deframer (hostile-input unit tests only) | M2 |
