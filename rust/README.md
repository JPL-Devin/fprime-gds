# `fprime-gds-rust`

A Rust re-implementation of the F´ Ground Data System.  CLI-only — no web GUI.
Lives next to the existing Python `fprime-gds` and shares its wire format so
the two are interoperable on the network.

## Why a Rust port?

The Python GDS is a multi-process system: a `comm` process talks to the FSW
over TCP, a `tcpserver` multiplexes ground-side clients, and a Flask app
hosts the GUI.  For many operator and CI workflows that's overkill — what
people actually want is a single binary that can:

1. Open a TCP port (server or client) to talk to a running F´ deployment.
2. Frame and deframe packets on the wire.
3. Decode events and telemetry against a JSON dictionary.
4. Encode and send commands.
5. Provide an interactive prompt for commanding while telemetry streams in.

This crate does exactly that, in a single statically-linked binary.

## Layout

```
rust/
├── Cargo.toml                  # workspace
├── rust-toolchain.toml         # pins the stable toolchain
└── crates/
    ├── fprime-types/           # F´ wire-format primitives (U8…U64, I8…I64, F32/F64, bool, string, TimeType)
    ├── fprime-frame/           # F´ framer/deframer (DEADBEEF + len + payload + CRC32) + Framer/Deframer traits
    ├── fprime-ccsds/           # CCSDS Space Packet (133.0) + TC/TM Space Data Link (132.0/232.0) + chained framer
    ├── fprime-dict/            # JSON topology dictionary loader
    ├── fprime-pipeline/        # decoders (event/channel) and encoder (command)
    ├── fprime-comm/            # tokio TCP adapter (server or client) with auto-reconnect, protocol-agnostic
    ├── fprime-dp/              # data-product (.fdp) decoder (header + records) -> JSON
    └── fprime-gds-rust/        # `fprime-gds-rust` binary: CLI + REPL
```

Every crate has its own unit tests and is small enough to be understood in a
sitting.

## Building

The workspace targets stable Rust (1.85+).  After cloning:

```bash
cd rust
cargo build --release          # produce target/release/fprime-gds-rust
cargo test --workspace         # run unit tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

A compatible toolchain is pinned via `rust-toolchain.toml`, so `rustup` will
download it automatically the first time you run `cargo`.

## Usage

The binary has four sub-commands.  `run` is the default and can be invoked
without naming it:

```bash
# default: listen on 0.0.0.0:50000 for an FSW connection
fprime-gds-rust --dictionary /path/to/TopologyAppDictionary.json

# connect outbound to FSW
fprime-gds-rust --dictionary dict.json --connect --host 192.0.2.10 --port 50000

# inspect a dictionary without connecting
fprime-gds-rust dict-info /path/to/TopologyAppDictionary.json

# wire-level debugging: round-trip hex payloads through the framer
fprime-gds-rust frame-test deadbabe

# decode a data-product (.fdp) binary file to JSON (mirrors the Python GDS
# `data_products` tool).  Validates the header and data CRC32s.
fprime-gds-rust dp-decode --dictionary dict.json container1.fdp
fprime-gds-rust dp-decode --dictionary dict.json container1.fdp -o -   # stdout

# CCSDS: Space Packet inside TC/TM transfer frames (chained — same as the
# Python GDS `space-packet-space-data-link` plugin)
fprime-gds-rust --dictionary dict.json --protocol ccsds --scid 0x44 --vcid 1

# CCSDS: Space Packet only (`raw-space-packet`)
fprime-gds-rust --dictionary dict.json --protocol ccsds-space-packet

# CCSDS: Space Data Link only (`raw-space-data-link`)
fprime-gds-rust --dictionary dict.json --protocol ccsds-space-data-link \
    --scid 0x44 --vcid 1 --frame-size 1024
```

### Interactive REPL

When stdin is a TTY, `run` drops you into a prompt.  Telemetry and events
print above the prompt as they arrive:

```
fprime-gds-rust — type 'help' for commands, 'quit' to exit.
[comm] listening on 0.0.0.0:50000
[comm] connected to 127.0.0.1:47128
00:05:30.569  EVT DIAGNOSTIC Ref.rateGroup1Comp.RateGroupStarted
00:05:30.569  TLM Ref.blockDrv.BD_Cycles = 9999
fprime> cmd Ref.cmdDisp.CMD_NO_OP_STRING "hello world"
ok: queued Ref.cmdDisp.CMD_NO_OP_STRING (opcode 1281)
fprime> dict events RateGroup
   512  DIAGNOSTIC  Ref.rateGroup1Comp.RateGroupStarted  (Rate group started.)
   513  WARNING_HI  Ref.rateGroup1Comp.RateGroupCycleSlip  (Rate group cycle slipped on cycle {})
fprime> mute channels
fprime> quit
```

REPL commands:

| Command | Description |
|---|---|
| `help` | print the help text |
| `dict commands [pattern]` | list commands (substring filter) |
| `dict events [pattern]` | list events |
| `dict channels [pattern]` | list channels |
| `cmd <qualified.name> [args...]` | encode + uplink a command |
| `raw-uplink <hex>` | uplink an unframed payload (for debugging) |
| `mute events\|channels` / `unmute …` | silence/unsilence a downlink stream |
| `quit` | exit |

Argument syntax for `cmd` mirrors the dictionary's primitive types:

* Integers can be decimal or `0x`-prefixed hex.
* Booleans accept `true`/`false`/`yes`/`no`/`1`/`0`.
* Strings should be quoted with shell-style quoting (`"hello world"`).

When stdin is *not* a TTY (CI, shell pipes, redirected input) the REPL is
disabled automatically and downlink is simply logged to stderr — handy for
recording sessions with `tee`.

## Wire compatibility

`fprime-gds-rust` speaks the same byte protocol as the Python GDS:

| Layer | Format |
|---|---|
| Frame (F´ DEADBEEF) | `start (4 BE = 0xDEADBEEF) \| length (4 BE) \| payload \| crc32 (4 BE)` |
| Packet (any direction) | `descriptor (FwPacketDescriptorType, U16 BE) \| body` |
| Event body | `event_id (FwEventIdType, U32 BE) \| TimeType (11) \| args` |
| Channel body | `channel_id (FwChanIdType, U32 BE) \| TimeType (11) \| value` |
| Command body (uplink) | `desc=0 (U16 BE) \| opcode (FwOpcodeType, U32 BE) \| args` |
| TimeType | `time_base (2 BE) \| time_context (1) \| seconds (4 BE) \| useconds (4 BE)` |

Note: the Python GDS' `cmd_encoder` prepends `0x5A5A5A5A | U32 length` to its
output, but those eight bytes are *internal middleware framing* between the
GDS comm process and the local Tcp server — they get stripped before the wire
ever reaches the FSW.  We don't emit them.

Frames produced by `fprime_frame::frame(...)` are byte-identical to those
produced by `fprime_gds.common.communication.framing.FpFramerDeframer` with
the default `crc32` checksum.

### CCSDS protocols

With `--protocol ccsds-space-packet`, `--protocol ccsds-space-data-link`, or
`--protocol ccsds`, the comm layer wraps payloads in CCSDS instead.  Each of
the three layers is byte-for-byte identical to the corresponding plugin in
`fprime_gds.common.communication.ccsds`:

| Rust `--protocol` | Python plugin | Wire format |
|---|---|---|
| `ccsds-space-packet` | `raw-space-packet` (`SpacePacketFramerDeframer`) | 6-byte primary header + payload (no transfer frame) |
| `ccsds-space-data-link` | `raw-space-data-link` (`SpaceDataLinkFramerDeframer`) | 5-byte TC primary header + payload + 16-bit CCITT-FALSE CRC (uplink); fixed-size 6-byte TM primary header + payload + CRC (downlink) |
| `ccsds` | `space-packet-space-data-link` (`SpacePacketSpaceDataLinkFramerDeframer`) | TC/TM transfer frame around an inner Space Packet |

* APID is read from the leading 4 bytes of the F´ payload (the descriptor
  field) — same convention used by the Python `SpacePacketFramerDeframer`,
  which calls `ConfigManager().get_type("ComCfg.Apid").deserialize(data, 0)`.
  Per-APID sequence counters increment automatically.
* Spacecraft id (`--scid`, default `0x44`), virtual channel id (`--vcid`,
  default `1`), and TM frame size (`--frame-size`, default `1024`) match the
  Python `FALLBACK_*` constants when no dictionary override is supplied.
* TM downlink filters: bad CRC, wrong scid/vcid, and the idle APID `0x7FF`
  are dropped silently; sequence-count gaps log a warning but the packet is
  still delivered.

`crates/fprime-ccsds/tests/python_parity.rs` checks the wire bytes against
the Python reference for three concrete vectors so any future refactor that
breaks parity will fail in CI.

## Out of scope (for the initial port)

These are intentionally not implemented yet.  Each can be added as a separate
crate without changing the existing public APIs.

* Web GUI / Flask static.
* ZMQ transport (Python alternative to TCP).
* SDLS (CCSDS encryption) — adding it would slot in as another link in the
  ccsds chain.
* Serial (UART) adapter.
* File uplink/downlink protocol.
* Sequence file generation (`seqgen`).
* Integration test API.
* Packetized telemetry decoding (frames are surfaced as `PKTLM` events).
* Compound argument types (enum / struct / array) — the dictionary loader
  reports them as unsupported and the REPL refuses to encode commands that
  use them.

## Relation to the Python GDS

This crate does *not* replace the Python `fprime-gds`.  It targets the
operational subset (TCP comm + commanding + telemetry tail) used by most
operators and CI jobs, with much faster startup and a single static binary.
The Python GDS is still the canonical implementation for the GUI, the
Integration Test API, and the broad plugin ecosystem.

Both implementations share the on-the-wire protocol and the same JSON
dictionary, so they can be mixed: a Python `gds` GUI can talk to FSW that
is also being driven by `fprime-gds-rust`, or the Rust binary can replace
the Python `comm` process while keeping the rest of the Python pipeline.
