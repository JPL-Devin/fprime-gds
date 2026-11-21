# Comparison with the Python GDS

Section 6.1 of the design: the current `fprime-gds` is the reference for what `fgds` must report.
`compare.py` runs both and fails on any difference that is not listed here.

| File | Role |
|---|---|
| `scenario.py` | The scenario (commands, timing), the TCP proxy that captures both directions, the common form, the coverage count |
| `python_gds.py` | The Python GDS side; needs the fprime virtual environment |
| `rust_gds.py` | The `fgds` side: `fgds run` and its HTTP API; standard library only |
| `compare.py` | Runs both sides, compares, prints one line per comparison and the coverage |

Three comparisons, on two builds of `Ref`:

| Comparison | `Ref` with F Prime framing (second profile of `scripts/check.sh`) | `Ref` as it is, CCSDS framing (main profile of `scripts/check.sh`) |
|---|---|---|
| Live | yes | yes |
| Replay of the bytes of the live run | yes | yes |
| Synthetic replay of every event and channel (`compare.py` without `--ref`) | | yes (the packets are fed to `fgds` in F Prime frames) |

`--framing fprime` (default) or `--framing ccsds` tells both sides which `Ref` they talk to. Both builds stay
in the checks.

## Scenario

`Ref` connects to the GDS through the capturing proxy. Once telemetry arrives the GDS sends the 15 commands of
`scenario.COMMANDS`, 0.5 s apart, then the 2 requests of `scenario.REJECTED`, and the run ends 4 s later:

- scalar, string and enum arguments (`CMD_NO_OP_STRING`, `CMD_TEST_CMD_1`, `HLTH_ENABLE`, `SG1.Settings`);
- the type-demo commands of `Ref.typeDemo`, whose arguments are enums, arrays, arrays of arrays, structs holding
  arrays and structs, and every scalar at its limits; each answers with an event and channels of the same types;
- `FileHandling.fileManager.RemoveFile` and `FileSize` of a file that does not exist: commands that fail on board
  (`OpCodeError`, `EXECUTION_ERROR`);
- refused by the GDS: an enum constant that does not exist, and a command that does not exist.

## Common form

One JSON file per run: `events` (identifier, name, severity, flight time, arguments, formatted text), `channels`
(identifier, name, flight time, value, formatted text), `commands` (name, opcode, arguments), `answers` (the
status of each request). Integers are written as text (64-bit safe), non-finite floats by name, flight time as
`[base, context, seconds, microseconds]`. Arguments are `(name, value)` pairs in declaration order: a value
under the wrong name is a difference.

## Coverage

Each comparison prints how many distinct commands, events and channels it saw out of the dictionary's totals,
and which type shapes were among them: enum, array, struct, string, 64-bit, float. A shape with no sample is a
failure. The synthetic replay must cover every event and every channel of the dictionary.

## Live

The Python side is what the Python GDS shows: `fprime-gds` is started with its GUI server and no browser, and
the run is read from its REST endpoints (`/commands`, `/events`, `/channels`, `/dictionary/*`); commands are
sent with `PUT /commands/<name>`. The `fgds` side uses `/api/v1`. On both sides the export waits until `Ref` is
stopped and the record cursor has not moved for one second.

Compared: the sent commands (name, opcode, argument pairs); the bytes of the uplink, command packet by command
packet (F Prime) or TC frame by TC frame (CCSDS); which requests were accepted and which refused; the events of
the components the scenario commands; the last sample of the channels the scenario sets; the set of channels
received.

Each GDS drives its own run of `Ref`, so the two runs are not the same data. Accepted differences:

| Difference | Why it is accepted | What is still compared |
|---|---|---|
| Flight time and ground time | Two runs at two moments | Everything else in the compared items |
| Events not caused by the scenario (start-up, health pings, rate groups) | Their number depends on when the GDS connected | Every event of `CdhCore.cmdDisp`, `CdhCore.health`, `FileHandling.fileManager`, `Ref.SG1`, `Ref.typeDemo`: identifier, name, severity, text |
| Order of the command dispatcher's events between two commands | A command may complete after the next one is dispatched | The events sorted by opcode, same order within one opcode |
| Event arguments | The REST endpoint of the Python GDS gives the text only | The text, which is formatted from the arguments; arguments are compared in the replay |
| Channel values and number of samples | Counters and system resources depend on the instant sampled | The set of channel names, and the last sample of `CommandsDispatched` and of the `Ref.typeDemo` channels |
| Bytes between frames on the uplink | The Python GDS writes a keep-alive text between frames, `fgds` does not; flight software discards it | The command packets or TC frames, byte for byte, in order |
| Text of a refusal | Two programs, two wordings | That the same requests are refused, and that nothing is sent for them |

## Replay: no difference accepted

The bytes `Ref` sent during the `fgds` run, cut at the last whole frame, are decoded by the Python deframer and
decoders (`FpFramerDeframer` or `SpacePacketSpaceDataLinkFramerDeframer`; `EventDecoder`, `ChDecoder`,
`PktDecoder`) and by a fresh `fgds` fed over TCP. Every event and every channel sample must be identical in every
field of the common form, in the same order. `fgds` must also report no decode error, no unknown packet and no
discarded byte.

## Synthetic replay: no difference accepted

Needs no `Ref`. For every event and every channel of the dictionary, the Python encoders (`EventEncoder`,
`ChEncoder`) build seven packets, each with one boundary value in every field: minimum, maximum, negative, NaN,
positive and negative infinity for floats, empty and full strings, first and last enum constants. The Python
decoders and `fgds` decode them; every field must be equal, the formatted text included.

## Not compared

- Rejected commands as records: `fgds` stores a refused request (`outcome: rejected`), the Python GDS stores
  nothing. Only the refusal is compared.
- A command with too few arguments: the Python GDS sends it and flight software answers `FORMAT_ERROR`; `fgds`
  refuses it. Not in the scenario.
- Events per packet: both decoders accept several events in one packet; the comparison only sees the resulting
  list of events, and `Ref` sends one per packet.
- Files, data products, parameters and sequences.
