"""The integration test API over `fgds`, with the operation names of the F Prime
`IntegrationTestAPI` (SCR-3).

One background thread reads the record stream from the sequence number the server
had when the API was set up ("from now", SCR-4) and files each event, channel
update and command into a history. Searches and assertions run here, on those
histories (SCR-6); clearing a history only forgets it in this client.
"""

import bisect
import datetime
import json
import os
import shutil
import subprocess
import tempfile
import threading
import time

from . import predicates
from .client import Client, Lagged
from .data import KINDS, ChData, CmdData, EventData, EventSeverity, Time

__test__ = False


class History:
    """Items of one type received by this client, ordered by flight time.

    Items with equal times keep their order of arrival. `arrivals` keeps every
    item in order of arrival, for searches that wait for new items.
    """

    def __init__(self, changed, by_time=True):
        self.changed, self.by_time = changed, by_time
        self.items, self.keys, self.arrivals = [], [], []

    def add(self, item):
        with self.changed:
            if self.by_time:
                at = bisect.bisect_right(self.keys, item.get_time()._key())
                self.keys.insert(at, item.get_time()._key())
                self.items.insert(at, item)
            else:
                self.items.append(item)
            self.arrivals.append(item)
            self.changed.notify_all()

    def size(self):
        with self.changed:
            return len(self.items)

    def retrieve(self, start=None):
        """Items from `start` on: an index, a predicate marking the first item, or all."""
        with self.changed:
            return self._from(start)

    def _from(self, start):
        if start is None:
            return list(self.items)
        if predicates.is_predicate(start):
            for index, item in enumerate(self.items):
                if start(item):
                    return self.items[index:]
            return []
        return self.items[start:]

    def clear(self, start=None):
        """Forget everything, or everything before the first item satisfying `start`."""
        with self.changed:
            kept = [] if start is None else self._from(start)
            self.items = kept
            self.keys = [item.get_time()._key() for item in kept] if self.by_time else []
            self.arrivals = list(kept)


class Dictionaries:
    """Names and identifiers of the dictionary the server loaded."""

    def __init__(self, client, dictionary_path=None):
        self._client, self._path = client, dictionary_path
        self.command_name = {d["name"]: d for d in client.definitions("commands")}
        self.command_id = {d["opcode"]: d for d in self.command_name.values()}
        self.event_name = {d["name"]: d for d in client.definitions("events")}
        self.event_id = {d["id"]: d for d in self.event_name.values()}
        self.channel_name = {d["name"]: d for d in client.definitions("channels")}
        self.channel_id = {d["id"]: d for d in self.channel_name.values()}

    @property
    def dictionary_path(self):
        """A file holding the dictionary: the given path, or a copy fetched from the server."""
        if self._path is None:
            with tempfile.NamedTemporaryFile("w", suffix=".json", prefix="fgds-dictionary-", delete=False) as out:
                json.dump(self._client.dictionary(), out)
                self._path = out.name
        return self._path


class TestApi:
    """Send commands, await and assert events and telemetry against a running `fgds`."""

    NOW = "NOW"

    def __init__(self, client=None, logpath=None, dictionary_path=None, deployment_config=None):
        self.deployment_config = deployment_config
        self._sequence_tempdirs = []
        self.client = client if isinstance(client, Client) else Client(client) if client else Client()
        self.logpath, self._dictionary_path = logpath, dictionary_path
        self._log_file = None
        self._changed = threading.Condition()
        self.event_history = History(self._changed)
        self.telemetry_history = History(self._changed)
        self.command_history = History(self._changed, by_time=False)
        self._histories = {EventData: self.event_history, ChData: self.telemetry_history, CmdData: self.command_history}
        self.latest_time = Time()
        self._dictionaries = None
        self._stop = threading.Event()
        self._reader = None
        self._reader_error = None
        self._last_seq = None
        self._stream = None

    # Life cycle.

    def setup(self):
        if self.logpath is not None:
            os.makedirs(self.logpath, exist_ok=True)
            name = datetime.datetime.now().strftime("test-%Y%m%d-%H%M%S-%f.log")
            self._log_file = open(os.path.join(self.logpath, name), "w", encoding="utf-8")
        self._dictionaries = Dictionaries(self.client, self._dictionary_path)
        self._last_seq = self.client.cursor()
        self.__log(f"Connected to {self.client.api}; reading records after {self._last_seq}")
        self._reader = threading.Thread(target=self._read, name="fgds-records", daemon=True)
        self._reader.start()
        return self

    def teardown(self):
        for directory in self._sequence_tempdirs:
            shutil.rmtree(directory, ignore_errors=True)
        self._sequence_tempdirs = []
        self._stop.set()
        if self._stream is not None:
            self._stream.close()
        if self._log_file is not None:
            self._log_file.close()
            self._log_file = None

    def __enter__(self):
        return self.setup()

    def __exit__(self, *_):
        self.teardown()

    def _read(self):
        """File every record after the starting cursor, resuming after the last one seen."""
        failures = 0
        while not self._stop.is_set():
            try:
                self._stream = self.client.stream(list(KINDS), after_seq=self._last_seq)
                for record in self._stream:
                    failures = 0
                    seq = int(record["seq"])
                    if seq <= self._last_seq:
                        continue
                    self._last_seq = seq
                    item = KINDS[record["kind"]](record)
                    self._histories[type(item)].add(item)
                    if isinstance(item, EventData):
                        self.__log(f"Received EVR: {item.get_str(verbose=True)}", sender="GDS")
            except Lagged:
                continue
            except Exception as error:  # noqa: BLE001 - reported to the test thread
                failures += 1
                if failures > 20:
                    self._reader_error = error
                    with self._changed:
                        self._changed.notify_all()
                    return
            self._stop.wait(0.1)

    def _alive(self):
        if self._reader_error is not None:
            raise ConnectionError(f"record stream of {self.client.api} lost: {self._reader_error}")
        if self._reader is None:
            raise RuntimeError("TestApi.setup() was not called")

    # Logging (SCR-5).

    def __log(self, message, sender="Test API"):
        if self._log_file is not None:
            stamp = datetime.datetime.now().isoformat(timespec="milliseconds")
            try:
                self._log_file.write(f"{stamp} [{sender}] {message}\n")
                self._log_file.flush()
            except ValueError:
                pass

    def log(self, msg, color=None):
        self.__log(str(msg), sender="API user")

    def start_test_case(self, case_name, case_id):
        self.__log(f"[STARTING CASE] {case_name} ({case_id})")
        self.get_latest_time()
        self.clear_histories()

    # Assertions.

    def test_assert(self, value, msg="", expect=False):
        kind = "User expectation" if expect else "User assertion"
        text = f"{kind} {'succeeded' if value else 'failed'}: {msg}"
        self.__log(text)
        if not expect:
            assert value, text
        return value

    def predicate_assert(self, predicate, value, msg="", expect=False):
        return self.__assert_pred("User", predicate, value, msg, expect)

    def __assert_pred(self, name, predicate, value, msg="", expect=False):
        name += " expectation" if expect else " assertion"
        described = predicates.get_descriptive_string(value, predicate)
        passed = bool(predicate(value))
        self.__log(f"{name} {'succeeded' if passed else 'failed'}: {msg}\nassert {described}")
        if not expect:
            assert passed, described
        return passed

    # Histories.

    @property
    def dictionaries(self):
        return self._dictionaries

    def get_latest_time(self):
        for history in (self.event_history, self.telemetry_history):
            items = history.retrieve()
            if items:
                self.latest_time = max(self.latest_time, items[-1].get_time())
        return self.latest_time

    def clear_histories(self, time_stamp=None):
        """Forget what this client received; nothing is deleted on the server (SCR-4)."""
        start = None
        if time_stamp is not None:
            reached = predicates.greater_than_or_equal_to(time_stamp)
            start = lambda item: reached(item.get_time())  # noqa: E731
        self.event_history.clear(start)
        self.telemetry_history.clear(start)
        self.command_history.clear()
        self.__log("Clearing Test Histories" + (f" after {time_stamp}" if time_stamp is not None else ""))

    def get_command_test_history(self):
        return self.command_history

    def get_telemetry_test_history(self):
        return self.telemetry_history

    def get_event_test_history(self):
        return self.event_history

    # Commands.

    def translate_command_name(self, command):
        """Opcode of a command given by name or opcode; `KeyError` when unknown."""
        if isinstance(command, str):
            if command in self._dictionaries.command_name:
                return self._dictionaries.command_name[command]["opcode"]
            raise KeyError(f"The command mnemonic, {command}, wasn't in the dictionary")
        if command in self._dictionaries.command_id:
            return command
        raise KeyError(f"The command id, {command}, wasn't in the dictionary")

    def send_command(self, command, args=None, dry_run=False):
        """Send a command. With `dry_run` it is only validated and encoded; the answer holds the packet."""
        args = [] if args is None else list(args)
        self.__log(f"Sending Command: {command} {args}" + (" (dry run)" if dry_run else ""))
        opcode = self.translate_command_name(command)
        answer = self.client.send_command(self._dictionaries.command_id[opcode]["name"], args, dry_run=dry_run)
        self.__log(f"Command answer: {answer}")
        return answer

    def set_tlm_packet_level(self, level=3, timeout=10, commander="cmdDisp"):
        matches = [name for name in self._dictionaries.command_name if name.endswith(".SET_LEVEL")]
        if not matches:
            self.__log("SET_LEVEL command not found in dictionary; skipping set_tlm_packet_level")
        for name in matches:
            self.send_and_assert_command(name, [level], timeout=timeout, commander=commander)

    def send_and_await_telemetry(self, command, args=None, channels=None, timeout=5):
        return self.__send_then(self.telemetry_history, self.await_telemetry_sequence, self.await_telemetry, command, args, channels, timeout)

    def send_and_await_event(self, command, args=None, events=None, timeout=5):
        return self.__send_then(self.event_history, self.await_event_sequence, self.await_event, command, args, events, timeout)

    def send_and_assert_telemetry(self, command, args=None, channels=None, timeout=5):
        return self.__send_then(self.telemetry_history, self.assert_telemetry_sequence, self.assert_telemetry, command, args, channels, timeout)

    def send_and_assert_event(self, command, args=None, events=None, timeout=5):
        return self.__send_then(self.event_history, self.assert_event_sequence, self.assert_event, command, args, events, timeout)

    def __send_then(self, history, for_list, for_one, command, args, items, timeout):
        items = [] if items is None else items
        start = history.size()
        self.send_command(command, args)
        search = for_list if isinstance(items, list) else for_one
        return search(items, start=start, timeout=timeout)

    def send_and_assert_command(self, command, args=None, max_delay=None, timeout=5, events=None, commander="cmdDisp"):
        """Send a command and assert its dispatch, the given events, then its completion, in order."""
        opcode = self.translate_command_name(command)
        dispatch = [self.get_event_pred(f"{commander}.OpCodeDispatched", [opcode, None])]
        complete = [self.get_event_pred(f"{commander}.OpCodeCompleted", [opcode])]
        results = self.send_and_assert_event(command, args, dispatch + list(events or []) + complete, timeout=timeout)
        if max_delay is not None:
            delay = results[-1].get_time() - results[0].get_time()
            assert delay < max_delay, f"The delay, {delay}, between the two events should be < {max_delay}"
        return results

    # Telemetry.

    def __translate(self, what, by_name, key, name, force_component):
        if isinstance(name, str):
            if name in by_name:
                return by_name[name][key]
            matching = [d[key] for full, d in by_name.items() if full.endswith(f".{name}")]
            if force_component or not matching:
                raise KeyError(f"The {what} mnemonic, {name}, wasn't in the dictionary")
            return matching
        if any(d[key] == name for d in by_name.values()):
            return name
        raise KeyError(f"The {what} id, {name}, wasn't in the dictionary")

    def translate_telemetry_name(self, channel, force_component=True):
        return self.__translate("telemetry", self._dictionaries.channel_name, "id", channel, force_component)

    def translate_event_name(self, event, force_component=True):
        return self.__translate("event", self._dictionaries.event_name, "id", event, force_component)

    @staticmethod
    def __id_pred(identifier):
        if isinstance(identifier, list):
            return predicates.is_a_member_of(identifier)
        return predicates.equal_to(identifier)

    def get_telemetry_pred(self, channel=None, value=None, time_pred=None):
        if not predicates.is_predicate(value) and value is not None:
            value = predicates.equal_to(value)
        if isinstance(channel, predicates.telemetry_predicate):
            if value is None and time_pred is None:
                return channel
            return predicates.telemetry_predicate(
                channel.id_pred,
                value if value is not None else channel.value_pred,
                time_pred if time_pred is not None else channel.time_pred,
            )
        if not predicates.is_predicate(channel) and channel is not None:
            channel = self.__id_pred(self.translate_telemetry_name(channel, force_component=False))
        return predicates.telemetry_predicate(channel, value, time_pred)

    def await_telemetry(self, channel, value=None, time_pred=None, history=None, start="NOW", timeout=5):
        pred = self.get_telemetry_pred(channel, value, time_pred)
        return self.find_history_item(pred, self.telemetry_history if history is None else history, start, timeout)

    def await_telemetry_sequence(self, channels, history=None, start="NOW", timeout=5):
        preds = [self.get_telemetry_pred(channel) for channel in channels]
        return self.find_history_sequence(preds, self.telemetry_history if history is None else history, start, timeout)

    def await_telemetry_count(self, count, channels=None, history=None, start="NOW", timeout=5):
        search = self.__any_of(channels, self.get_telemetry_pred)
        return self.find_history_count(count, self.telemetry_history if history is None else history, search, start, timeout)

    def assert_telemetry(self, channel, value=None, time_pred=None, history=None, start=None, timeout=0):
        pred = self.get_telemetry_pred(channel, value, time_pred)
        result = self.await_telemetry(channel, value, time_pred, history, start, timeout)
        self.__assert_pred("Telemetry received", pred, result, "checks if item search found a correct update")
        return result

    def assert_telemetry_sequence(self, channels, history=None, start=None, timeout=0):
        results = self.await_telemetry_sequence(channels, history, start, timeout)
        self.__assert_pred("Telemetry sequence", predicates.equal_to(len(channels)), len(results), "checks if sequence search found every update")
        return results

    def assert_telemetry_count(self, count, channels=None, history=None, start=None, timeout=0):
        results = self.await_telemetry_count(count, channels, history, start, timeout)
        self.__assert_pred("Telemetry count", self.__count_pred(count), len(results), "checks if count search found a correct amount of updates")
        return results

    # Events.

    def get_event_pred(self, event=None, args=None, severity=None, time_pred=None):
        if isinstance(event, predicates.event_predicate):
            return event
        if not predicates.is_predicate(event) and event is not None:
            event = self.__id_pred(self.translate_event_name(event, force_component=False))
        if not predicates.is_predicate(args) and args is not None:
            args = predicates.args_predicate(args)
        if not predicates.is_predicate(severity) and severity is not None:
            if not isinstance(severity, EventSeverity):
                raise TypeError(f"Given severity was not a valid Severity Enum Value: {severity} ({type(severity)})")
            severity = predicates.equal_to(severity)
        return predicates.event_predicate(event, args, severity, time_pred)

    def await_event(self, event, args=None, severity=None, time_pred=None, history=None, start="NOW", timeout=5):
        pred = self.get_event_pred(event, args, severity, time_pred)
        return self.find_history_item(pred, self.event_history if history is None else history, start, timeout)

    def await_event_sequence(self, events, history=None, start="NOW", timeout=5):
        preds = [self.get_event_pred(event) for event in events]
        return self.find_history_sequence(preds, self.event_history if history is None else history, start, timeout)

    def await_event_count(self, count, events=None, history=None, start="NOW", timeout=5):
        search = self.__any_of(events, self.get_event_pred)
        return self.find_history_count(count, self.event_history if history is None else history, search, start, timeout)

    def assert_event(self, event, args=None, severity=None, time_pred=None, history=None, start=None, timeout=0):
        pred = self.get_event_pred(event, args, severity, time_pred)
        result = self.await_event(event, args, severity, time_pred, history, start, timeout)
        self.__assert_pred("Event received", pred, result, "checks if item search found a correct event")
        return result

    def assert_event_sequence(self, events, history=None, start=None, timeout=0):
        results = self.await_event_sequence(events, history, start, timeout)
        self.__assert_pred("Event sequence", predicates.equal_to(len(events)), len(results), "checks if sequence search found every event")
        return results

    def assert_event_count(self, count, events=None, history=None, start=None, timeout=0):
        results = self.await_event_count(count, events, history, start, timeout)
        self.__assert_pred("Event count", self.__count_pred(count), len(results), "checks if count search found a correct amount of events")
        return results

    # Deployment configuration: the JSON file of `--deployment-config`, as in the F Prime API.

    def get_deployment(self):
        return self.client.dictionary().get("metadata", {}).get("deploymentName")

    def get_config_file_path(self):
        return self.deployment_config if self.deployment_config else None

    def load_config_file(self):
        path = self.get_config_file_path()
        if path is None:
            return None
        try:
            with open(path, "r", encoding="utf-8") as file:
                return json.load(file)
        except FileNotFoundError:
            self.__log(f"Error: File not found at path {path}")
        except json.JSONDecodeError as error:
            self.__log(f"Error: {path} is not JSON: {error}")
        return None

    def get_mnemonic(self, comp=None, name=None):
        mnemonic = (self.load_config_file() or {}).get(comp)
        if mnemonic is None:
            if self.get_config_file_path() is not None:
                self.__log(f"Error: {comp} not found")
            mnemonic = comp
        return f"{mnemonic}.{name}" if name else f"{mnemonic}"

    def get_prm_db_path(self):
        path = (self.load_config_file() or {}).get("Svc.PrmDb.filename")
        if path is None:
            return None
        if not path.startswith("/"):
            self.__log(f"Error: {path} did not start with a forward slash")
            return None
        return path

    # Files: queued for uplink through the files plugin of `fgds`.

    def uplink_file(self, file_path, destination=None, packets=None):
        """Put the file on the uplink queue; `packets` (fault injection of the Python GDS) is not supported."""
        if packets is not None:
            raise NotImplementedError("fgds sends every packet of a file; a packet selection is not supported")
        entry = self.client.upload(file_path, destination)
        self.__log(f"Queued {file_path} for uplink to {destination or '/' + os.path.basename(str(file_path))}")
        return entry

    def uplink_file_and_await_completion(self, file_path, destination=None, timeout=10, packets=None):
        start = self.event_history.size()
        self.uplink_file(file_path, destination, packets)
        return self.await_event("FileReceived", start=start, timeout=timeout) is not None

    def uplink_sequence(self, sequence_path, destination=None):
        """Compile the sequence with `fgds seqgen` (`$FGDS` or `fgds` on the path) and queue the binary file."""
        directory = tempfile.mkdtemp(prefix="fgds-sequence-")
        self._sequence_tempdirs.append(directory)
        binary = os.path.join(directory, os.path.splitext(os.path.basename(str(sequence_path)))[0] + ".bin")
        command = [os.environ.get("FGDS", "fgds"), "seqgen", str(sequence_path), binary,
                   "--dictionary", str(self._dictionaries.dictionary_path)]
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        if result.returncode != 0:
            message = f"Failed to generate sequence binary from {sequence_path}: {(result.stderr or result.stdout).strip()}"
            self.__log(message)
            raise RuntimeError(message)
        return self.uplink_file(binary, destination)

    def uplink_sequence_and_await_completion(self, sequence_path, destination=None, timeout=10):
        start = self.event_history.size()
        self.uplink_sequence(sequence_path, destination)
        return self.await_event("FileReceived", start=start, timeout=timeout) is not None

    # Searches.

    @staticmethod
    def __count_pred(count):
        return count if predicates.is_predicate(count) else predicates.equal_to(count)

    @staticmethod
    def __any_of(items, to_pred):
        if items is None:
            return None
        if isinstance(items, list):
            return predicates.satisfies_any([to_pred(item) for item in items])
        return to_pred(items)

    def __search(self, name, history, start, timeout, accept, result):
        """Feed `accept` the history from `start`, then new arrivals, until it returns True or time runs out."""
        with self._changed:
            if start == self.NOW:
                current = []
            elif isinstance(start, Time):
                reached = predicates.greater_than_or_equal_to(start)
                current = history._from(lambda item: reached(item.get_time()))
            else:
                current = history._from(start)
            seen = len(history.arrivals)
        if accept(current, True):
            return result()
        if timeout:
            self.__log(f"{name} now awaiting for at most {timeout} s.")
            deadline = time.monotonic() + timeout
            while True:
                with self._changed:
                    self._alive()
                    remaining = deadline - time.monotonic()
                    if len(history.arrivals) <= seen and remaining > 0:
                        self._changed.wait(remaining)
                    new = history.arrivals[seen:]
                    seen += len(new)
                if new and accept(new, False):
                    return result()
                if not new and time.monotonic() >= deadline:
                    break
            self.__log(f"{name} timed out and ended unsuccessfully.")
        else:
            self.__log(f"{name} ended unsuccessfully.")
        return result()

    def find_history_item(self, search_pred, history, start=None, timeout=0):
        """The first item satisfying the predicate, or `None`."""
        found = []
        self.__log(f"Beginning an item search for an item that satisfies:\n    {search_pred}")

        def accept(items, _first):
            for item in items:
                if search_pred(item):
                    self.__log(f"History search found the specified item: {item}")
                    found.append(item)
                    return True
            return False

        return self.__search("Item search", history, start, timeout, accept, lambda: found[0] if found else None)

    def find_history_sequence(self, seq_preds, history, start=None, timeout=0):
        """The items satisfying the predicates in order, other items allowed in between."""
        found, waiting = [], list(seq_preds)
        self.__log(f"Beginning a sequence search of {len(waiting)} items.")

        def accept(items, _first):
            for item in items:
                if not waiting:
                    break
                if waiting[0](item):
                    self.__log(f"Sequence search found the next item: {item}")
                    found.append(item)
                    waiting.pop(0)
            return not waiting

        return self.__search("Sequence search", history, start, timeout, accept, lambda: found)

    def find_history_count(self, count, history, search_pred=None, start=None, timeout=0):
        """The items satisfying the predicate, once their number satisfies `count`."""
        count_pred = self.__count_pred(count)
        search_pred = search_pred or predicates.always_true()
        found = []
        self.__log(f"Beginning a count search for an amount of items ({count_pred}).")

        def accept(items, first):
            for item in items:
                if search_pred(item):
                    self.__log(f"Count search counted another item: {item}")
                    found.append(item)
                    if not first and count_pred(len(found)):
                        return True
            return count_pred(len(found))

        return self.__search("Count search", history, start, timeout, accept, lambda: found)
