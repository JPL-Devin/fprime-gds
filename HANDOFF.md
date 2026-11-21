# Handoff

## State
- Design baseline approved by the user on 2026-11-21: `docs/design/requirements-and-architecture.md`.
- First slice built (M0 and the smallest part of M1): the Cargo workspace of section 4.10 with `fgds-dictionary`, `fgds-core`, `fgds-shell`, `fgds`, the plugins `comm-tcp`, `framing-fprime`, `storage-memory`, `events`, `channels`, `commands`, and `examples/custom-gds`.
- `scripts/check.sh` runs every check and passes locally: format, lints, tests; `fgds` against `Ref` (commands, events, channels, WebSocket replay then live); the comparison with the Python GDS, live and on replayed bytes (`tests/compare/README.md` lists the accepted differences); the example binary.
- `.github/workflows/check.yml` calls the same script. It passes on this branch.
- Where the code differs from the design, and what to change in the design before more plugins are built: `docs/design/deviations.md`. The design document itself is unchanged.

## How to run
- `FPRIME=<fprime checkout with fprime-venv> scripts/check.sh`. The first run builds `Ref` under `target/ref` (several minutes).
- `Ref` is built twice: as it is (CCSDS framing, `tests/ccsds/run.sh`, plugin `framing-ccsds`) and with F Prime framing (`scripts/build-ref.sh` switches a copy of fprime from `ComCcsds` to `ComFprime`; `scripts/check.sh`). Both are compared with the Python GDS.
- `cargo run -p fgds -- run --dictionary <RefTopologyDictionary.json>`, then `Ref -a 127.0.0.1 -p 50000`; the API is on `http://127.0.0.1:5000/api/v1`.

## Known gaps in the slice
- Storage failure: records are broadcast unstored and a gap record is written on recovery; no buffer, retry or commit time-out (deviation 8). To finish with `storage-sqlite`.
- A raw send (`POST /api/v1/raw`) is not recorded (deviations 4 and 5).
- Format strings of array elements and struct members are not applied to display text.
- No browser test; the page shell only lists and loads tab modules.

## Rules for every session working here
- The design document is the reference. A change to a core interface (section 4.2 to 4.5) or to a requirement goes through the orchestrating session, not into a feature branch on its own.
- Ground truth (section 6.1): data seen when running the `Ref` deployment must match what the current `fprime-gds` reports, and parsers are tested against the fixtures in the Python `fprime-gds` repository (`test/fprime_gds/`).
- Work on a branch created from this one. Do not open pull requests and do not write to any other repository.
- Scope for now is a local run: no tokens, roles or sign-in (section 3.9, deferred list).

## Decisions
Section 7 of the design document.

## Deferred
Access control, the stall watchdog, the load test, project rules checked before a send, `pip install` packaging, a dedicated repository.

## Next steps
1. Review `docs/design/deviations.md` and update the design ("To change in the design").
2. Then one branch per plugin crate, per the milestones of section 6.3; `storage-sqlite` first, since it completes the pipeline.
