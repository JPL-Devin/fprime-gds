"""Tests for the data product catalog fed by the FileDownlinker completion hook"""
import json
import shutil
import threading
from pathlib import Path
from unittest import mock

import pytest

from fprime_gds.common.dp.catalog import DataProductCatalog, DecodeQueueFull, DecodeState, UnknownProduct
from fprime_gds.common.dp.common import calculate_crc32, dp_json_filename
from fprime_gds.common.dp.decoder import DataProductDecoder
from fprime_gds.common.files.downlinker import FileDownlinker
from fprime_gds.common.handlers import DataHandler
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.common.utils.cleanup import globals_cleanup
from fprime_gds.executables.data_products import renamed_output_path

TEST_DATA_DIR = Path(__file__).parent / "test_dp_data"
REF_DICTIONARY = TEST_DATA_DIR / "dictionary_ref.json"
DICTIONARY = TEST_DATA_DIR / "dictionary.json"
CONTAINER_ID = 2576
SHORT_NAME = "DpDemoContainer"
UNCOMPRESSED = "DpDemoUncompressed.fdp"
REF_JSON = "DpDemoContainer_1781654123_00894107.json"


@pytest.fixture(autouse=True)
def clean_globals():
    globals_cleanup()
    yield
    globals_cleanup()


def load(path=REF_DICTIONARY):
    return Dictionaries.load_dictionaries_into_config(str(path))


def stage(directory, source, name, corrupt_offset=None, truncate=None):
    destination = directory / name
    shutil.copy(TEST_DATA_DIR / source, destination)
    if corrupt_offset is not None or truncate is not None:
        raw = bytearray(destination.read_bytes())
        if corrupt_offset is not None:
            raw[corrupt_offset] ^= 0xFF
        if truncate is not None:
            raw = raw[:truncate]
        destination.write_bytes(raw)
    return destination


def downlink(downlinker, name, source=UNCOMPRESSED, chunks=1):
    """Drive a complete file through the downlinker's start/data/end handlers"""
    raw = (TEST_DATA_DIR / source).read_bytes()
    downlinker.handle_start(mock.Mock(sourcePath=f"/dp/{name}".encode(), destPath=f"./{name}".encode(), size=len(raw)))
    step = max(1, len(raw) // chunks)
    for offset in range(0, len(raw), step):
        downlinker.handle_data(mock.Mock(offset=offset, dataVar=raw[offset : offset + step], seqID=downlinker.sequence))
    downlinker.handle_end(mock.Mock(seqID=downlinker.sequence))


class Recorder(DataHandler):
    def __init__(self):
        self.received = []

    def data_callback(self, data, sender=None):
        self.received.append((data, sender))


class Raiser(DataHandler):
    def data_callback(self, data, sender=None):
        raise RuntimeError("observer failure")


def make(tmp_path, dictionary=REF_DICTIONARY, **kwargs):
    catalog = DataProductCatalog(load(dictionary), tmp_path, **kwargs)
    assert catalog.enabled
    return catalog


def entry(catalog, name):
    return next(item for item in catalog.list() if item["name"] == name)


class TestHook:
    def test_hook_fires_after_close(self, tmp_path):
        downlinker = FileDownlinker(str(tmp_path))
        recorder = Recorder()
        downlinker.register(recorder)
        downlink(downlinker, "a.fdp", chunks=3)
        assert len(recorder.received) == 1
        finished, sender = recorder.received[0]
        assert sender is downlinker
        assert finished.state == "FINISHED"
        assert finished.seek == finished.size == (TEST_DATA_DIR / UNCOMPRESSED).stat().st_size
        assert (tmp_path / "._a.fdp").read_bytes() == (TEST_DATA_DIR / UNCOMPRESSED).read_bytes()

    def test_hook_cancel_timeout_error_not_listed(self, tmp_path):
        downlinker = FileDownlinker(str(tmp_path))
        catalog = make(tmp_path)
        downlinker.register(catalog)
        raw = (TEST_DATA_DIR / UNCOMPRESSED).read_bytes()
        downlinker.handle_start(mock.Mock(sourcePath=b"/dp/c.fdp", destPath=b"c.fdp", size=len(raw)))
        downlinker.handle_data(mock.Mock(offset=0, dataVar=raw[:40], seqID=0))
        downlinker.handle_cancel(None)
        downlinker.handle_start(mock.Mock(sourcePath=b"/dp/t.fdp", destPath=b"t.fdp", size=len(raw)))
        downlinker.handle_data(mock.Mock(offset=0, dataVar=raw[:40], seqID=0))
        downlinker.timeout()
        assert downlinker.active is None
        assert catalog.list() == []

    def test_observer_exception_does_not_break_downlinker(self, tmp_path):
        downlinker = FileDownlinker(str(tmp_path))
        downlinker.register(Raiser())
        recorder = Recorder()
        downlinker.register(recorder)
        downlink(downlinker, "a.fdp")
        assert downlinker.active is None
        assert downlinker.state.name == "IDLE"
        assert len(recorder.received) == 1

    def test_timer_thread_hook_vs_list(self, tmp_path):
        downlinker = FileDownlinker(str(tmp_path))
        catalog = make(tmp_path)
        downlinker.register(catalog)
        stop = threading.Event()
        errors = []

        def reader():
            while not stop.is_set():
                try:
                    catalog.list()
                except Exception as exc:  # pragma: no cover
                    errors.append(exc)

        thread = threading.Thread(target=reader)
        thread.start()
        try:
            for index in range(5):
                downlink(downlinker, f"p{index}.fdp")
                raw = (TEST_DATA_DIR / UNCOMPRESSED).read_bytes()
                downlinker.handle_start(mock.Mock(sourcePath=b"/dp/x.fdp", destPath=b"x.fdp", size=len(raw)))
                timer = threading.Timer(0.0, downlinker.timeout)
                timer.start()
                timer.join()
        finally:
            stop.set()
            thread.join()
        assert errors == []
        assert len(catalog.list()) == 5


class TestDetection:
    def test_sniff_ref_vectors(self, tmp_path):
        catalog = make(tmp_path)
        good = stage(tmp_path, UNCOMPRESSED, "good.fdp")
        compressed = stage(tmp_path, "DpDemoCompressed.fdp", "compressed.fdp")
        others = [stage(tmp_path, UNCOMPRESSED, f"trunc{size}.fdp", truncate=size) for size in (0, 10, 30, 60)]
        others.append(stage(tmp_path, UNCOMPRESSED, "flipped.fdp", corrupt_offset=2))
        others.append(stage(tmp_path, "dictionary.json", "dict.fdp"))
        for path in [good, compressed, *others]:
            catalog._consider(path, "2026-01-01T00:00:00+00:00", from_scan=False)
        listed = catalog.list()
        assert [item["name"] for item in listed] == ["good.fdp", "compressed.fdp"]
        item = listed[0]
        assert item["container_id"] == CONTAINER_ID
        assert item["container"] == "Ref.dpDemo.DpDemoContainer"
        assert item["short_name"] == SHORT_NAME
        assert item["time"]["seconds"] == 1781654123 and item["time"]["microseconds"] == 894107
        assert item["dp_state"] == "UNTRANSMITTED"
        assert item["data_size"] == 1212 and item["file_size"] == 1277
        assert item["json_name"] == REF_JSON
        assert item["state"] == "DETECTED" and item["error"] is None

    def test_sniff_crc_vectors(self, tmp_path):
        catalog = make(tmp_path, DICTIONARY)
        catalog._consider(stage(tmp_path, "CRC_HEADER_FAILURE_EXPECTED.bin", "h.bin"), "t", False)
        assert catalog.list() == []
        catalog._consider(stage(tmp_path, "CRC_FAILURE_EXPECTED.bin", "d.bin"), "t", False)
        assert entry(catalog, "d.bin")["state"] == "DETECTED"
        catalog.request_decode("d.bin")
        assert catalog.decode_next(block=False)
        item = entry(catalog, "d.bin")
        assert item["state"] == "FAILED" and item["error"].startswith("RecordNotFoundError")

    def test_unknown_container(self, tmp_path):
        catalog = make(tmp_path)
        raw = bytearray((TEST_DATA_DIR / UNCOMPRESSED).read_bytes())
        header_size = catalog._header_size
        # Id is a U32 right after the 2-byte packet descriptor; recompute the trailing header checksum
        raw[2:6] = (99).to_bytes(4, "big")
        raw[header_size - 4 : header_size] = calculate_crc32(bytes(raw[: header_size - 4])).to_bytes(4, "big")
        path = tmp_path / "unknown.fdp"
        path.write_bytes(raw)
        catalog._consider(path, "t", False)
        item = entry(catalog, "unknown.fdp")
        assert item["container_id"] == 99 and item["container"] is None and item["short_name"] is None
        assert item["json_name"] == "unknown.json"
        catalog.request_decode("unknown.fdp")
        catalog.decode_next(block=False)
        assert entry(catalog, "unknown.fdp")["state"] == "DECODED"
        assert (tmp_path / "unknown.json").is_file()

    def test_short_file_listed_with_error(self, tmp_path):
        catalog = make(tmp_path)
        catalog._consider(stage(tmp_path, UNCOMPRESSED, "short.fdp", truncate=200), "t", False)
        item = entry(catalog, "short.fdp")
        assert item["state"] == "DETECTED" and item["error"] == "file shorter than header DataSize"
        catalog.request_decode("short.fdp")
        catalog.decode_next(block=False)
        assert entry(catalog, "short.fdp")["state"] == "FAILED"


class TestDecode:
    def test_decode_success(self, tmp_path):
        catalog = make(tmp_path)
        for name, source in (("u.fdp", UNCOMPRESSED), ("c.fdp", "DpDemoCompressed.fdp")):
            catalog._consider(stage(tmp_path, source, name), "t", False)
            assert catalog.request_decode(name) == "QUEUED"
            assert catalog.decode_next(block=False)
            assert entry(catalog, name)["state"] == "DECODED"
        with open(tmp_path / REF_JSON) as file_handle:
            data = json.load(file_handle)
        assert data["Header"]["Id"]["value"] == CONTAINER_ID and len(data["Records"]) == 14
        assert entry(catalog, "c.fdp")["json_name"] != REF_JSON and (tmp_path / entry(catalog, "c.fdp")["json_name"]).is_file()
        assert not list(tmp_path.glob("*.tmp"))
        assert not catalog.decode_next(block=False)

    def test_decode_failures_and_retry(self, tmp_path):
        catalog = make(tmp_path)
        good = (TEST_DATA_DIR / UNCOMPRESSED).read_bytes()
        for offset, exception in ((-10, "DeserializeException"), (-2, "CRCError")):
            path = stage(tmp_path, UNCOMPRESSED, f"bad{-offset}.fdp", corrupt_offset=offset)
            catalog._consider(path, "t", False)
            catalog.request_decode(path.name)
            catalog.decode_next(block=False)
            item = entry(catalog, path.name)
            assert item["state"] == "FAILED" and item["error"].startswith(exception), item
            assert catalog.request_decode(path.name) == "QUEUED"
            assert entry(catalog, path.name)["error"] is None
            path.write_bytes(good)
            catalog.decode_next(block=False)
            assert entry(catalog, path.name)["state"] in ("DECODED", "FAILED")
        assert entry(catalog, "bad10.fdp")["state"] == "DECODED"
        assert (tmp_path / REF_JSON).is_file()

    def test_queue_full_semantics(self, tmp_path):
        catalog = make(tmp_path, queue_limit=1)
        for name in ("a.fdp", "b.fdp"):
            catalog._consider(stage(tmp_path, UNCOMPRESSED, name), "t", False)
        assert catalog.request_decode("a.fdp") == "QUEUED"
        with pytest.raises(DecodeQueueFull):
            catalog.request_decode("b.fdp")
        item = entry(catalog, "b.fdp")
        assert item["state"] == "DETECTED" and item["error"] == "decode queue full"
        assert catalog.request_decode("a.fdp") == "QUEUED"  # idempotent
        catalog.set_auto_decode(True)
        catalog._consider(stage(tmp_path, UNCOMPRESSED, "c.fdp"), "t", False)
        assert entry(catalog, "c.fdp")["state"] == "DETECTED"
        with pytest.raises(UnknownProduct):
            catalog.request_decode("nope.fdp")

    def test_request_decode_is_atomic(self, tmp_path):
        catalog = make(tmp_path, queue_limit=1)
        catalog._consider(stage(tmp_path, UNCOMPRESSED, "a.fdp"), "t", False)
        results = []
        barrier = threading.Barrier(2)

        def request():
            barrier.wait()
            results.append(catalog.request_decode("a.fdp"))

        threads = [threading.Thread(target=request) for _ in range(2)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        assert results == ["QUEUED", "QUEUED"]
        assert catalog._queue.qsize() == 1

    def test_stale_generation_discarded(self, tmp_path):
        catalog = make(tmp_path)
        path = stage(tmp_path, UNCOMPRESSED, "a.fdp")
        catalog._consider(path, "t", False)
        catalog.request_decode("a.fdp")
        real_decode = DataProductDecoder.decode

        def redetect_then_decode(self):
            catalog._consider(path, "t2", False)
            return real_decode(self)

        with mock.patch.object(DataProductDecoder, "decode", redetect_then_decode):
            catalog.decode_next(block=False)
        item = entry(catalog, "a.fdp")
        assert item["state"] == "DETECTED" and item["received"] == "t2"
        assert not (tmp_path / REF_JSON).exists() and not list(tmp_path.glob("*.tmp"))
        catalog.request_decode("a.fdp")
        with mock.patch.object(DataProductDecoder, "decode", side_effect=RuntimeError("boom")):
            with mock.patch.object(catalog, "_sniff", wraps=catalog._sniff):
                catalog._consider(path, "t3", False)
                catalog.decode_next(block=False)
        assert entry(catalog, "a.fdp")["state"] == "DETECTED"

    def test_eviction_releases_json_owner(self, tmp_path):
        catalog = make(tmp_path, catalog_limit=2)
        for name in ("a.fdp", "b.fdp"):
            catalog._consider(stage(tmp_path, UNCOMPRESSED, name), "t", False)
        catalog.request_decode("a.fdp")
        catalog.decode_next(block=False)
        assert entry(catalog, "a.fdp")["state"] == "DECODED"
        catalog.request_decode("b.fdp")  # QUEUED: never evicted
        catalog._consider(stage(tmp_path, UNCOMPRESSED, "c.fdp"), "t", False)
        assert [item["name"] for item in catalog.list()] == ["b.fdp", "c.fdp"]
        assert (tmp_path / REF_JSON).is_file()
        catalog.request_decode("c.fdp")
        catalog.decode_next(block=False)  # b: collision owner was evicted, so b takes the JSON
        catalog.decode_next(block=False)
        assert entry(catalog, "b.fdp")["state"] == "DECODED"
        assert entry(catalog, "c.fdp")["state"] == "FAILED"

    def test_collision_and_self_name(self, tmp_path):
        catalog = make(tmp_path)
        shutil.copy(TEST_DATA_DIR / UNCOMPRESSED, tmp_path / REF_JSON)
        catalog._consider(tmp_path / REF_JSON, "t", False)
        catalog.request_decode(REF_JSON)
        catalog.decode_next(block=False)
        item = entry(catalog, REF_JSON)
        assert item["state"] == "FAILED" and item["error"] == "output name equals input"
        with pytest.raises(UnknownProduct):
            catalog.json_path(REF_JSON)


class TestScan:
    def test_scan(self, tmp_path):
        catalog = make(tmp_path)
        first = stage(tmp_path, UNCOMPRESSED, "first.fdp")
        second = stage(tmp_path, UNCOMPRESSED, "second.fdp")
        import os

        os.utime(first, (1_000_000, 1_000_000))
        os.utime(second, (2_000_000, 2_000_000))
        (tmp_path / "notes.json").write_text("{}")
        (tmp_path / "gds.log").write_text("log")
        (tmp_path / "subdir").mkdir()
        shutil.copy(TEST_DATA_DIR / UNCOMPRESSED, tmp_path / REF_JSON)  # stale "decoded" output: first owns it
        (tmp_path / REF_JSON).write_text("{}")
        real_stat = Path.stat

        def flaky_stat(self, *args, **kwargs):
            if self.name == "gds.log":
                raise FileNotFoundError(self)
            return real_stat(self, *args, **kwargs)

        with mock.patch.object(Path, "stat", flaky_stat):
            assert catalog.scan() == 2
        listed = catalog.list()
        assert [item["name"] for item in listed] == ["first.fdp", "second.fdp"]
        assert listed[0]["state"] == "DECODED" and listed[0]["received"] == "1970-01-12T13:46:40+00:00"
        assert listed[1]["state"] == "DETECTED" and listed[1]["error"].startswith("output ")
        assert catalog.json_path("first.fdp") == tmp_path / REF_JSON

    def test_scan_and_hook_same_file(self, tmp_path):
        catalog = make(tmp_path)
        path = stage(tmp_path, UNCOMPRESSED, "a.fdp")
        (tmp_path / REF_JSON).write_text("{}")
        real_sniff = catalog._sniff

        def hook_during_scan(sniff_path):
            result = real_sniff(sniff_path)
            catalog._sniff = real_sniff
            catalog._consider(path, "hook", from_scan=False)
            return result

        catalog._sniff = hook_during_scan
        catalog.scan()
        item = entry(catalog, "a.fdp")
        assert len(catalog.list()) == 1 and catalog._entries["a.fdp"].generation == 1
        # whichever of the two detections lands last wins; ownership always matches the final state
        if item["state"] == "DECODED":
            assert catalog._json_owner == {REF_JSON: "a.fdp"}
        else:
            assert item["state"] == "DETECTED" and catalog._json_owner == {}

    def test_scan_missing_directory(self, tmp_path):
        catalog = make(tmp_path / "missing")
        assert catalog.scan() == 0


class TestFeatureOff:
    def test_feature_off(self, tmp_path):
        dictionaries = load()
        for value in (None, {}):
            with mock.patch.object(Dictionaries, "dp_container_id", new_callable=mock.PropertyMock, return_value=value):
                catalog = DataProductCatalog(dictionaries, tmp_path)
            assert not catalog.enabled
            stage(tmp_path, UNCOMPRESSED, "a.fdp")
            assert catalog.scan() == 0 and catalog.list() == []
            catalog.start()
            assert catalog._worker is None
            catalog.set_auto_decode(True)
            assert not catalog.auto_decode
            with pytest.raises(UnknownProduct):
                catalog.request_decode("a.fdp")
        assert not DataProductCatalog(None, tmp_path).enabled
        with mock.patch("fprime_gds.common.dp.catalog.get_dp_header_type", side_effect=RuntimeError("no types")):
            assert not DataProductCatalog(load(), tmp_path).enabled

    def test_naming_parity(self, tmp_path):
        dictionaries = load()
        assert dp_json_filename(SHORT_NAME, 10, 20) == f"{SHORT_NAME}_00000010_00000020.json"
        assert dp_json_filename(SHORT_NAME, 1781654123, 894107) == REF_JSON
        for name in ("Dp_00002576_00000010_00000020.fdp", "._DpCat_Dp_00002576_00000010_00000020.fdp"):
            assert renamed_output_path(dictionaries, tmp_path / name) == tmp_path / f"{SHORT_NAME}_00000010_00000020.json"
        assert renamed_output_path(dictionaries, tmp_path / "other.fdp") is None
