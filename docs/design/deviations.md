# Deviations from the design

Where the code of the first slice differs from `requirements-and-architecture.md`, and decisions taken while
the user was away. The design document is unchanged. Nothing here changes a requirement or a decision of section 7.

## Differences from the Python GDS

Every known difference between what `fgds` and the Python GDS do for stock CCSDS `Ref`, one row each. Anything not
in this table is meant to be equal. Numbers are those of the verifier reports: W for the web report, C for the
command line report. "Python GDS defect, not reproduced" means `fgds` does what the dictionary and the bytes say.

Python versions used: the reference for every live comparison (`tests/compare`, `tests/browser`, `tests/cli`,
`tests/files`, `tests/seq` and the `Ref/test/int` tests) is the `fprime-gds` release 4.4.0 installed in the fprime
virtual environment. The fixtures of `fixtures/dp` and the `dp` subcommands follow `fprime-dp` of the `devel` branch,
commit `de71b640`. All other fixtures (`fixtures/command-args`, `fixtures/prm`, `fixtures/seq`, floats) were made
with release 4.4.0.

| Area | What Python does | What fgds does | Reason | Evidence |
|---|---|---|---|---|
| `command-send`, wrong argument count (C4) | Exit 0, no message, a frame is sent for `CMD_NO_OP extra`, `CMD_TEST_CMD_1 1 2.0`, `CMD_TEST_CMD_1 1 2.0 3 4`, `CMD_TEST_CMD_1` | Exit 1, no frame, `invalid: CdhCore.cmdDisp.CMD_NO_OP takes 0 arguments, 1 given`, then the command help | Python GDS defect, not reproduced: the dictionary gives the number of arguments, and fgds refuses any other count. With too few (`CMD_TEST_CMD_1 1 2.0`, `CMD_TEST_CMD_1`) `Ref` answers the Python frame with a format error (`FORMAT_ERROR`); with too many (`CMD_NO_OP extra`, `CMD_TEST_CMD_1 1 2.0 3 4`) `Ref` executes the command and ignores the extra bytes | Python frame for `CMD_NO_OP extra`: `20440412001000c02c0005000001000000e012`; 5 extra frames and their events on the Python side only |
| `command-send`, string longer than its maximum (C5) | `CMD_NO_OP_STRING` with 21 `é` (42 bytes, maximum 40): exit 0, frame sent | Exit 1, no frame, `invalid: argument arg1: string of 42 bytes exceeds maximum 40`, then the command help | Python GDS defect, not reproduced: Python counts characters (21), the limit of the dictionary is in bytes | String length in the Python frame: `002a` (42) |
| Event with a string of multi-byte characters (C8) | `CMD_NO_OP_STRING` with 20 `é` (40 bytes, valid; same frame from both; `Ref` prints the event): `fprime-cli events` does not show `NoOpStringReceived` | `CdhCore.cmdDisp.NoOpStringReceived (16777224) ... EventSeverity.ACTIVITY_HI : Received a NO-OP string=éééééééééééééééééééé` | Python GDS defect, not reproduced: the Python string type moves on by the number of characters, not bytes (the cause of C17), the event fails to decode and is dropped | Events of the run: 154 on Python, 143 on fgds, the difference being those of C4, C5 and this one |
| `Ref.wasmSeq.FileOpenError` status (C17) | `Failed to open Wasm module file seqs/é/none.wasm: status 1828716544` | `... status 1` | Python GDS defect, not reproduced: same cause as C8; Python reads the I32 one byte early, `6d 00 00 00` (the `m` of `wasm` and three bytes of the status) = 0x6D000000 = 1828716544 | Captured bytes: `0011` + 17 bytes of `seqs/é/none.wasm` (16 characters) + `00000001`; `Ref` sent 1 |
| `events -j` (C19) | 0 records, exit 1, `TypeError: Object of type Svc.PrmDb.PrmReadError is not JSON serializable`, or `... of type U32Type ...` | 357 records with keys `args`, `display_text`, `id`, `name`, `raw_time`, `severity`, `time` | Python GDS defect, not reproduced: the argument values handed to `json.dumps` are objects of the GDS type classes, so any event with an argument stops it (an enumeration, a plain U32) | The message above; `tests/cli` puts `patch/sitecustomize.py` on the Python path to compare the records at all |
| Channel samples when following a replay (C20) | Prints some samples twice and omits others; the count changes from run to run | Every sample once, in every run | Python GDS defect, not reproduced: `fprime-cli` asks its history for items "after the last time printed", which repeats and skips when one frame carries packets of two seconds | Same 198,656 bytes, 3 runs: Python 4,192 / 4,143 / 4,168 lines, of which 442 / 393 / 420 repeat a sample and 259 / 259 / 261 samples are absent; fgds 4,009 each time; no sample is Python-only |
| `prm-decode`, string parameter (C N1) | `"SEQ_BASE_DIR": ""`; `SEQ_BASE_DIR = "" (type: String_240, id: 268464130)`; `Ref.wasmSeq,SEQ_BASE_DIR,,String_240,268464130` | `"SEQ_BASE_DIR": "seqs/\u00e9"`; `SEQ_BASE_DIR = "seqs/é" (...)`; `Ref.wasmSeq,SEQ_BASE_DIR,seqs/é,String_240,268464130` | Python GDS defect, not reproduced: `fprime-prm-decode` reads the string length as 16 bits; the `Ref` dictionary says `FwSizeStoreType` is `U64` | The file `Ref` saved has `0000000000000007` + `seqs/é` after identifier `10007002`; the scenario set that value |
| `dp decode`, message (C29) | Release 4.4.0: `Decoding complete!` | `Decoding complete: Dp_00002576_1795286013_00670860.json` | fgds follows `fprime-dp` of `devel`, commit `de71b640` (the source of `fixtures/dp`), which prints `Decoding complete: {path}` | `git show de71b640` of `fprime-gds`; the virtual environment has 4.4.0 |
| `dp decode -r` (C30) | Release 4.4.0: exit 2, `fprime-dp: error: unrecognized arguments: -r` | Exit 0, writes `DpDemoContainer_1795286013_00670860.json` | As C29: `-r/--rename-id` exists at `de71b640` | As C29 |
| `dp decode` of a corrupt product (C31) | Exit 1, `Unexpected error: Array index 0 failed to deserialize: ... Needed: 18374686479671623697 Left: 760`, then a traceback ending `...DeserializeException: Array index 0 failed ...` | Exit 1, the same first line, then `Error: failed to decode corrupt.fdp: Array index 0 failed ...` | Text only. A traceback is the Python interpreter reporting an exception nobody caught: file paths and line numbers of Python sources that do not exist here. The last line of the traceback (the message) is kept, with the file name so that it is useful when several files are decoded | Exit code and first line equal; no output file on either side |
| Command line refused by the argument parser (C2, C7, C16) | Exit 2, the usage block (`usage: fprime-cli [-h] [-V]` / `{channels,command-send,events,send-raw,file-uplink} ...`), then `fprime-cli: error: unrecognized arguments: -3.4e38 255`, `fprime-cli: error: One of command-name or --list is required`, `fprime-cli events: error: argument -i/--ids: invalid int value: 'abc'`, `fprime-cli events: error: argument -t/--timeout: invalid float value: 'abc'` | Exit 2, no frame, the same message after `fgds: error: ` | Text only. The usage block names the program `fprime-cli` and lists its five subcommands; `fgds` is another program with other subcommands, so the block cannot be equal, and the prefix is the program name for the same reason (`fgds:` for `fprime-cli:` and `fprime-cli events:`) | Exit code, absence of a frame and the message after the prefix are equal (`tests/cli/check.py`) |
| Argument refused by both (C6) | Exit 1, no frame, a traceback ending `...CommandArgumentsException:   arg3[U8Type]: Value 256 out of range!` | Exit 1, no frame, `invalid: argument arg3: 256 is out of range for U8`, then the help of the command (name, opcode, arguments, description) | Text only. The traceback is an uncaught exception (see C31). The wording is that of `Dictionary::encode_command`, which the API, the page and the Python client also return, and it names the type by its dictionary name (`U8`) rather than the Python class (`U8Type`); the help lines follow because the user needs the argument list to correct the line | Exit code and absence of a frame equal |
| `prm-write`, `prm-decode` refusals (C34) | Exit 1, traceback ending `RuntimeError: Unable to find param Ref.recvBuffComp.nope in dictionary`; `...TypeRangeException: Value 256 out of range!`; `...EnumMismatchException: Invalid enum member PURPLE set in ABCMeta enum!`; `RuntimeError: CRC mismatch: file header has 0xd0546925, computed 0x0a062f42` | Exit 1, one line: the same message; for a refused value preceded by the parameter and its type, `Ref.sendBuffComp.parameter3 [U8Type]: Value 256 out of range!` | Text only. No traceback (see C31). The parameter name is added because a file sets many parameters and the Python message alone does not say which one is wrong (in Python the traceback does not say it either) | Exit code equal, no output file on either side |
| `command-send --list -j` (C14) | A Python `repr` per command: `{'args': [{'name': 'color', 'description': '', 'type': <class 'abc.Ref.DpDemo.ColorEnum'>}], ...` | JSON: `{"args": [{"name": "color", "description": "", "type": "Ref.DpDemo.ColorEnum"}], ...` | Python GDS defect, not reproduced: the option asks for JSON and no Python line parses as JSON | 126 items, 4 filter variants; all keys and other values equal |
| Readable time on the command line | Local time of the machine, no zone | UTC, no zone | A ground record should not change with the workstation's time zone | Equal where the machine runs in UTC |
| Command line layout | `fprime-cli` with dictionary and ZeroMQ options; `-t 0` to follow | `fgds <subcommand> --url`; also `commands`, `--history`, `--no-follow`, `--dry-run`; `fgds file-uplink` and `fgds files upload` are the same command | There is one process and an HTTP API, not ZeroMQ | `tests/cli/README.md`, 7 |
| `file-uplink` / `files upload`, progress lines (C35) | `Uplinking 'name': N%` lines are what a timer happens to see: small file, `0%` only in 7 of 9 runs and no line in 2 of 9; empty file, none, none, `100%` in 3 runs; 100,003 bytes, 17 / 18 / 17 lines, first `5%` / `0%` / `5%`, last `100%` / `99%` / `99%` | The same line, printed when the percentage read every 250 ms has changed; the verifier saw `0%` to `100%` in every run. The last one is `100%`, because the file stays in transfer until the flight side confirms it | The Python lines differ from run to run, so there is no text to equal; which percentages appear in between depends on the timing in fgds as well | The other lines (`Queued ...`, `All uplink packets sent ...`, `Flight software confirmed receipt: ...`, the errors) and the exit codes are equal (`tests/files/check.py`, which removes the progress lines before comparing) |
| File uplink: when it is finished (W18) | FINISHED when its own `comm` process has written the end packet, whatever the flight side did | `finished` on the `FileReceived` event naming the destination, or after 2 s with the link up; `failed` when the link goes away or a FATAL event arrives | "Finished" must not be shown for a file `Ref` did not get | `tests/files/repeat.py` |
| Sequences tab: refused sequence | HTTP 403 | HTTP 422 | 403 means "not allowed"; the request is allowed and its content is wrong | Messages in the page equal |
| Sequences tab: editor (W17, W30) | CodeMirror (line numbers, completion, checking while typing), `Build` panel | Plain text area, no `Build` panel | Layout only: a fourth vendored library for no difference in what is built or sent | Compiled bytes and messages equal (`tests/browser`) |
| Charts tab (W16, W29) | Zoom, pan, `Reset Zoom`, `Lock Scale`; streaming plugin axis | Absent; linear axis moved every 100 ms, tick text in the browser's locale | Layout only: two further libraries; entries, samples plotted and time window are equal | 187 entries in the same order |
| Start-up events `CdhCore.version.FrameworkVersion` (0x1003000) and `ProjectVersion` (0x1003001) sometimes absent (W33) | Absent from the page and `event.log` in 1 of 40 starts | Absent in 2 of 40 starts through a proxy, 1 of 40 without one | Not a difference: in each of these starts `Ref` printed both events and did not write their bytes to the socket (they are not in the proxy capture). In all 40 proxied fgds starts the raw log equals the captured bytes and the event records are the event packets in them; nothing read is dropped | 40 starts of stock CCSDS `Ref` per GDS through a byte proxy, events read 6 s after the start; `tests/link/check.py` repeats the byte and record comparison on 8 starts |
| Charts tab: pause and remove controls (W round 3) | Each chart has buttons with an icon and no text: pause/play (`fa-pause`, `fa-play`), delete (`fa-times`), collapse (`fa-minus`) | Each chart has buttons with the text `Pause`/`Resume` and `Remove`; no collapse | Layout only: fgds ships no icon font; the two actions do the same | `chart-display/addon-templates.js` lines 87 to 92 and 126 to 128 of fprime-gds 4.4.0; `plugins/charts/ui/charts.js` |
| Event text holding HTML (N1) | The page sets event text as HTML after escaping only part of it: `<h1>b</h1>` in the string of `CMD_NO_OP_STRING` is drawn as a heading, `<span title="t">` as a span | The characters that were sent, tags visible; nothing is interpreted and nothing is removed. `&amp;` reads `&` and two spaces read as one, as a user of the Python page reads them | Python GDS defect, not reproduced: text from the flight side must not become page structure (injection) | `tests/browser/parity.py` (texts with a span and a heading set aside from the cell comparison, then checked literal in fgds) |
| Composite channel values (N4) | An expandable nested table per array or structure; the cell text starts with a tab | One line of text, the same characters without the leading white space | Layout only | `tests/browser/parity.py` compares the cell text of 12 arrays and structures after trimming |
| Top-bar counters (N8, round 4 F1) | `WARNING HI: 0`, `FATAL: 0`, `GDS Errors: 0` whatever arrives: 8 WARNING_HI events and 1 FATAL event shown in its Events table left all three at 0 | The number of WARNING_HI and of FATAL events the page has received (`99+` above 99), and of errors of the page itself: `WARNING HI: 8`, `FATAL: 1` for the same captures | Python GDS defect, not reproduced: its page counts the field `severity` of each incoming event (`js/datastore.js`, `wrapFieldCounter`) and the items have none (keys `display_text`, `id`, `time`), so the counters never move | Web verifier round 4, F1; `tests/browser/ui.py` compares the counters with the event records. `tests/browser/parity.py` requires each counter to equal the number of rows of that severity in the Events table, for a replay (half read from the history, half from the stream) and after a reload. The "6 against 7 rows" of finding 35 was not reproduced: `counters.py` counts the rows that hold the text `WARNING_HI` anywhere, and `DUMP_FILTER_STATE` gives an ACTIVITY_LO event `WARNING_HI filter state. true`; the check has such a row and it is not counted. The capture of the verifier was not available, so that this is its seventh row is an inference |
| Page furniture (N8) | `Clear All` on Events and Channels; `Edit View` on Channels; a filter field and `First` `<` `>` `Last` on every table; a light/dark switch in the top bar | `Clear All` on Events and Channels (the page forgets its rows, the GDS keeps the records, a reload brings them back, UI-3). No `Edit View`, no light/dark switch, no paging buttons. Events toolbar: `Filter`, `Component`, a box per severity, `Rows`, box `Ground time`, `Pause`. Channels toolbar: `Filter`, `Telemetry packet`, box `Channels without a value` (off at start: the rows are those of Python), box `Ground time`. `Ground time` is the only column that can be hidden. A status footer | `Edit View` was not built; the rest is layout or extra | Both pages read side by side on one `Ref` session each; `tests/browser/ui.py` |
| Dashboard tab (N7) | Present when `config.enableDashboards` is set, hidden otherwise | Not built | Deferred: milestone M6 of the design, kept out of this work by the orchestrator (decision 15) | |
| `fprime-cli events` misses an event (F5) | In 1 of 3 runs `CdhCore.events.ID_FILTER_ENABLED` (`ID 12345 is filtered.`) was not printed although `Ref` sent it (present in the bytes captured on the Python side) | Printed in 3 of 3 | Python GDS defect, not reproduced: every record received is printed | Fresh command-line verifier, F5 |
| `command-send` with the link down | Exit 0, no message; nothing reaches the flight side | Exit 1, `503 Service Unavailable: link_down: link down` | A command that cannot be sent is an error for the script that sends it (CMD requirement: no silent loss) | Seen once by the fresh command-line verifier after `Ref` had died |
| Restart after SIGKILL with `Ref` connected (N5) | Page served again after 4.8 s | Listening and receiving telemetry again after about 1 s | Not a difference found here. The verifier saw `tcp 0.0.0.0:50100: Address already in use` for 65 to 126 s; not reproduced: the listening socket has `SO_REUSEADDR` (`strace`: `setsockopt(17, SOL_SOCKET, SO_REUSEADDR, [1], 4)` before `bind`), no source change was made. Port 50100 is in the range Linux gives to outgoing connections, so another socket of the machine can hold it; this is a guess, not verified | `tests/link` kills fgds with `Ref` connected and requires telemetry within 5 s of the restart; by hand 9 restarts (kill order fgds/`Ref`/both, with and without the verifier's `proxy.py`): 0 failed starts |
| Command history (W15) | The text typed | The value sent (`-0.0010000000474974513` for an F32 typed `-0.001`); extra columns opcode, outcome, Load, Resend | The history is the record of the bytes sent and is the same in every window | Argument text format equal otherwise |
| Ground time column, paging (W3) | No ground time; 40 events per page | Ground time column that can be hidden; one table with a row limit | Intended extra (API-4); layout | All other columns compared in `tests/browser/parity.py` |
| CSS classes, text colours (W4) | Bootstrap classes | Own classes | Layout only: the page is not Bootstrap | Row background colours equal and compared |
| Uplink tab (W19, W32, N6, round 4 F2, F3) | Columns Start Time, End Time, Source, Destination, State, Control, Progress. Staged row `up.bin`, the Destination Folder text, `NOT STARTED`, `Remove` `Select Packets`, `0%`. Sent rows newest first under the staged ones; state in capitals (`QUEUED`, `TRANSMITTING`, `FINISHED`); progress `100%`. The pause button reads `Unpause` while its file list is empty and `Pause` once it holds a file, also after `Pause` was pressed (seen: paused with a queued file, the button still reads `Pause`) | The same columns, staged row (without `Select Packets`) and order. State in lower case (`queued`, `transferring`, `finished`); progress `30000 / 30000 bytes (100%)` with a bar; a line `Queue: running, link: connected`. The button reads `Unpause` while the GDS has no file or the queue is paused, `Pause` otherwise | `Select Packets` (sending chosen packets of a file) is not built. Bytes and the queue line are extra. The button names what a press does; the Python text does not change with the pause state (`flags.uploading` in `js/datastore.js` is "the list was not empty in the last 5 s") | `tests/browser/ui.py`: columns, staged cells, button text, order. Times equal in form. Destination equal in the page and in `Ref`'s `FileReceived` text for the Destination Folder texts ``, `.`, `dir`, `dir/`, `dir//`, `./dir`: the folder as typed, a `/` unless it ends with one, the name (`os.path.join`); no folder: the name alone. `/` gives `/name` in both; `Ref` answers `FileOpenError`, so it is not in the check. Source is the staged copy in both: `<file storage directory>/fprime-uplink/<name>` |
| `Send Cancel Packet` (round 5 U1, U2) | The button is disabled while a row is `TRANSMITTING`. Idle, the packet has index 0 (the uplinker's index starts again at zero at the end of each file): frame `20440413001003c61d0006000303000000003571`. Its server has no check (read in `uplinker.py`, `send_cancel_packet`: "regardless of state"; not run) | The button is disabled while a file is `transferring`; idle, the same packet (`0003 03 00000000`). `POST /files/uplink/cancel-packet` during a transfer answers 409 and sends nothing | A cancel packet inside a file makes `Ref` drop the rest (`UplinkCanceled`, then `InvalidReceiveMode`) while the GDS goes on to "finished"; the page never allows it, so the server refuses it too | `tests/files/check.py` (index 0 before any file, after a finished and after a canceled file; 409 during a transfer; the packet bytes through the proxy), `tests/browser/ui.py` (button disabled during the transfer) |
| Downlink tab (W21) | Columns Start Time, End Time, Source, Destination, State, Control, Progress; Destination is the path on the ground (`<file storage directory>/fprime-downlink/down.bin`); `FINISHED`, `100%`; `Download` in Control | The same columns and `Download`, then a column Problems; Destination is the name (`down.bin`); `finished`, `20000 / 20000 bytes (100%)`. Below, a second table of every file in the downlink directory (data products included) | Layout only; the directory is a setting of the run | Side by side: `SendFile` and `SendPartial` (100, 500) of a 20000-byte file give `FINISHED`/`finished` and `3%` in both |
| Downlink of more bytes than announced | Not checked | `failed` | The file is not what the flight side announced | `tests/files` |
| Logs tab (W22) | Files of its processes (`comm.py.log`, `fprime-gds.log`, ...) | One log per record kind built from the record API; `gds` is the text log of `fgds` | There are no such processes | Same events and values in the same order |
| Dictionaries tab (W23) | Decimal identifiers; commands, events, channels | Hexadecimal identifiers; also argument and type columns and the sections Parameters, Types, Telemetry packets | Extra data from the same dictionary; numerically equal | |
| Advanced tab (W24) | Polling intervals and buffer sizes | Versions, link state, counters, plugins, tabs | The page is fed by a stream; there is no polling to set | |
| Data Products tab (W25) | None | Present, after the Python tabs | Requirement DP-5 | |
| Reload, second window (W26, W27) | The page comes back empty | The history comes back | Requirement UI-3 | |
| Python client imports (C23) | `fprime_gds` | `Ref/test/int` runs unchanged; its `fprime_gds` imports (`predicates`, `EventSeverity`) resolve to the Python package, the `fprime_test_api` fixture is that of `fgds_client` | Without the Python GDS installed the two imports change to `fgds_client` | 15 tests equal in 3 of 3 |

Not differences of `fgds`, but seen in the checks and handled there:

- **`Ref` prints events it does not send (C26).** Any `Ref/test/int` test can fail now and then for this reason, with either GDS: the test waits for an event that `Ref` printed in its log and did not write to the socket. Seen with `test_seqgen_complex_args` (fgds side: `Ref` printed four events, `Ref.typeDemo.ChoicePairEv`, `ChoicesEv`, `ChoiceSlurryEv`, a completion, flight times `1795298759:173448` to `:173476`, whose bytes are not in `raw.log`) and with `test_signal_generation` (Python side, 1 of 6 runs of the command-line verifier). `tests/cli/ref_int.py` runs a failed test once more, alone, only in that case.
- **The Python uplinker stops now and then** (`tests/files`). `FileUplinker.send` hands the packet to the encoder and only then sets the handshake it expects; for a start packet the handshake can return in between and is ignored, and the file ends as `TIMEOUT` after 20 s (reproduced by a 0.5 s pause at that point). `tests/files/check.py` begins the Python run again, at most three times, only on that state. fgds has no handshake step.

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
| 3 | The Python Space Packet object counts sequence numbers per application identifier in one table for both directions | The same: the framer and the deframer of a run share one table (`SequenceCounts`); a packet received sets the count of its identifier to the count received plus one, a packet sent takes the count and adds one | Not a deviation (N2): with separate tables `Ref` reported `UnexpectedSequenceCount` for a file uplink after a file downlink. `tests/files` |
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
- Flight time is shown as the Python GDS page shows it (ISO text to the millisecond for time base 2, `seconds.microseconds` otherwise). Channels without a sample are listed only on request.
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
- `prm-decode` writes the same JSON, text and CSV files as `fprime-prm-decode` for array and struct parameters too (command line finding 32): in text and CSV the value is the description of the type (`{"name": ..., "format": "{}", "values": [...]}`), with the quotes inside not doubled in CSV; in JSON a struct inside a struct keeps its `format` and `description` keys. `tests/seq/ref.py` compares the three files of the `Ref.typeDemo` parameters byte for byte.
- A record whose size is less than the identifier (4 bytes) is rejected; the Python tool computes a negative size and reads on.
- Parameters are written in the order of their qualified names, as the Python tool does (its loader sorts them); with the same identifier twice in a dictionary the last one is used for decoding.

Not built: `fpy` and other sequence languages; `fprime-prm-write` has no other output forms.


## File uplink and downlink

1. **Ground-side downlink names follow the Python GDS**: the destination path of the start packet becomes one file
   name with `/` and `\` replaced by `_` (`./DpCat/x.fdp` is stored as `._DpCat_x.fdp`), directly inside the
   downlink directory. Names that are empty, `.` or `..` fail the transfer and are reported.
2. **Upload is the raw request body**: `POST <prefix>/uplink?name=<file>&destination=<flight path>`, not a multipart
   form. The staged copy lives under the files directory; `--files-max-upload-bytes` bounds it.
3. **The queue starts running**, as the Python GDS queue does (ruling on verifier finding 35; FIL-4 said paused at
   start). A file given before the first connection waits and is sent once the link connects. A link drop fails the
   active uplink, pauses the queue and keeps the files behind it; resume is refused while the link is not connected.
4. **Pause ends the active uplink** with a cancel packet, as cancel does; the file is not continued after resume,
   the next one is sent.
5. **`fprime.file.packet` is a transient kind**: file packets are decoded for the handler but not stored, so file
   content is never in the record store. Progress, state and problems are `fprime.file.uplink`,
   `fprime.file.downlink` and `fprime.file.problem` records.
6. **`fgds files`** (upload, queue, pause, resume, cancel, list, download) is the online tool; it supersedes item 3
   of the previous section for file uplink.
7. **Lit** is one embedded file (`plugins/files/ui/lit-core.min.js`) served by the plugin.
8. **Data products**: a product made by `dpDemo.Dp` is downlinked through `dpCat` and compared by content with the
   file `Ref` wrote; it is not handed to a decoder, no data products plugin is on the base branch.
9. **Core change**: `StatusHandle::link()` in `crates/fgds-core/src/status.rs` returns the link state, which the
   uplink handler needs for FIL-4.
10. **The tabs were not exercised in a browser**; the checks only fetch the modules.

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

## Integration

Decisions taken while making `scripts/check.sh` pass on the merged tree.

1. **Stock CCSDS `Ref` is the main build.** `check.sh` builds each `Ref` once (`target/ref-ccsds` as it is, `target/ref` with F Prime framing), runs sections 2 and 3 on both, and gives the CCSDS one to every `tests/*/run.sh`. `tests/ccsds/run.sh` is gone: its work is sections 2 and 3. The synthetic comparison stays on F Prime framing: the Python side writes that capture with its F Prime framer.
2. **Example binaries deleted**: `fgds-online`, `fgds-dp`, `fgds-ccsds`, `fgds-offline` and the `fgds` dev-dependency of their plugins. Every run script uses `$FGDS`. `examples/browser-gds` remains, as the stock GDS plus a tab that fails.
3. **New crate `crates/fgds-offline`** (a library, not a plugin). `params` depended on `sequences`, which MNT-1 forbids and `check.sh` refuses. What both use (values as the Python tools read them, conversion to a dictionary type, the dictionary file as those tools accept it, the checksum) moved there unchanged; `sequences` re-exports it.
4. **An array or struct command argument given as text is read as JSON** (`fgds-dictionary`, `Codec::encode`), as `fprime-cli command-send` does (`json.loads`). `fgds command-send` sends its arguments as text.
5. **Non-finite floats in the browser.** The Python page shows a scalar channel value with JavaScript (`Infinity`, `-Infinity`, `NaN`: the number its JSON carries) and the members of an array or struct as the text Python formatted (`inf`, `-inf`, `nan`). The channel record carries both forms and the Channels tab applies the same rule; event texts are formatted by the backend in both and already agreed.
6. **Processes.** Everything started through `scenario.start` is in its own process group and is stopped, with its descendants, when the script ends in any way (exit, exception, SIGTERM, SIGHUP). `check.sh` ends with a check that no process and no file is left (databases and logs go to `OUT`).
7. **`fprime-cli` following may print an item twice** (accepted difference 1 of `tests/cli/README.md`): the check requires each item it printed to be among those of `fgds`.
8. **Replay checks wait for the packet count to stop changing** instead of a count computed from the capture: with CCSDS framing the number of packets is not the number of frames.
9. **`tests/ui` removed.** It ran the page shell of the storage branch in node with a hand-made page; the merged shell is the Lit one and needs a browser. Its two subjects are in `tests/browser/ui.py`: failing tabs (already there) and a stream that ends (new, with a mocked WebSocket).

## Verifier report (web UI against the Python GDS on stock CCSDS `Ref`)

- **File uplink pace (finding 18).** `Ref` has no flow control for file packets: `Svc.FileUplink` has a queue of 10 and ends with an assert when a packet does not fit. The Python GDS does not wait for the flight side either (its "handshake" is made by its own `comm` process once the frame is written); its pace, about 11 ms per packet, is what its process hops take. fgds sent a packet every 1.2 ms, and a relay or a loaded machine handing `Ref` more than 10 at once ended it. `--files-cooldown` is now 0.01 s by default (was 0), which is the Python pace. `tests/files/repeat.py` uploads 30000 bytes 20 times, half through a relay that holds bytes for 30 ms.
- **When an uplink is finished (finding 18).** After the end packet fgds watches for up to 2 s: `finished` on the `FileReceived` event of `Svc.FileUplink` naming the destination, or when the time passes with the link connected; `failed` when the link goes away or a FATAL event arrives (also during the transfer). The Python GDS says FINISHED once its own handshake for the end packet is back, whatever the flight side did.
- **Events and Channels text (findings 1, 2, 5 to 9).** Fixed, not deviations: millisecond time, events oldest first, channels by identifier, event text through the Python page's rule (markup escaped, entities decoded, runs of spaces rendered as one), composite values with the Python quotes and spacing, green rows for a value inside its limits. `tests/browser/parity.py` compares order, text and row colour.
- **Ground time column (finding 3).** Intended extra: the record API has the time of reception and operators asked for it. A box in each toolbar hides it; the choice is kept in the browser.
- **Paging (finding 3).** The Python page shows 40 events per page; fgds shows one table with a row limit. Layout only.
- **CSS class names and text colours (finding 4).** Intended: the page is not Bootstrap. Row background colours are equal and compared.

### Command argument text (web finding 20, command line findings on argument parsing)

- Integer, float and boolean arguments given as text are read by one rule in `fgds-dictionary` (`input.rs`, used by
  `Dictionary::encode_command`): Python `int(text, 0)`, `float(text)` and the boolean words of
  `CmdData.convert_arg_value`. `fixtures/command-args/table.json` (835 texts, made by `generate.py` from the Python
  GDS) is the test table: each text gives the same bytes or is refused.
- An array or a struct given as JSON text is read as Python `json.loads` reads it (`fgds-dictionary`, `pyjson.rs`),
  into a typed value of its own: an integer literal of any size is an integer (out of range for an integer member,
  named as given in the refusal; `float(int)` for a float member, refused when beyond a double or beyond `F32`); a
  float literal beyond a double is an infinity and is sent; `NaN`, `Infinity`, `-Infinity` are numbers; an object
  is never a number (command line round 5, N2 to N5). `fixtures/command-args/json-table.json` (3500 rows, made by
  `generate_json.py` from the Python GDS 4.4.0: integer and float literals around 2^7 to 2^128, 10^38, 10^39,
  2^1023, 2^1024, 10^308, 10^309, 10^400, both signs, for each of the ten scalar members of `Ref.ScalarStruct` and
  as the text of a scalar argument) is the test table: the same bytes, or refused. The wording of a refusal is the
  one of row C6 (`18446744073709551616 is out of range for U64` against `Value 18446744073709551616 out of range!`).
- The Commanding tab applies the stricter rules of the Python GDS page before sending (signed integers decimal only;
  unsigned binary, octal, decimal, hexadecimal; no surrounding spaces) and shows its messages; a float is a number
  input.
- Every input that is empty or not valid gets its message in one press of the send button (web finding 36), also
  the inputs nothing was typed in. `fixtures/command-messages/python-page.json` holds what the page of Python GDS
  release 4.4.0 shows for 111 presses on 8 commands (integers of each width, floats, string, bool, enum, array,
  struct, struct holding arrays; each input empty, with an invalid text, typed then emptied, alone and with the
  others invalid), recorded by `tests/browser/messages.py record`; `messages.py check` replays them on the new page.
  One difference is kept: where the Python page marks a field without a message of its own (a text typed then
  removed, a boolean not chosen, `007`) and leaves the explanation to the bubble of the browser, the new page
  writes `<command> was not sent: the marked fields are empty or not valid` under the form, so no refusal is silent.

### Command line output (command line findings 2, 3, 6, 7, 9 to 16, 21, 22, 35)

- Fixed, not deviations: `events`, `channels` and `command-send` print the lines of `fprime-cli` (`EventSeverity.X`, `None` for a missing description, `U32Type`, JSON as `json.dumps` writes it, time without zone, channel `display_text` typed as in Python and no `value` key); an unknown command prints the text of `fprime-cli` on the standard output with exit code 0 and the near names of `difflib.get_close_matches`; no command name, `-i abc` and an argument such as `-3.4e38` give the argparse error and exit code 2. `tests/cli/check.py` compares the output byte for byte.
- **Order of the lines (finding 22).** Fixed: `fgds events` and `fgds channels` print in the order of `fprime-cli`, by flight time and by arrival where the times are equal (the stored records at once; when following, records that arrive within 100 ms of each other). The record store, the API and the stream keep the order of arrival. The Events tab shows the order of arrival; `tests/browser/parity.py` compares its row order with the Python page.
- **File uplink from the command line (finding 35).** Fixed: `fgds file-uplink SRC [DEST]` (also `fgds files upload`) prints the lines of `fprime-cli file-uplink` (`Queued ...`, `All uplink packets sent ...`, `Flight software confirmed receipt: <the FileReceived event>`) and returns when the flight side has the file; `-t/--timeout` (60 s), `--no-wait`; exit code 1 on a timeout, a missing file or a failed uplink. Without `DEST` the destination printed and sent is `/<name>`, as `fprime-cli` (F18); the route, the page and the command line take the path from one function, `uplink_destination`.
- What is kept is in the table "Differences from the Python GDS".

### Remaining web findings

- **Keep-alive (web finding 10, command line finding 1).** Fixed: `comm-tcp` writes the 12 bytes `sitting well` every `keepalive-interval` seconds (default 0.5, as `fprime_gds.common.communication.adapters.ip`; 0 turns it off), never inside a frame.
- **Invalid-input messages (findings 12 to 14, 28).** Fixed: the messages of the Python page next to the field and no line under the form, an empty string refused, a float in a number input that keeps the text being typed (`1e3`, `-2.5e-3`, `.5`).
- **Command history (finding 15).** The argument text is that of the Python page (strings in double quotes, arguments separated by a comma, composites in brackets and braces). Kept: the columns (opcode, outcome, Load and Resend) and a float shown as the value that was sent (`-0.0010000000474974513` for an F32 typed `-0.001`) rather than the text typed: the history is the record of the bytes, and it is the same in every window.
- **Charts tab (findings 16, 29).** As the Python page: one channel per chart, chosen from the numeric entries of the dictionary in identifier order (187 for `Ref`; array elements as `name.0`, structure members as `name.member`), drawn from the first sample received after the choice, on an axis of 60 s that ends at the workstation time; `Add Chart`, `Save` and `Load` (the same file: one channel per line), `Help`, `Data Window (S)`, the three time modes, pause. Differences: zoom, pan, `Reset Zoom` and `Lock Scale` are absent, because they are the two further libraries the Python page loads (`chartjs-plugin-zoom`, Hammer) and nothing displayed depends on them; the axis is a linear Chart.js axis moved every 100 ms rather than the streaming plugin, so the tick texts are those of the browser's locale time.
- **Sequences tab (findings 17, 30).** As the Python page: name, uplink directory (`/seq` when empty), `Uplink`, `Run` (the `CS_RUN` command of the dictionary with the uplinked path and `BLOCK`), `Save As`, `Open`, an empty editor, and the messages of the Python page (`<staging directory>/<name>:<line>: ...`, `Sequence is N bytes with timebase ANY`, `CRC: ...`, `Uplinking to ... Please confirm uplink EVRs before running.`, `Sent CS_RUN for ...`). Differences: the editor is a plain text area (the Python page uses CodeMirror: line numbers, completion and checking while typing; a fourth vendored library for no difference in what is built or sent); no `Build` panel (it is the form of the Commanding tab, one click away); a refused sequence is answered 422 where Python answers 403.
- **Tab order (finding 31).** Fixed: the order of the Python page, then Data Products. `UiBundle.order` is the position a plugin registers; `[tab_order]` in the configuration file (`<tab id> = <number>`) moves a tab without touching the plugin.
- **Uplink and Downlink tabs (findings 19, 21, 32).** Start and end times are shown in the text of the Python page (`Sat, 21 Nov 2026 22:53:23 GMT`); the uplink destination is the file name when no directory is given, and otherwise the directory as typed, a `/` unless it ends with one, and the name (`os.path.join` in the Python uplinker), in the page and in the start packet. The columns are those of the Python page (Downlink adds Problems); uplinked files are listed newest first. Kept: see the rows "Uplink tab" and "Downlink tab" of the table.
- **Partial downlink (finding 20).** Fixed: fewer bytes than announced with a matching checksum is `finished` with the percentage received, and data that starts at the offset of `SendPartial` is not counted as a problem. More bytes than announced stays `failed`.
- **Logs tab (finding 22).** Layout only: one log per record kind built from the record API, in the line format of the API; the lines hold the same events and values in the same order. There are no `comm.py.log` or `fprime-gds.log` because there are no such processes; the text log of `fgds` is the choice `gds`.
- **Dictionaries tab (finding 23).** Descriptions are on one line as on the Python page (fixed). Kept: identifiers in hexadecimal as in the Events and Channels tables (numerically equal), argument and type columns, and the sections Parameters, Types and Telemetry packets, which the Python page does not have.
- **Advanced tab (finding 24).** Different content by design: the Python page lists its polling intervals and buffer sizes, which do not exist here (the page is fed by a stream); `fgds` shows versions, link state, counters, plugins and tabs.
- **Data Products tab (finding 25).** Extra, a requirement (DP-5).
- **Reload and second window (findings 26, 27).** Intended, a requirement (UI-3): the history comes back; the Python page comes back empty.

### Remaining command line findings

- Fixed: `seqgen` (findings 27, 28: the two lines of `fprime-seqgen`, messages on the standard output); `prm-decode` and `prm-write` output files and lines (findings 32, 33); file uplink sent at once at the pace of the Python uplinker, source path `/tmp/<user>/fprime-uplink/<name>`, `Error: File not found at <path>`, `--url` (findings 36 to 38).
- Kept: see the table "Differences from the Python GDS".
