"""Python client for a running `fgds`: commands, records, awaits and test assertions.

Everything goes through the public HTTP API and WebSocket stream of `fgds`
(`/api/v1`); only the Python standard library is used.
"""

from . import predicates
from .client import API_VERSION, ApiError, Client, VersionMismatch
from .data import ChData, CmdData, EventData, EventSeverity, Time
from .test_api import History, TestApi

__all__ = [
    "API_VERSION",
    "ApiError",
    "ChData",
    "Client",
    "CmdData",
    "EventData",
    "EventSeverity",
    "History",
    "TestApi",
    "Time",
    "VersionMismatch",
    "predicates",
]
