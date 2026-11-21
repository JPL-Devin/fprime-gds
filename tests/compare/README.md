# Comparison with the Python GDS

Section 6.1 of the design: the current `fprime-gds` is the reference for what `fgds` must report.
`compare.py` runs both and fails on any difference that is not listed here. `scripts/check.sh` calls it.

| File | Role |
|---|---|
| `scenario.py` | The scenario (commands, timing), the TCP proxy that captures both directions, the common form |
| `python_gds.py` | The Python GDS side: `fprime-gds` headless plus a `StandardPipeline` client; needs the fprime virtual environment |
| `rust_gds.py` | The `fgds` side: `fgds run` and its HTTP API; standard library only |
| `compare.py` | Runs both sides, compares, prints one line per comparison |

## Scenario

`Ref` (built with F Prime framing by `scripts/build-ref.sh`) connects to the GDS through the capturing proxy.
Once telemetry arrives the GDS sends `CMD_NO_OP`, `CMD_NO_OP_STRING "hello"` and `CMD_TEST_CMD_1 1 2.5 3`
(`I32`, `F32`, `U8`), 0.5 s apart, and the run ends 4 s later.

## Common form

One JSON file per run: `events` (identifier, name, severity, flight time, arguments, formatted text), `channels`
(identifier, name, flight time, value, formatted text), `commands` (name, opcode, arguments). The command bytes on
the link are taken from the proxy's capture. Integers are written as text (64-bit safe), non-finite floats by name,
arguments as a list in declaration order, flight time as `[base, context, seconds, microseconds]`.

## Replay: no difference accepted

The bytes `Ref` sent during the `fgds` run are decoded by the Python decoders (`FpFramerDeframer`, `EventDecoder`,
`ChDecoder`, `PktDecoder`) and by a fresh `fgds` fed over TCP. Every event and every channel sample must be
identical in every field of the common form, in the same order. `fgds` must also report no decode error, no
unknown packet and no discarded byte.

## Live: accepted differences

Each GDS drives its own run of `Ref`, so the two runs are not the same data.

| Difference | Why it is accepted | What is still compared |
|---|---|---|
| Flight time and ground time | Two runs at two moments | Everything else in the compared items |
| Events not caused by the scenario (start-up, health) | Their number depends on when the GDS connected | The command dispatcher's events (`OpCodeDispatched`, `NoOp...`, `OpCodeCompleted`, ...): same list, same order, same arguments and text |
| Channel values and number of samples | Counters and system resources depend on the instant sampled | The set of channel names received, and the last `CommandsDispatched` sample (determined by the scenario) |
| Bytes between frames on the uplink | The Python GDS writes a keep-alive text (`sitting well`) between frames, `fgds` does not; flight software discards it | The command packets, byte for byte, in order |

Not compared in this slice: files, data products, parameters and sequences (later milestones), and commands with
enum, array or struct arguments (covered by unit tests of the encoder, not yet by the scenario).
