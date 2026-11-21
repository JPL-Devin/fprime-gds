# Browser checks

`run.sh` is run by `scripts/check.sh` (or alone, from the repository root, with `FGDS`,
`REF_BIN`, `DICTIONARY`, `VENV` and `OUT` set). It installs Playwright 1.60.0 and its
Chromium in `target/browser-venv` on the first run and runs two scripts against the stock
binary `$FGDS` and the stock `Ref` (CCSDS framing).

`examples/browser-gds` is the stock GDS plus a tab whose element throws; only the check
of a failing tab runs it.

## `ui.py`: the page against a running `Ref`

- each tab loads and shows data; the status bar reports the link;
- `CMD_NO_OP` sent from the Commanding tab shows its events in the Events tab, in the
  severity colour;
- a text for an `I32` and 300 for a `U8` are refused in the page; enum inputs are sent;
- a string that contains HTML is shown as text in the command history, Events and Logs;
- a command history row loads into the form and is sent again;
- a channel value changes while the page stays open;
- the Events table has the records of the API, none repeated or missing; pause, resume, filter;
- every request goes to the GDS itself (content security policy);
- Uplink: a file chosen in the page is queued, resumed, finishes, and `Ref` has written the same bytes;
- Downlink: a file sent by `Ref` is listed as finished and downloads from the page with the same bytes;
- Data Products: a product `Ref` downlinks into the watched directory appears, and its detail opens decoded;
- a stream the server ends (`lagged`, or cut) is opened again after the last record delivered;
- no page error and no console error in any of the nine tabs;
- with the broken tab: an error panel names its plugin, the other tabs work, Events stays live.

Screenshots of each tab are written to `$OUT/ui`.

## `parity.py`: the same rows as the Python GDS page

A `Ref` session (the commands of `tests/compare/scenario.py` and a `CMD_NO_OP_STRING` that
contains HTML) is captured once through the proxy of `tests/compare`. The bytes are then
replayed into the new GDS and into the Python GDS (`fprime-gds -g html`), each with its
page open. For the new page, half arrives before a reload and half after, so the table is
built from the history and from the stream. The script reads the Events table (time,
identifier, name, severity, text) and the Channels table (last sample time, identifier,
name, value) of both pages and requires the same rows. Both tables are in
`$OUT/parity/tables.json`.

### Accepted differences

1. Time: the Python GDS page shows the flight time to the millisecond
   (`2026-11-21T15:52:41.748Z`), the new page to the microsecond, which is what the
   packet carries. The comparison cuts the new page's time to milliseconds, after checking
   that it shows microseconds.
2. Struct channel values: the Python GDS page shows a nested table,
   the new page one line (`[a, b]`, `{name: value}`). The comparison writes the Python
   value in the one-line form; the members and their values must be equal.

Column order, row order, colours and paging are not compared.

Event arguments are compared as part of the text: neither page has a column for them.
