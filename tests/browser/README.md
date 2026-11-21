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
identifier, name, severity, text, colour) and the Channels table (last sample time, identifier,
name, value, colour) of both pages and requires the same rows in the same order. Both tables are in
`$OUT/parity/tables.json`.

### Accepted differences

None in the cells compared. Rows are compared in the order shown (events oldest first,
channels by identifier), cell text as rendered (`innerText` of a visible tab) and the
background colour of each row, the four green in-limit channel rows included.

Presentation differences that stay:

1. The new page has a Ground time column. Its box hides it (kept across a reload); the
   remaining headers must be those of the Python GDS page.
2. The Python page shows 40 events per page; the new page shows them in one table
   (row limit setting). The script walks the Python pages.
3. An array or struct value is one cell of text in the new page (the heading text of the
   Python nested table, same quotes and spacing); the Python page can also expand it.
4. CSS class names and text colours differ; background colours are equal.

Event arguments are compared as part of the text: neither page has a column for them.
