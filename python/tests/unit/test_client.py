"""Parts of `fgds_client` that need no running GDS."""
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from fgds_client import ApiError, ChData, Client, EventData, EventSeverity, History, Time, VersionMismatch, predicates


def record(seq, kind, identifier, seconds, body):
    time = {"base": 2, "context": 0, "seconds": seconds, "microseconds": 5}
    return {"seq": str(seq), "kind": kind, "id": identifier, "flight_time": time, "body": body}


def event(seq, seconds=10, args=None):
    body = {"name": "a.b.Done", "severity": "ACTIVITY_HI", "args": args or {"code": 3, "why": "x"}, "display_text": "Done 3 x"}
    return EventData(record(seq, "fprime.event", 0x101, seconds, body))


def serve(answers):
    """An HTTP server answering each path with (status, JSON)."""

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            status, body = answers.get(self.path, (404, {"error": {"code": "not_found", "message": self.path}}))
            self.send_response(status)
            self.end_headers()
            self.wfile.write(json.dumps(body).encode())

        def log_message(self, *_):
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_port}"


def test_a_server_of_another_version_is_refused_naming_both_versions():
    server, url = serve({"/api/v1/version": (200, {"api": 7})})
    try:
        with pytest.raises(VersionMismatch, match="API version 1,.*speaks version 7"):
            Client(url)
    finally:
        server.shutdown()


def test_generic_routes_and_error_answers():
    server, url = serve({"/api/v1/version": (200, {"api": 1}), "/api/v1/anything?x=1": (200, {"got": True})})
    try:
        client = Client(url)
        assert client.get("anything", x=1) == {"got": True}
        with pytest.raises(ApiError) as error:
            client.get("missing")
        assert (error.value.status, error.value.code) == (404, "not_found")
    finally:
        server.shutdown()


def test_event_and_channel_items():
    item = event(1)
    assert (item.get_id(), item.get_name(), item.get_args(), item.get_severity()) == (0x101, "a.b.Done", [3, "x"], EventSeverity.ACTIVITY_HI)
    assert str(item.get_time()) == "(2(0)-10:5)"
    assert item.get_str(verbose=True).endswith(": a.b.Done (257) (2(0)-10:5) EventSeverity.ACTIVITY_HI : Done 3 x")
    channel = ChData(record(2, "fprime.channel", 7, 11, {"name": "a.b.Count", "value": 4, "display_text": "4"}))
    assert (channel.get_val(), channel.get_display_text()) == (4, "4")
    assert channel.get_time() - item.get_time() == pytest.approx(1.0)
    assert Time(2, 0, 1, 0) < Time(2, 0, 1, 1) < Time(2, 0, 2, 0)


def test_predicates_run_locally():
    pred = predicates.event_predicate(predicates.equal_to(0x101), predicates.args_predicate([predicates.greater_than(2), None]))
    assert pred(event(1)) and not pred(event(1, args={"code": 1, "why": "x"}))
    assert predicates.within_range(1, 3)(2) and not predicates.within_range(1, 3)(4)
    assert predicates.satisfies_any([predicates.less_than(0), predicates.is_a_member_of([5])])(5)
    assert predicates.invert(predicates.equal_to(1))(2)
    assert "5" in predicates.get_descriptive_string(5, predicates.equal_to(5))


def test_history_orders_by_time_and_clears_on_the_client_only():
    history = History(threading.Condition())
    for seq, seconds in ((1, 30), (2, 10), (3, 20)):
        history.add(event(seq, seconds))
    assert [item.seq for item in history.retrieve()] == [2, 3, 1]
    assert [item.seq for item in history.retrieve(1)] == [3, 1]
    history.clear()
    assert history.size() == 0
