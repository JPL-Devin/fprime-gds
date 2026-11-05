####
# dataproducts.py:
#
# REST resources for the Data Products tab: list detected data products, toggle server-side auto-decode,
# request a decode of one product and serve its decoded JSON. The catalog itself lives in
# fprime_gds.common.dp.catalog and is fed by the FileDownlinker completion hook.
####
import logging
import pathlib

import flask
import flask_restful
import flask_restful.inputs
import flask_restful.reqparse

from fprime_gds.common.dp.catalog import DataProductCatalog, DecodeQueueFull, UnknownProduct

LOGGER = logging.getLogger("dataproducts")


def setup_catalog(pipeline) -> DataProductCatalog:
    """Build the catalog for the pipeline's downlinker: register the completion hook, scan the downlink
    directory once and start the decode worker. Any failure degrades to a disabled catalog so the GUI
    starts exactly as before."""
    downlinker = pipeline.files.downlinker
    directory = pathlib.Path(downlinker.directory)
    catalog = None
    try:
        catalog = DataProductCatalog(pipeline.dictionaries, directory)
        if catalog.enabled:
            downlinker.register(catalog)  # before the scan: nothing completes unobserved
            LOGGER.info("Data products: %d found at startup", catalog.scan())
            catalog.start()
        return catalog
    except Exception as exc:
        LOGGER.warning("Data products disabled: %s", exc)
        if catalog is not None:
            catalog.disable()
            downlinker.deregister(catalog)
        return DataProductCatalog(None, directory)


class DataProductList(flask_restful.Resource):
    """GET: list of products and settings. PUT: set the auto-decode flag."""

    def __init__(self, catalog: DataProductCatalog):
        self.catalog = catalog
        self.parser = flask_restful.reqparse.RequestParser()
        self.parser.add_argument(
            "auto_decode", type=flask_restful.inputs.boolean, required=True, location="json"
        )

    def get(self):
        return {
            "enabled": self.catalog.enabled,
            "auto_decode": self.catalog.auto_decode,
            "products": self.catalog.list(),
        }

    def put(self):
        if not self.catalog.enabled:
            flask_restful.abort(404, message="Data products are not available for this dictionary")
        args = self.parser.parse_args()
        self.catalog.set_auto_decode(args["auto_decode"])
        return {"auto_decode": self.catalog.auto_decode}


class DataProduct(flask_restful.Resource):
    """PUT {"action": "decode"}: queue a decode of one product. GET: serve its decoded JSON inline."""

    def __init__(self, catalog: DataProductCatalog):
        self.catalog = catalog
        self.parser = flask_restful.reqparse.RequestParser()
        self.parser.add_argument("action", type=str, required=True, location="json")

    def put(self, name):
        args = self.parser.parse_args()
        if args["action"] != "decode":
            flask_restful.abort(400, message=f"Unknown action '{args['action']}', expected 'decode'")
        try:
            return {"name": name, "state": self.catalog.request_decode(name)}
        except UnknownProduct:
            return flask_restful.abort(404, message=f"Unknown data product '{name}'")
        except DecodeQueueFull:
            return flask_restful.abort(503, message="Decode queue full, retry later")

    def get(self, name):
        try:
            json_path = self.catalog.json_path(name)
        except UnknownProduct:
            return flask_restful.abort(404, message=f"No decoded JSON for data product '{name}'")
        if not json_path.is_file():
            return flask_restful.abort(404, message=f"Decoded JSON for '{name}' is missing on disk, decode again")
        return flask.send_from_directory(
            json_path.parent, json_path.name, mimetype="application/json"
        )
