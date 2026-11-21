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

1. **`Ref` is built with F Prime framing.** `Ref` on `devel` uses the `ComCcsds` subtopology and CCSDS is not in
   this slice. `scripts/build-ref.sh` copies the fprime checkout and switches the copy to `ComFprime` (three files
   under `Ref/Top`). The fprime repository is not modified. To drop when `framing-ccsds` exists (M5).
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

## Review fixes, SQLite storage

Decisions taken while fixing the review of the first slice (`rust-gds-review.md`) and building `storage-sqlite`.

- STO-9 is the simple form (section 7 of the design, decision taken by the orchestrator).
- Kind `gds.status` exists: one record each time the link state, its peer or its error changes (body `link`, `peer`, `error`). It takes a sequence number, so the first record of a run is usually the link's `connected`.
- A write that fails or times out drops the connection: half a frame may be on the link, and the next frame must not follow it. The adapter reconnects; the send is answered 502 and recorded with outcome `failed`.
- `Core::start_link` returns an error when the adapter cannot be built; `tcp-server` binds when it is built, so a port in use stops startup. A later failure is retried after 1 s, 2 s, ... 30 s and reported once per distinct message, under the plugin that owns the adapter.
- `tcp-server` closes a second peer at once while one is connected. The listener stays open while the link is disabled (LNK-22 asks for it closed): not built.
- Stopping: the link task and the handlers get 2 s, then are aborted; storage gets 3 s to flush and close. A second interrupt is not handled separately: the 5 s limit ends the process.
- A stream client that does not take a message within 10 s is closed without `lagged`; a client that reads but fell behind gets `lagged`. The stream refuses an `Origin` that is not the `Host` (403); a request without `Origin` (not a browser) is accepted. Tokens stay deferred.
- Dictionary limits: named types nested at most 64 deep, a type expands to at most 100,000 nodes, a struct has at most 10,000 members, a type encodes to at most 1 MiB, an array element has a size. Replacement fields of format strings: width and precision at most 512, checked at load (error naming the event, channel or type) and left as written when formatting.
- A command float beyond the range of `F32` is refused. `Infinity` and `NaN` given by name are still sent: the Python GDS sends them.
- `framing-fprime` refuses to frame a packet of 4,096 bytes or more (the answer is 422, the record `raw.sent` has outcome `rejected`), because the deframer of both GDS takes such a length for noise.
- A packet with several events or channel samples keeps the items decoded before a malformed one; the rest is one `gds.decode_error` with `partial: true` and `raw` set to the bytes not decoded (`fgds_core::keep_decoded`). A telemetry packet (fixed layout) is still all or nothing. The Python GDS was not compared on these bytes.
- Memory storage keeps at most `max-records` records (default 1,000,000), drops the oldest, and warns; it is no longer the default of the stock binary.

### `storage-sqlite`

- It is the default storage of the stock binary, and `logs` now defaults to `logs`: without `--database` the file is `logs/<start time>/gds.db` (STO-3). Tests of the core use `FakeGds::config()` (memory storage, no log directory).
- One file holds every run: tables `meta` (schema version, reserved sequence number), `dictionaries` (content by SHA-256, shared by the runs that used it), `runs`, `records`. Write-ahead log with `synchronous = NORMAL`: a commit survives a killed process (STO-11); a power loss may lose the last commits and leaves the file consistent.
- Sequence numbers: a run starts after the highest number reserved or stored, and reserves 1,000,000 ahead; the pipeline reserves further at half a block. A stop does not give the reservation back, so each run begins 1,000,000 or more after the previous one. Numbers are unique, not dense across runs.
- `append` ignores a sequence number that is already stored, so a batch repeated after a time-out makes no duplicate.
- SQL decides kind, run, sequence number, identifier and ground time; `Query::matches` decides field and text filters on the rows SQL returns, so both storages give the same answer and integers compare exactly. A query with a field or text filter reads every row that passes the SQL conditions until the page is full.
- `<database>.lock` is locked while a GDS writes, so a second one is refused (STO-10). The file stays after exit.
- Read-only (STO-12): no run begins and the last run is the current one. A database that was closed properly is opened as immutable and no file is created beside it; after a killed GDS, SQLite creates `<database>-shm` to read the write-ahead log, and the database file itself is still not written.
- `fgds run --replay <database>` is `--database <database> --read-only` with the dictionary stored with the last run; `--dictionary` is not needed.
- `GET /api/v1/dictionary?run=N` returns the dictionary of that run; `run=all` on the record routes selects every run.
- `SQLITE_DBCONFIG_DEFENSIVE` is on and `TRUSTED_SCHEMA` off on every connection (STO-13); no test shows a hostile file being stopped by them.

Not built for storage:

| Requirement | What is missing |
|---|---|
| STO-6 | No migration: there is one schema version. A newer one is refused by name; nothing is copied to `<file>.bak` |
| STO-15 | No integrity check at open. A file that is not a database is refused by name and left as it is |
| STO-16 | Database size and free disk space are not in the status |
| STO-7 | The UI does not offer a choice of run; the API does |
| 4.6 | No retention or size limit; the database grows until the disk is full, which is then a storage failure (STO-9) |

### Not built in this slice

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
