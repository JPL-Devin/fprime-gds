"""
catalog.py:

In-memory catalog of data products found among downlinked files, for the GDS Data Products tab.

The catalog registers with the FileDownlinker (a HandlerRegistrar) and, for every finished downlink, sniffs the
data product header (including its CRC) through DataProductDecoder.decode_header(). Detected products are listed
with their container (resolved through the dictionary) and header time stamp. Decoding runs on one daemon worker
thread fed by a bounded queue, either on request (REST) or automatically when the auto-decode flag is set, and
writes <ContainerShortName>_<sec:08d>_<usec:08d>.json next to the .fdp (the same name `fprime-dp decode
--rename-id` produces).

The catalog never raises towards the downlinker and is a no-op when the dictionary has no data product containers.
"""
import collections
import dataclasses
import datetime
import enum
import json
import logging
import os
import pathlib
import queue
import stat
import threading
from typing import Optional

import fprime_gds.common.handlers
from fprime_gds.common.dp.common import (
    ChecksumConfig,
    dp_container_short_name,
    dp_json_filename,
    get_dp_header_type,
)
from fprime_gds.common.dp.decoder import CRCError, DataProductDecoder
from fprime_gds.common.files.helpers import TransmitFile
from fprime_gds.common.models.serialize.type_exceptions import DeserializeException

LOGGER = logging.getLogger("dataproducts")
CATALOG_LIMIT = 1000
QUEUE_LIMIT = 64


class DecodeQueueFull(Exception):
    """The bounded decode queue is full; the request was not queued"""


class UnknownProduct(KeyError):
    """No product of that name (or no decoded output for it) in the catalog"""


class DecodeState(str, enum.Enum):
    """Decode state of a listed product (distinct from the header's Fw.DpState)"""

    DETECTED = "DETECTED"
    QUEUED = "QUEUED"
    DECODING = "DECODING"
    DECODED = "DECODED"
    FAILED = "FAILED"


ACTIVE_STATES = (DecodeState.QUEUED, DecodeState.DECODING)


@dataclasses.dataclass
class DpEntry:
    """One detected data product"""

    name: str
    path: pathlib.Path
    container_id: int
    container: Optional[str]
    short_name: Optional[str]
    priority: int
    proc_types: int
    dp_state: str
    time_seconds: int
    time_microseconds: int
    time_base: int
    time_context: int
    data_size: int
    file_size: int
    received: str
    json_name: str
    state: DecodeState = DecodeState.DETECTED
    error: Optional[str] = None
    generation: int = 0

    def to_dict(self) -> dict:
        """REST representation"""
        return {
            "name": self.name,
            "container_id": self.container_id,
            "container": self.container,
            "short_name": self.short_name,
            "priority": self.priority,
            "proc_types": self.proc_types,
            "dp_state": self.dp_state,
            "time": {
                "base": self.time_base,
                "context": self.time_context,
                "seconds": self.time_seconds,
                "microseconds": self.time_microseconds,
            },
            "data_size": self.data_size,
            "file_size": self.file_size,
            "received": self.received,
            "state": self.state.value,
            "json_name": self.json_name,
            "error": self.error,
        }


def _utc_iso(value) -> str:
    """ISO-8601 UTC string for a datetime or POSIX timestamp (now when None)"""
    if value is None:
        value = datetime.datetime.now(datetime.timezone.utc)
    elif not isinstance(value, datetime.datetime):
        value = datetime.datetime.fromtimestamp(float(value), datetime.timezone.utc)
    elif value.tzinfo is None:
        value = value.astimezone(datetime.timezone.utc)
    return value.isoformat()


class DataProductCatalog(fprime_gds.common.handlers.DataHandler):
    """Bounded catalog of data products plus a single decode worker. Thread-safe."""

    def __init__(self, dictionaries, directory: pathlib.Path, catalog_limit: int = CATALOG_LIMIT, queue_limit: int = QUEUE_LIMIT):
        super().__init__()
        self._dictionaries = dictionaries
        self.directory = pathlib.Path(directory)
        self._catalog_limit = catalog_limit
        self._entries: "collections.OrderedDict[str, DpEntry]" = collections.OrderedDict()
        self._json_owner: dict = {}  # JSON basename -> entry name, live entries only
        self._lock = threading.Lock()
        self._queue: queue.Queue = queue.Queue(maxsize=queue_limit)
        self._auto_decode = False
        self._worker = None
        self._enabled = False
        self._header_size = 0
        try:
            if dictionaries is not None and bool(dictionaries.dp_container_id):
                self._header_size = get_dp_header_type()().getMaxSize()
                self._enabled = True
        except Exception as exc:
            LOGGER.warning("Data products disabled, header type not available: %s", exc)
            self._enabled = False

    @property
    def enabled(self) -> bool:
        return self._enabled

    @property
    def auto_decode(self) -> bool:
        return self._auto_decode

    def disable(self):
        """Turn the catalog into a no-op (used when setup fails after construction)"""
        self._enabled = False

    # ------------------------------------------------------------------ observer API (downlinker threads)

    def data_callback(self, data, sender=None):
        """Called by FileDownlinker.finish() with the finished TransmitFile"""
        if not self._enabled or not isinstance(data, TransmitFile):
            return
        try:
            if data.size <= 0 or data.seek < data.size:
                return  # cancelled, timed out or errored: not a complete file
            path = pathlib.Path(data.destination)
            if path.stat().st_size < data.size:
                return
            self._consider(path, _utc_iso(data.end), from_scan=False)
        except Exception as exc:
            LOGGER.warning("Data product detection failed for %s: %s", getattr(data, "destination", data), exc)

    def scan(self) -> int:
        """Scan the downlink directory once (oldest first). Returns the number of products listed."""
        if not self._enabled:
            return 0
        try:
            candidates = list(self.directory.iterdir())
        except OSError as exc:
            LOGGER.warning("Data product scan of %s failed: %s", self.directory, exc)
            return 0
        stamped = []
        for path in candidates:
            try:
                status = path.stat()
            except OSError:
                continue  # vanished between iterdir and stat
            if stat.S_ISREG(status.st_mode):
                stamped.append((status.st_mtime, path))
        count = 0
        for mtime, path in sorted(stamped, key=lambda item: item[0]):
            if self._consider(path, _utc_iso(mtime), from_scan=True):
                count += 1
        return count

    # ------------------------------------------------------------------ detection

    def _sniff(self, path: pathlib.Path) -> Optional[dict]:
        """Header as a jsonable dict when the file starts with a valid, CRC-checked DP header, else None"""
        try:
            with open(path, "rb") as file_handle:
                return DataProductDecoder(self._dictionaries, str(path)).decode_header(file_handle).to_jsonable()
        except (CRCError, DeserializeException, OSError, ValueError, IndexError) as exc:
            LOGGER.debug("%s is not a data product: %s", path.name, exc)
            return None

    def _consider(self, path: pathlib.Path, received: str, from_scan: bool) -> bool:
        """Sniff a complete file and list it when it is a data product. Returns True when listed."""
        try:
            file_size = path.stat().st_size
        except OSError:
            return False
        header = self._sniff(path)
        if header is None:
            return False
        container_id = header["Id"]["value"]
        container = self._dictionaries.dp_container_id.get(container_id)
        short_name = dp_container_short_name(container) if container is not None else None
        time = header["Time"]
        seconds, microseconds = time["seconds"], time["microseconds"]
        data_size = header["DataSize"]["value"]
        name = path.name
        json_name = dp_json_filename(short_name, seconds, microseconds) if short_name else f"{path.stem}.json"
        entry = DpEntry(
            name=name,
            path=path,
            container_id=container_id,
            container=container.get_name() if container is not None else None,
            short_name=short_name,
            priority=header["Priority"]["value"],
            proc_types=header["ProcTypes"]["value"],
            dp_state=str(header["DpState"]["value"]),
            time_seconds=seconds,
            time_microseconds=microseconds,
            time_base=time["base"],
            time_context=time["context"],
            data_size=data_size,
            file_size=file_size,
            received=received,
            json_name=json_name,
        )
        if self._header_size + data_size + ChecksumConfig.CHECKSUM_LEN > file_size:
            entry.error = "file shorter than header DataSize"
        json_path = self.directory / json_name
        existing_json_matches = from_scan and json_path.is_file() and self._json_matches(json_path, entry)
        with self._lock:
            previous = self._entries.get(name)
            if previous is not None:
                entry.generation = previous.generation + 1
                if self._json_owner.get(previous.json_name) == name:
                    self._json_owner.pop(previous.json_name)
            self._entries[name] = entry
            self._entries.move_to_end(name)
            if from_scan and json_path.is_file():
                if json_name == name:
                    entry.error = "output name equals input"
                elif json_name in self._json_owner:
                    entry.error = f"output {json_name} already produced by {self._json_owner[json_name]}"
                elif existing_json_matches:
                    entry.state = DecodeState.DECODED
                    self._json_owner[json_name] = name
                else:
                    entry.error = f"existing {json_name} does not match this product, decode again"
            elif self._auto_decode:
                try:
                    self._request_decode_locked(entry)
                except DecodeQueueFull:
                    LOGGER.warning("Decode queue full, %s left for manual decode", name)
            self._evict_locked()
        LOGGER.info("Data product %s (%s) %s", name, short_name or container_id, entry.state.value)
        return True

    @staticmethod
    def _json_matches(json_path: pathlib.Path, entry: DpEntry) -> bool:
        """True when an existing JSON file is a complete decode of this product (same container id and time)"""
        try:
            with open(json_path) as file_handle:
                header = json.load(file_handle)["Header"]
            return (
                header["Id"]["value"] == entry.container_id
                and header["Time"]["seconds"] == entry.time_seconds
                and header["Time"]["microseconds"] == entry.time_microseconds
            )
        except (OSError, ValueError, KeyError, TypeError):
            return False

    def _evict_locked(self):
        """Drop the oldest non-active entries while over the catalog limit"""
        while len(self._entries) > self._catalog_limit:
            victim = next((key for key, entry in self._entries.items() if entry.state not in ACTIVE_STATES), None)
            if victim is None:
                LOGGER.warning("Data product catalog over %d entries, all active", self._catalog_limit)
                return
            evicted = self._entries.pop(victim)
            if self._json_owner.get(evicted.json_name) == victim:
                self._json_owner.pop(evicted.json_name, None)

    # ------------------------------------------------------------------ decode requests

    def _request_decode_locked(self, entry: DpEntry) -> str:
        """Queue a decode of entry (caller holds the lock). Returns the resulting state name."""
        if entry.state in ACTIVE_STATES:
            return entry.state.value
        previous_state = entry.state
        entry.state = DecodeState.QUEUED
        entry.error = None
        try:
            self._queue.put_nowait((entry.name, entry.generation))
        except queue.Full:
            entry.state = previous_state
            entry.error = "decode queue full"
            raise DecodeQueueFull(entry.name)
        return entry.state.value

    def request_decode(self, name: str) -> str:
        """Queue a decode of the named product. Raises UnknownProduct or DecodeQueueFull."""
        with self._lock:
            entry = self._entries.get(name) if self._enabled else None
            if entry is None:
                raise UnknownProduct(name)
            return self._request_decode_locked(entry)

    def json_path(self, name: str) -> pathlib.Path:
        """Path of the decoded JSON of a DECODED product. Raises UnknownProduct otherwise."""
        with self._lock:
            entry = self._entries.get(name) if self._enabled else None
            if entry is None or entry.state != DecodeState.DECODED:
                raise UnknownProduct(name)
            return self.directory / entry.json_name

    def set_auto_decode(self, value: bool):
        """Set the server-side auto-decode flag (ignored when disabled). Existing entries are not caught up."""
        if not self._enabled:
            return
        with self._lock:
            self._auto_decode = bool(value)

    def list(self) -> list:
        """Products as plain dicts, oldest first"""
        with self._lock:
            return [entry.to_dict() for entry in self._entries.values()]

    # ------------------------------------------------------------------ decode worker

    def start(self):
        """Start the single daemon decode worker"""
        if not self._enabled or self._worker is not None:
            return
        self._worker = threading.Thread(target=self._run, name="dp-decode", daemon=True)
        self._worker.start()

    def _run(self):
        while True:
            try:
                self.decode_next()
            except Exception:
                LOGGER.exception("Data product decode worker error")

    def decode_next(self, block: bool = True, timeout: Optional[float] = None) -> bool:
        """Decode one queued product. Returns False when the queue was empty."""
        try:
            name, generation = self._queue.get(block, timeout)
        except queue.Empty:
            return False
        try:
            self._decode_one(name, generation)
        finally:
            self._queue.task_done()
        return True

    def _decode_one(self, name: str, generation: int):
        with self._lock:
            entry = self._entries.get(name)
            if entry is None or entry.generation != generation:
                return
            entry.state = DecodeState.DECODING
            path, json_name = entry.path, entry.json_name
        tmp_path = self.directory / f"{json_name}.tmp"
        try:
            result = DataProductDecoder(self._dictionaries, str(path)).decode()
            with open(tmp_path, "w") as file_handle:
                json.dump(result, file_handle, indent=2, default=str)
        except Exception as exc:
            self._remove(tmp_path)
            LOGGER.warning("Decoding %s failed: %s: %s", name, type(exc).__name__, exc)
            self._fail(name, generation, f"{type(exc).__name__}: {exc}")
            return
        with self._lock:
            entry = self._entries.get(name)
            if entry is None or entry.generation != generation:
                self._remove(tmp_path)
                return
            if json_name == name:
                self._remove(tmp_path)
                entry.state, entry.error = DecodeState.FAILED, "output name equals input"
            elif self._json_owner.get(json_name) not in (None, name):
                self._remove(tmp_path)
                entry.state = DecodeState.FAILED
                entry.error = f"output {json_name} already produced by {self._json_owner[json_name]}"
            else:
                try:
                    os.replace(tmp_path, self.directory / json_name)
                except OSError as exc:
                    self._remove(tmp_path)
                    entry.state, entry.error = DecodeState.FAILED, f"{type(exc).__name__}: {exc}"
                    return
                entry.state, entry.error = DecodeState.DECODED, None
                self._json_owner[json_name] = name

    def _fail(self, name: str, generation: int, error: str):
        with self._lock:
            entry = self._entries.get(name)
            if entry is None or entry.generation != generation:
                return
            entry.state, entry.error = DecodeState.FAILED, error

    @staticmethod
    def _remove(path: pathlib.Path):
        try:
            os.remove(path)
        except OSError:
            pass  # already gone or never written
