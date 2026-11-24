# `fgds tui`: three panes on a running GDS

`fgds tui` is [`fgds repl`](repl.md) on a full screen: the same command line, with the events
and the channels in panes of their own. It is an extra of fgds: the Python GDS has no counterpart.

```
fgds run --dictionary RefTopologyDictionary.json      # one terminal: the GDS
fgds tui                                              # another: the screen
fgds tui --url http://127.0.0.1:5000                  # a GDS at another address
```

```
┌ Events and commands ──────────────────────────────┐┌ Channels: 21 of 21 ─────────────────────────┐
│fgds> cmd CdhCore.cmdDisp.CMD_NO_OP                ││CdhCore.cmdDisp.CommandsDispatched 3  11:02:4│
│2026-11-24T11:02:43.772634Z CMD   CdhCore.cmdDisp.C││Ref.sendBuffComp.SendState  SEND_IDLE 11:02:4│
│2026-11-24T11:02:43.772713 EVENT ACTIVITY_HI CdhCor││Ref.systemResources.CPU  1.01 percent 11:02:4│
└───────────────────────────────────────────────────┘└─────────────────────────────────────────────┘
┌ Tab completes, PgUp/PgDn events, Alt-Up/Down channels, `help`, `quit` ───────────────────────────┐
│fgds>                                                                                             │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

## The panes

- **Events and commands**: what `fgds repl` prints above its prompt, except channel samples:
  events, the commands of every client, each line typed and its answer (`cmds`, `help`, a
  refusal). Long lines continue on the next row. The last 5000 lines are kept; Page Up and
  Page Down move by half a pane, and the title says how far back the pane is. A typed line
  returns it to the end.
- **Channels**: one row per channel, with its latest value and the time of it, sorted by name.
  Alt-Up and Alt-Down move the table when it is longer than the pane.
  In a narrow pane the time is left out first, then the start of the names (`…cmdDisp.CommandErrors`).
- **Command line**: takes what [`fgds repl` takes](repl.md#what-can-be-typed). Tab completes
  from the dictionary as there; when several names fit, what they share is completed and the
  names are listed above the line until the next key.

## Keys

| Key | Does |
|---|---|
| Enter | Runs the line |
| Tab | Completes |
| Up, Down | Lines typed before, in this session |
| Left, Right, Home, End, Ctrl-A, Ctrl-E | Move in the line |
| Backspace, Delete, Ctrl-U, Ctrl-K | Erase: a character, to the start, to the end |
| Ctrl-C | Empties the line |
| Page Up, Page Down | Scroll the events pane |
| Alt-Up, Alt-Down | Scroll the channel table |
| Ctrl-D on an empty line, `quit`, `exit` | Leave; the terminal is as it was |

## Filters

`mute`, `unmute`, `subscribe`, `unsubscribe`, `filters` and `clear-filters` are those of the
prompt. On events they decide what enters the events pane from then on. On channels they
decide which rows the table shows: every channel is still recorded, and the title gives the
rows shown and the channels received (`Channels: 1 of 23`).

## Limits

- Those of [`fgds repl`](repl.md#limits): a running GDS is needed, and only records that arrive after the start are shown.
- It needs a terminal on its input and output; without one it stops and names `fgds repl`, which prints the stream.
- The history of typed lines is not kept across sessions, and is not the file of `fgds repl`.
- One line is asked of the GDS at a time. A `cmd` or `raw-uplink` entered while the one before is not answered is refused on the screen and not kept; the title of the command pane names the line waited for. Lines that ask the GDS nothing (`help`, filters) run meanwhile.
- Leaving does not wait for the answer: nothing is posted after `quit`, and the line that was not answered is named on the terminal, since the GDS may have sent it.
- Text is escaped as in [`fgds repl`](repl.md#what-is-printed); wide characters take two columns, in the panes and for the cursor.
- SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGALRM, SIGUSR1 and SIGUSR2 leave the alternate screen and put the terminal back, as `quit` does.
- A GDS that stops, and another dictionary when it is back, are handled as in `fgds repl`.
- No mouse, and no choice of layout: the events pane has 62% of the width.
- `NO_COLOR` set to anything turns the colours of event severities off.
