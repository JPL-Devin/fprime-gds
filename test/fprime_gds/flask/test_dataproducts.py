"""Tests for the /dataproducts REST resources"""
import shutil
from pathlib import Path

import flask
import flask_restful
import pytest

from fprime_gds.common.dp.catalog import DataProductCatalog
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.common.utils.cleanup import globals_cleanup
from fprime_gds.flask.dataproducts import DataProduct, DataProductList

TEST_DATA_DIR = Path(__file__).parent.parent / "common" / "dp" / "test_dp_data"
REF_DICTIONARY = TEST_DATA_DIR / "dictionary_ref.json"
NAME = "._DpCat_Dp_00002576_1781654123_00894107.fdp"
REF_JSON = "DpDemoContainer_1781654123_00894107.json"


@pytest.fixture(autouse=True)
def clean_globals():
    globals_cleanup()
    yield
    globals_cleanup()


def make_client(catalog):
    app = flask.Flask(__name__)
    api = flask_restful.Api(app)
    api.add_resource(DataProductList, "/dataproducts", resource_class_args=[catalog])
    api.add_resource(DataProduct, "/dataproducts/<string:name>", resource_class_args=[catalog])
    return app.test_client()


@pytest.fixture
def catalog(tmp_path):
    dictionaries = Dictionaries.load_dictionaries_into_config(str(REF_DICTIONARY))
    catalog = DataProductCatalog(dictionaries, tmp_path, queue_limit=1)
    shutil.copy(TEST_DATA_DIR / "DpDemoUncompressed.fdp", tmp_path / NAME)
    assert catalog.scan() == 1
    return catalog


@pytest.fixture
def client(catalog):
    return make_client(catalog)


def test_list(client):
    response = client.get("/dataproducts?session=abc&limit=5")
    assert response.status_code == 200
    body = response.get_json()
    assert body["enabled"] is True and body["auto_decode"] is False
    assert [item["name"] for item in body["products"]] == [NAME]
    assert body["products"][0]["state"] == "DETECTED"
    assert body["products"][0]["json_name"] == REF_JSON


def test_list_disabled(tmp_path):
    client = make_client(DataProductCatalog(None, tmp_path))
    assert client.get("/dataproducts").get_json() == {"enabled": False, "auto_decode": False, "products": []}
    assert client.put("/dataproducts", json={"auto_decode": True}).status_code == 404
    assert client.put(f"/dataproducts/{NAME}", json={"action": "decode"}).status_code == 404
    assert client.get(f"/dataproducts/{NAME}").status_code == 404


def test_settings(client, catalog):
    assert client.put("/dataproducts", json={"auto_decode": True}).get_json() == {"auto_decode": True}
    assert catalog.auto_decode is True
    assert client.put("/dataproducts", json={"auto_decode": False}).get_json() == {"auto_decode": False}
    assert client.put("/dataproducts", json={}).status_code == 400
    assert client.put("/dataproducts", json={"auto_decode": "yes"}).status_code == 400
    assert client.put("/dataproducts", json={"auto_decode": 1}).status_code == 400
    assert client.put("/dataproducts", json={"auto_decode": None}).status_code == 400
    assert client.put("/dataproducts", data="auto_decode=true", content_type="text/plain").status_code == 415
    assert catalog.auto_decode is False


def test_decode(client, catalog, tmp_path):
    response = client.put(f"/dataproducts/{NAME}", json={"action": "decode"})
    assert response.status_code == 200 and response.get_json() == {"name": NAME, "state": "QUEUED"}
    assert client.put(f"/dataproducts/{NAME}", json={"action": "decode"}).get_json()["state"] == "QUEUED"
    assert client.put(f"/dataproducts/{NAME}", json={"action": "delete"}).status_code == 400
    assert client.put(f"/dataproducts/{NAME}", json={}).status_code == 400
    assert client.put("/dataproducts/nope.fdp", json={"action": "decode"}).status_code == 404
    assert client.put("/dataproducts/..", json={"action": "decode"}).status_code == 404
    assert client.get(f"/dataproducts/{NAME}").status_code == 404  # not decoded yet
    assert catalog.decode_next(block=False)
    response = client.get(f"/dataproducts/{NAME}")
    assert response.status_code == 200 and response.mimetype == "application/json"
    assert "attachment" not in response.headers.get("Content-Disposition", "")
    assert response.get_json()["Header"]["Id"]["value"] == 2576
    (tmp_path / REF_JSON).unlink()
    assert client.get(f"/dataproducts/{NAME}").status_code == 404
    assert client.get("/dataproducts").get_json()["products"][0]["state"] == "DECODED"


def test_decode_queue_full(client, catalog, tmp_path):
    shutil.copy(TEST_DATA_DIR / "DpDemoCompressed.fdp", tmp_path / "second.fdp")
    catalog._consider(tmp_path / "second.fdp", "t", from_scan=False)
    assert client.put(f"/dataproducts/{NAME}", json={"action": "decode"}).status_code == 200
    response = client.put("/dataproducts/second.fdp", json={"action": "decode"})
    assert response.status_code == 503
    second = [item for item in client.get("/dataproducts").get_json()["products"] if item["name"] == "second.fdp"][0]
    assert second["state"] == "DETECTED" and second["error"] == "decode queue full"
