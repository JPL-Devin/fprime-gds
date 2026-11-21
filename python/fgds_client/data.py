"""Events, channel updates and commands as test code sees them."""

import datetime
import functools
from enum import Enum


class _Bool:
    """A boolean argument: equal to the bool, with `.val` as the type objects of the Python GDS."""

    def __init__(self, value):
        self.val = value

    def __eq__(self, other):
        return self.val == other

    def __hash__(self):
        return hash(self.val)

    def __bool__(self):
        return self.val

    def __repr__(self):
        return repr(self.val)


def _arg(value):
    """An argument value that is the plain value (it compares, prints and encodes as it) and also has
    `.val`, which tests written for the Python GDS read."""
    if isinstance(value, bool):
        return _Bool(value)
    if value is None or hasattr(value, "val"):
        return value
    kind = type(value)
    plain = {"__repr__": kind.__repr__, "__str__": kind.__str__}
    return type(kind.__name__, (kind,), {"val": property(lambda self: kind(self)), **plain})(value)


class EventSeverity(Enum):
    COMMAND = 1
    ACTIVITY_LO = 2
    ACTIVITY_HI = 3
    WARNING_LO = 4
    WARNING_HI = 5
    DIAGNOSTIC = 6
    FATAL = 7

    def __str__(self):
        return f"EventSeverity.{self.name}"


@functools.total_ordering
class Time:
    """A flight time tag. Orders by base, seconds, microseconds, then context."""

    def __init__(self, base=0, context=0, seconds=0, useconds=0):
        self.base, self.context, self.seconds, self.useconds = base, context, seconds, useconds

    @classmethod
    def from_json(cls, value):
        if value is None:
            return cls()
        return cls(value["base"], value["context"], value["seconds"], value["microseconds"])

    def _key(self):
        return (self.base, self.seconds, self.useconds, self.context)

    def get_float(self):
        return self.seconds + self.useconds / 1_000_000

    def __eq__(self, other):
        return self._key() == other._key() if isinstance(other, Time) else self.get_float() == other

    def __lt__(self, other):
        return self._key() < other._key() if isinstance(other, Time) else self.get_float() < other

    def __hash__(self):
        return hash(self._key())

    def __sub__(self, other):
        """Seconds between two times, as a float."""
        return self.get_float() - (other.get_float() if isinstance(other, Time) else other)

    def get_datetime(self, tz=None):
        # Bases 1 (processor time) and 2 (workstation time) count from the Unix epoch.
        if self.base in (1, 2):
            return datetime.datetime.fromtimestamp(self.seconds, tz) + datetime.timedelta(microseconds=self.useconds)
        return None

    def to_readable(self, time_zone=None):
        moment = self.get_datetime(time_zone)
        if moment:
            return moment.isoformat(timespec="microseconds")
        return "%d: %d.%06ds, context=%d" % (self.base, self.seconds, self.useconds, self.context)

    def __str__(self):
        return "(%d(%d)-%d:%d)" % (self.base, self.context, self.seconds, self.useconds)

    __repr__ = __str__


class _Item:
    """Common part of the items built from one record."""

    def __init__(self, record):
        self.record = record
        self.seq = int(record["seq"])
        self.id = record["id"]
        self.time = Time.from_json(record.get("flight_time"))
        self.body = record.get("body") or {}
        self.name = self.body.get("name")

    def get_id(self):
        return self.id

    def get_time(self):
        return self.time

    def get_name(self):
        return self.name

    def __str__(self):
        return self.get_str()

    __repr__ = __str__


class EventData(_Item):
    def __init__(self, record):
        super().__init__(record)
        args = self.body.get("args") or {}
        self.arg_names = list(args) if isinstance(args, dict) else []
        self.args = [_arg(value) for value in (args.values() if isinstance(args, dict) else args)]
        self.display_text = self.body.get("display_text", "")
        severity = self.body.get("severity")
        self.severity = EventSeverity[severity] if severity in EventSeverity.__members__ else severity

    def get_args(self):
        """Argument values in dictionary order."""
        return self.args

    def get_severity(self):
        return self.severity

    def get_display_text(self):
        return self.display_text

    def get_dict(self, time_zone=None):
        return {
            "time": self.time.to_readable(time_zone),
            "raw_time": str(self.time),
            "name": self.name,
            "id": self.id,
            "severity": str(self.severity),
            "args": [a.val if isinstance(a, _Bool) else a for a in self.args],
            "display_text": self.display_text,
        }

    def get_str(self, time_zone=None, verbose=False, csv=False):
        when, raw = self.time.to_readable(time_zone), str(self.time)
        if verbose and csv:
            return f"{when},{raw},{self.name},{self.id},{self.severity},{self.display_text}"
        if verbose:
            return f"{when}: {self.name} ({self.id}) {raw} {self.severity} : {self.display_text}"
        if csv:
            return f"{when},{self.name},{self.severity},{self.display_text}"
        return f"{when}: {self.name} {self.severity} : {self.display_text}"


class ChData(_Item):
    def __init__(self, record):
        super().__init__(record)
        self.val = self.body.get("value")
        self.display_text = self.body.get("display_text", "")

    def get_val(self):
        return self.val

    def get_display_text(self):
        return self.display_text

    def get_dict(self, time_zone=None):
        return {
            "time": self.time.to_readable(time_zone),
            "raw_time": str(self.time),
            "name": self.name,
            "id": self.id,
            "display_text": self.display_text,
        }

    def get_str(self, time_zone=None, verbose=False, csv=False):
        when, raw = self.time.to_readable(time_zone), str(self.time)
        if verbose and csv:
            return f"{when},{raw},{self.name},{self.id},{self.display_text}"
        if verbose:
            return f"{when}: {self.name} ({self.id}) {raw} {self.display_text}"
        if csv:
            return f"{when},{self.name},{self.display_text}"
        return f"{when}: {self.name} = {self.display_text}"


class CmdData(_Item):
    """A command sent through the server, by any client."""

    def __init__(self, record):
        super().__init__(record)
        args = self.body.get("args") or {}
        self.args = [_arg(value) for value in (args.values() if isinstance(args, dict) else args)]
        self.client = self.body.get("client")
        self.outcome = self.body.get("outcome")

    def get_args(self):
        return self.args

    def get_str(self, time_zone=None, verbose=False, csv=False):
        return f"{self.record.get('ground_time')}: {self.name} ({self.id}) {self.args} {self.outcome}"


KINDS = {"fprime.event": EventData, "fprime.channel": ChData, "fprime.command": CmdData}
