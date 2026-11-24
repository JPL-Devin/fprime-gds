# `fgds repl`: a prompt on a running GDS

`fgds repl` is an interactive prompt for a GDS that is already running (`fgds run`). It sends
commands, and prints events, channel samples and the commands of other clients above the line
being typed. It is an extra of fgds: the Python GDS has no counterpart.

```
fgds run --dictionary RefTopologyDictionary.json      # one terminal: the GDS
fgds repl                                             # another: the prompt
fgds repl --url http://127.0.0.1:5000                 # a GDS at another address
```

Without `--url` the address is the configured `--bind`, as for `fgds command-send`.

```
fgds> mute channels
muted: channels
fgds> cmd CdhCore.cmdDisp.CMD_NO_OP
2026-11-24T08:24:40.675158Z CMD   CdhCore.cmdDisp.CMD_NO_OP {} by fgds-repl-31659: sent, opcode 16777216, packet 000001000000
2026-11-24T08:24:40.675168 EVENT COMMAND CdhCore.cmdDisp.OpCodeDispatched: Opcode 0x1000000 dispatched to port 0
2026-11-24T08:24:40.675204 EVENT ACTIVITY_HI CdhCore.cmdDisp.NoOpReceived: Received a NO-OP command
2026-11-24T08:24:40.675210 EVENT COMMAND CdhCore.cmdDisp.OpCodeCompleted: Opcode 0x1000000 completed
fgds> cmd CdhCore.cmdDisp.CMD_NO_OP 1
not sent: CdhCore.cmdDisp.CMD_NO_OP takes 0 arguments, 1 given
usage: cmd CdhCore.cmdDisp.CMD_NO_OP
```

## What can be typed

| Line | Does |
|---|---|
| `cmd <name> [argument ...]` | Sends a command. Arguments in order, as `fgds command-send --arguments` takes them, or all of them as `<parameter>=<value>` in any order. Quote an argument that holds spaces: `cmd CdhCore.cmdDisp.CMD_NO_OP_STRING "hello there"` |
| `raw-uplink <hexadecimal bytes>` | Sends the bytes as one packet, unchecked: `raw-uplink 00000001 01000000` |
| `cmds [pattern]`, `events [pattern]`, `channels [pattern]` | Lists the dictionary of the GDS. Also `dict commands\|events\|channels [pattern]` |
| `help [name]` | The list of lines, or one command, event or channel with its parameters |
| `mute events\|channels\|<pattern>`, `unmute ...` | Stops printing all events, all channel samples, or the names that match |
| `subscribe <pattern>`, `unsubscribe <pattern>` | With a subscription, only the names that match one are printed |
| `filters`, `clear-filters` | Lists the mutes and subscriptions; removes them all |
| `quit`, `exit`, Ctrl-D | Leaves. Ctrl-C drops the line being typed |

- A pattern matches a whole name: `*` is any text, `?` one character (`mute Ref.systemResources.CPU_*`).
  In a list, a pattern without `*` or `?` is looked for anywhere in the name, in any case (`cmds no_op`).
- Tab completes the first word, the names of the dictionary (by their start or by their last part:
  `cmd CMD_NO` and Tab) and the values of enumeration and boolean arguments.
- The history is kept in `~/.fgds_history`; the arrow keys and Ctrl-R go through it.

## Commands

The prompt sends `{"name", "args"}` to `POST /api/v1/commands` of the GDS, as `fgds command-send`
does. The GDS checks the arguments against its dictionary, encodes the packet and records the
command; the prompt encodes nothing. A command the GDS refuses is not sent: the prompt prints the
reason of the GDS and how the command is typed.

After a send the prompt prints the command record (opcode and packet). The events that name the
opcode of a command sent from this prompt (`OpCodeDispatched`, `OpCodeCompleted`, `OpCodeError` of
the command dispatcher) are printed for 10 seconds whatever the filters. Commands sent by other
clients (the browser, `fgds command-send`, a test) are printed with the name of the client.

## Without a terminal

When the standard input is not a terminal (`fgds repl < /dev/null`, a pipe, a service), nothing
is read: events, channel samples and commands are printed, one per line, until interrupted.

## Limits

- The prompt needs a running GDS; it does not start one in its own process.
- Only records that arrive after the prompt started are printed; `fgds events` and the browser show the history.
- A typed line longer than the terminal is wide leaves its first rows on the screen when an event is printed above it.
- If the GDS stops, the prompt says so once, tries again after 1 s, 2 s, 4 s and so on up to 30 s, and says once that it is back. A command typed meanwhile gets `not sent` when no GDS took the connection, and `no answer, it may have been sent` when the request left and no answer came. An error answer of the GDS with a status of 500 or above (`link down`, where the GDS records the command as rejected) is printed as `refused: 503 Service Unavailable: link down`.
- The dictionary is read again each time the stream is opened, and replaces the one in use when its content differs, whatever its version says. A name the prompt does not know is still sent: the GDS decides.

## What is printed

Text from flight, from the GDS and from the dictionary is never written to the terminal as it is, with or without a terminal on the output.
Control characters are printed as text (`\x1b`, `\r`, `\n`, `\t`, others `\xNN`), a backslash is doubled, and the characters that change the direction of text or have no width (U+2028, U+2029, U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069, U+200B to U+200D, U+2060, U+FEFF) are printed as `\u{XXXX}`.

## Signals

An error before the prompt (no GDS at `--url`, an error answer of the GDS) is printed escaped by the same rule, also without a terminal. A character the rule would change does not enter the line being typed, typed or pasted, and Ctrl-V and Ctrl-Q insert nothing; a line of `~/.fgds_history` that holds one is not recalled. Bytes that are not UTF-8 drop the line (`line dropped: ...`) and the prompt goes on.

A paste is read key by key between the marks the terminal puts around it (bracketed paste), marks inside the pasted text included. In a paste no key is a command: a line break or a tab becomes a space, so nothing pasted runs before Enter is pressed, and a paste of several lines is one line to look at. A paste whose end mark does not come is over half a second after its last key. On a terminal without bracketed paste, a pasted line break is Enter: the first line runs and the editor discards the lines after it.

SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGALRM, SIGUSR1 and SIGUSR2, and no other signal, put the terminal back (with bracketed paste off) as it was before the process ends with status 128 plus the signal. Each line is added to `~/.fgds_history` when it is entered.
