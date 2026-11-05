/**
 * dataproducts.test.mjs:
 *
 * Unit tests for the Data Products tab support code: time formatting, decode button labels and the datastore handler
 * of the /dataproducts poller. Run with: node --test dataproducts.test.mjs
 */
import { test } from "node:test";
import assert from "node:assert/strict";

// The GDS modules start browser timers on import (e.g. the responsiveness checker). Unref them so node can exit.
const nodeSetTimeout = globalThis.setTimeout;
globalThis.setTimeout = (...args) => nodeSetTimeout(...args).unref();
// Vue components register themselves on import; only the exported helpers are under test here
globalThis.Vue = { component() {} };
const js = "../../../../src/fprime_gds/flask/static/js/";
const { _datastore } = await import(js + "datastore.js");
const { _loader } = await import(js + "loader.js");
const { _settings } = await import(js + "settings.js");
const { dpTime, decodeLabel } = await import(js + "vue-support/dataproducts.js");

function product(name, state) {
    return {
        name: name, container_id: 2576, container: "Ref.dpDemo.DpDemoContainer", short_name: "DpDemoContainer",
        priority: 1, proc_types: 0, dp_state: "UNTRANSMITTED",
        time: { base: 2, context: 0, seconds: 1781654123, microseconds: 894107 },
        data_size: 1212, file_size: 1277, received: "2026-11-04T23:10:05+00:00", state: state,
        json_name: "DpDemoContainer_1781654123_00894107.json", error: null,
    };
}

test("dpTime pads microseconds to six digits", () => {
    assert.equal(dpTime({ seconds: 1781654123, microseconds: 894107 }), "1781654123.894107");
    assert.equal(dpTime({ seconds: 10, microseconds: 20 }), "10.000020");
});

test("decodeLabel per state in manual and auto mode", () => {
    assert.equal(decodeLabel("DETECTED", false), "Decode");
    assert.equal(decodeLabel("DECODED", false), "Re-decode");
    assert.equal(decodeLabel("FAILED", false), "Retry");
    assert.equal(decodeLabel("QUEUED", false), null);
    assert.equal(decodeLabel("DECODING", false), null);
    assert.equal(decodeLabel("DETECTED", true), null);
    assert.equal(decodeLabel("DECODED", true), null);
    assert.equal(decodeLabel("FAILED", true), "Retry");
});

test("updateDataProducts keeps the array object and mirrors the settings", () => {
    const array = _datastore.dataproducts;
    _datastore.updateDataProducts({ enabled: true, auto_decode: true, products: [product("a.fdp", "DETECTED")] });
    assert.equal(_datastore.dataproducts, array);
    assert.deepEqual(_datastore.dataproducts, [product("a.fdp", "DETECTED")]);
    assert.deepEqual(_datastore.dataproducts_meta, { enabled: true, auto_decode: true });
    _datastore.updateDataProducts({ enabled: true, auto_decode: false, products: [product("a.fdp", "DECODED"), product("b.fdp", "FAILED")] });
    assert.equal(_datastore.dataproducts.length, 2);
    assert.equal(_datastore.dataproducts[0].state, "DECODED");
    assert.equal(_datastore.dataproducts_meta.auto_decode, false);
});

test("updateDataProducts stops the poller when the feature is disabled", () => {
    let cleared = null;
    const nodeClearInterval = globalThis.clearInterval;
    globalThis.clearInterval = (handle) => { cleared = handle; };
    try {
        _loader.endpoints.dataproducts.interval = 42;
        _settings.polling_intervals.dataproducts = 1000;
        _datastore.updateDataProducts({ enabled: false, auto_decode: false, products: [] });
    } finally {
        globalThis.clearInterval = nodeClearInterval;
    }
    assert.equal(cleared, 42);
    assert.equal("interval" in _loader.endpoints.dataproducts, false);
    assert.equal(_settings.polling_intervals.dataproducts, -1);
    assert.deepEqual(_datastore.dataproducts, []);
    assert.equal(_datastore.dataproducts_meta.enabled, false);
});
