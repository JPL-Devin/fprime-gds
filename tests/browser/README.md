# Browser checks

`run.sh` is run by `scripts/check.sh` (or alone, from the repository root, with `FGDS`,
`REF_BIN`, `DICTIONARY` and `OUT` set). It installs Playwright 1.60.0 and its
Chromium in `target/browser-venv` on the first run and runs `ui.py` against the stock
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
