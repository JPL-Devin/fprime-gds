# The first bytes of a connection

`run.sh` starts the stock `fgds` and `Ref` (CCSDS) eight times, with a proxy between them that keeps the bytes.
For each start `check.py` requires that the raw log of the run is the byte stream `Ref` wrote, and that the event
records are the event packets of that stream, one for one.

`Ref` prints its two version events (`CdhCore.version.FrameworkVersion`, `ProjectVersion`) at every start and, in a
few starts out of a hundred, does not write them to the socket; the Python GDS then lacks them as well. The check
reports how often that happened and does not fail on it.
