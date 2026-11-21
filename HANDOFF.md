# Handoff

## State
- Design baseline approved by the user on 2026-11-21: `docs/design/requirements-and-architecture.md`.
- No code yet. Next: the thin end-to-end slice (section 6.2 step 2 of the design).

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
1. Thin slice: core, shell, dictionary, TCP adapter, F Prime framing, event decoding, command sending, in-memory storage, example project plugin, first comparison with `fprime-gds` on `Ref`.
2. Then one branch per plugin crate, per the milestones of section 6.3.
