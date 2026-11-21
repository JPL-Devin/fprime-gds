# Deviations from the design

Where the code of the first slice differs from `requirements-and-architecture.md`, and decisions taken while
the user was away. The design document is unchanged. Nothing here changes a requirement or a decision of section 7.

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
| 8 | Storage failure (4.2, STO-9): 2 s commit time-out, memory buffer of 100,000 records, retry with back-off, buffer written on recovery | A failed `append` sets storage to `failed`; its records are broadcast at once with `stored = false` and are not kept. The next successful append publishes `gds.storage_gap {first_seq, last_seq}` and `gds.storage_recovered`. No time-out, buffer or retry | Memory storage cannot fail; the full path belongs with `storage-sqlite` (M1). The record kinds and the `stored` flag are as designed |
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
| `futures-util` | `fgds-shell` tests only | Reading a WebSocket in tests with `tokio-tungstenite` |

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
4. **Display text is produced in the decoder** and stored in the body (`display_text`), written exactly as the
   Python GDS writes it, so the comparison can require equality and old runs show what was shown (STO-5).
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
7. Say where display text is produced (decoder, stored) and that it must equal the Python GDS text.
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

## Python client, pytest fixture and online command-line tools

1. **The online subcommands are a plugin** (`plugins/online`, `OnlinePlugin`), not code in `crates/fgds/src/cli.rs`:
   `crates/fgds` was closed to edits while the core was amended. They use the HTTP API and the stream only.
   Until the plugin is among the defaults, `plugins/online/examples/fgds-online.rs` is the binary the checks use.
2. **`fgds --version` (CLI-6)** is left to `crates/fgds`.
3. **Raw send and file uplink from the command line (CLI-3)** are not built.
4. **Definition lists** are built from `GET /api/v1/dictionary`; the `definitions` routes do not carry
   descriptions and argument types. Type names follow `fprime-cli` (alias by underlying type, `String_<size>`).
5. **`fgds_client` uses the standard library only**, with its own small WebSocket reader, so a test machine needs
   no extra package.
6. **Class and method names follow the F Prime integration test API** (`send_and_assert_command`, `assert_event`,
   `await_telemetry`, `predicates`, `EventSeverity`), so existing tests change in their imports only. File and
   sequence uplink methods raise `NotImplementedError`.
7. **Histories are client side** (SCR-5): `clear_histories` forgets items in the client; the server keeps its records.
8. **Checks** are in `tests/cli/run.sh`; accepted differences from `fprime-cli` are in `tests/cli/README.md`.
||||||| 1ccafcbc

## Data products

- The plugin is `plugins/data-products`. It is not in `default_plugins()` on this branch (the orchestrator adds the line); `plugins/data-products/examples/fgds-dp.rs` is the stock GDS with the plugin, used by `tests/dp/run.sh` until then.
- The decoder reads the dictionary JSON itself (`types.rs`) instead of using `fgds-dictionary`: `fprime-dp` output carries dictionary text (`format`, `description`, type definitions) that the shared crate does not keep, and that crate was closed to changes. To merge later.
- The subcommand is `fgds dp decode|validate` with the arguments of `fprime-dp`. Argument errors give exit code 2 and the `fprime-dp` message; the usage text and `--help` are not those of `argparse`.
- Compression: `fprime-dp` handles `UNCOMPRESSED` and `ZLIB_DEFLATE`, so does the plugin. `miniz_oxide` is a direct dependency not in section 5 (no compression crate is listed there). `sha2` is in section 5 but was not in the workspace dependency list; both are declared in the plugin's `Cargo.toml`.
- The watched directory is polled (a quarter of the settle time, between 50 ms and 1 s) instead of using `notify` (section 5): DP-6 needs the size checked over time anyway, and polling behaves the same on network file systems.
- Options: `--data-products-dir`, `--data-products-settle-ms` (default 2000, DP-6), `--data-products-max-bytes` (default 268435456). Without `dir` nothing is indexed.
- A product over the size limit, or that inflates over it, is rejected with `Data product is over the size limit of N bytes`. `fprime-dp` has no limit; no fixture reaches it.
- DP-1: only the watched directory is a source (`"watch"`); completed downlinks wait for the files plugin. A record keeps a list of sources so DP-7 needs no change of body.
- A file whose content changes is a new product (new hash); the old one becomes missing when no file holds its content any more.
- The index record holds header fields, record count and the validation result, read once when indexed; records are decoded on demand from the file, which is read again only if it is a regular file inside the watched directory with the indexed hash (DP-8).
- The detail route returns strict JSON (non-finite floats as strings); the export route returns the bytes `fprime-dp` writes.
- The tab was not driven in a browser: its module, Lit and the routes it calls are checked over HTTP only.

## CCSDS framing

Decisions taken while building `plugins/framing-ccsds`, the stock `Ref` build and the comparison.

| # | The design or the Python GDS says | The code does | Why |
|---|---|---|---|
| 1 | Profile `ccsds` is "TM/TC + Space Packet" (4.4) | Layers `ccsds-tm-tc` > `ccsds-space-packet`, equal to the Python chain `space-packet-space-data-link`. A second profile `ccsds-space-packet` (layer `ccsds-space-packet-stream`) equals the Python chain `raw-space-packet` | The first is what `Ref` uses; the second costs one registration |
| 2 | Python has a third chain `raw-space-data-link` and a TM frame aggregator | The aggregator stage is written and passes its vectors; it is not registered as a layer, and the third chain is not a profile | No deployment of the repository uses them; registering is one line each |
| 3 | The Python Space Packet object counts sequence numbers per application identifier in one table for both directions | Uplink and downlink keep separate tables | A framer and a deframer are two objects in the design (4.3). `Ref` never uses one identifier in both directions with different counts; the vectors pass |
| 4 | Python fails at start when the dictionary has no `ComCfg.Apid` type | The packet type is used as application identifier, and `ComCfg.SpacecraftId` (0x44) and `ComCfg.TmFrameFixedSize` (1024) fall back as in Python | A dictionary without the CCSDS configuration can still be loaded |
| 5 | Python silently drops what is left of a TM frame's data after its last whole Space Packet | Left bytes become a `raw.discarded` record | LNK-7: no byte is lost without a record |
| 6 | Python rejects a wrong `--scid`, `--vcid`, `--frame-size` at start | Options `scid`, `vcid`, `frame-size` of the plugin `framing-ccsds`; a value that does not parse is logged and the dictionary value is used | Option validation belongs to the core (not in this part) |
| 7 | Two vectors of `ccsds.json` call `deframe` with `None` | Replayed as an empty input | Rust has no such call |
| 8 | Default profile "as the Python GDS defaults for current fprime `devel`": `space-packet-space-data-link` | The default list and default profile of `fgds` are unchanged (F Prime). `plugins/framing-ccsds/examples/fgds-ccsds.rs` is `fgds` plus the plugin; `--framing ccsds` selects it | The orchestrator owns the default list; the two lines to change are in the final message of this part |
| 9 | `FramingLayer` (4.3) gets the plugin configuration only | Core change: `FramingLayer::with_dictionary(&Dictionary) -> Option<Arc<dyn FramingLayer>>`, default `None`, called once at start | The CCSDS layers need the spacecraft identifier, the frame size and the `ComCfg.Apid` type of the dictionary |
| 10 | The JSON dictionary puts the element format string on the array type (`format`); the Python GDS reads it from `elementType.format`, which the dictionary never has, so it never applies it | Core change, one line of `fgds-dictionary/src/load.rs`: the array format is read from `elementType.format` | Found by the synthetic replay (`Ref.SG*.History`, `Ref.SG*.Info`): the Python GDS is the reference, even where it looks like a defect of it. To revisit if the Python GDS is corrected |

Comparison with the Python GDS (`tests/compare/README.md`):

1. The Python side of the live run is read from the REST endpoints of its GUI server. They give no event
   arguments, so live events are compared on identifier, name, severity and text; the replay compares arguments.
2. `fgds` records a refused command (`outcome: rejected`), the Python GDS records nothing. Only the refusal
   itself is compared.
3. The Python GDS sends a command given too few arguments (flight software answers `FORMAT_ERROR`); `fgds`
   refuses it. The scenario does not use that case.
4. `scripts/build-ref.sh` builds the F Prime framing variant by default, because `scripts/check.sh` (not owned
   by this part) calls it with two arguments; `tests/ccsds/run.sh` builds the stock `Ref`.

## Browser UI

- Lit 4.1.1 (the `lit-core` bundle) is vendored as `crates/fgds-shell/ui/lit.js` and served at `/ui/lit.js`; tabs import it from there. Nothing is fetched from elsewhere.
- `crates/fgds-shell/src`: the content security policy gains `frame-ancestors 'none'` and the response `X-Frame-Options: DENY`; `/ui/lit.js` is a new route. Nothing else changed in the shell's Rust code.
- The tabs Dictionaries, Logs and Advanced are plugins (`dictionary-tab`, `logs`, `advanced`) that are not in `default_plugins()`: the list belongs to `crates/fgds`. `examples/browser-gds` composes them for the browser checks until they are added.
- `logs` adds `GET /api/v1/logs` and `GET /api/v1/logs/{name}`: one text line per record, built from the record API (one log per record kind, and `gds`). It does not read log files from disk.
- History then stream: a tab pages `GET /records` after its last sequence number, then opens the stream with `afterSeq`; records at or below the last sequence number shown are dropped in the page. The `live` message of the stream is not needed for this and is ignored by `records.follow`.
- A tab whose module fails to load, or whose element throws when created or connected, is replaced by an error panel naming the plugin; errors thrown later by a tab are reported in the console only.
- Flight time is shown to the microsecond (the Python GDS page: millisecond). Channels without a sample are listed only on request.
- Settings (row limit, filters, selected tab) are kept in the browser's local storage, not on the server.
- Playwright is installed by `tests/browser/run.sh` into `target/browser-venv`, not into the fprime virtual environment.

## Sequence compiler and parameter tools

`plugins/sequences` (`fgds seqgen`) and `plugins/params` (`fgds prm-write`, `fgds prm-decode`). Decided without asking; where the Python tools are named, the behaviour was observed with `fprime-gds` of the F Prime virtual environment and is checked by `tests/seq/run.sh`.

Structure
- The grammar is not rewritten by hand. `plugins/sequences/tools/gen_tables.py` reads the LALR tables Lark builds from the grammar of the Python GDS and writes `src/tables.rs`; the Rust parser runs those tables with a lexer that, like Lark's contextual lexer, tries only the terminals the parser state accepts. Run the script again when the Python grammar changes. Error positions and the token named in a syntax error are therefore those of the Python tool.
- Numbers of a sequence are kept as written (integers of any size) and converted by the argument type, with the messages of the Python types (`Value 256 out of range!`, `invalid literal for int() with base 0: 'x'`, ...). Messages of `json.loads` are reproduced by `src/pyjson.rs`. The messages are part of the fixtures, so they are kept word for word, including Python type names such as `I32Type`.
- Time tags go through the same floating-point steps as `datetime.strptime` and `timedelta.total_seconds()`, so the microseconds are those of the Python tool, rounding errors included (`A2015-075T22:32:40.123` gives 122999).
- The offline tools read the dictionary through `sequences::read_dictionary_json`, which adds the sections the fixture dictionaries leave out (`typeDefinitions`, `commands`, `events`, `telemetryChannels`) and the `kind` and size of type references written in the short form (`{"name": "I32"}`), then hands the result to `fgds_dictionary::Dictionary::parse`. The Python loaders accept both; `fgds-dictionary` requires the full form. To move into `fgds-dictionary` if other parts need it.
- `plugins/params` asks `fgds-dictionary` for the type of each parameter by presenting the parameters as commands with one argument, because `Dictionary` has no parameter list. A `parameters` field on `Dictionary` would remove this.
- `--dictionary` is the global option; the short form `-d` of the Python tools does not exist. `fgds seqgen --all-errors` is the `cont` argument of `generateSequence`, which `fprime-seqgen` does not expose. `prm-decode -f text` writes `<file>.text` by default, as the Python tool.
- Neither plugin is in `default_plugins()` yet (not mine to edit). `plugins/params/examples/fgds-offline.rs` is the stock binary plus the two plugins; `tests/seq/run.sh` uses it until `fgds --help` lists `seqgen` and `prm-write`.

Where the result differs from the Python tools, on purpose
- A value out of the range of `F32` is rejected with `Value ... out of range!`; `fprime-seqgen` ends with an `OverflowError` traceback.
- With every error reported (`--all-errors`), the statement after one with a bad time tag is checked too; the Python parser skips it. Every message of the Python tool is in the output, in the same order.
- A quoted array or object inside an array that is not JSON (`["{x"]`) is reported at its line; the Python tool always says line 1.
- Inputs that make the Python tools end with a traceback (a time base other than the known ones, a sequence that is not UTF-8, and the cases `tests/seq/differential.py` counts as crashes) are rejected with a message. A file larger than 256 MiB is refused.
- `prm-decode` reads the length of a string with `FwSizeStoreType` of the dictionary. `fprime-prm-decode` does not load the type configuration and reads it as `U16`, so with the `Ref` dictionary (`U64`) it reads any string parameter as empty; `fprime-prm-write` does use the dictionary. The fixtures use `U16` and are not affected.
- `prm-decode` text and CSV show an array or struct value as its JSON value (`["ONE", "TWO"]`); the Python tool prints the internal description of the type (`{"name": ..., "format": "{}", "values": [...]}`), and its CSV does not double the quotes inside. Scalar and string parameters are the same text. In JSON, the Python tool leaves `format` and `description` keys in a struct inside a struct; `fgds` writes the values only, which is what `prm-write` reads back.
- A record whose size is less than the identifier (4 bytes) is rejected; the Python tool computes a negative size and reads on.
- Parameters are written in the order of their qualified names, as the Python tool does (its loader sorts them); with the same identifier twice in a dictionary the last one is used for decoding.

Not built: the Sequences tab and uplink (another part); `fpy` and other sequence languages; `fprime-prm-write` has no other output forms.
