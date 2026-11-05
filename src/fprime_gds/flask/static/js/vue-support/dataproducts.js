/**
 * dataproducts.js:
 *
 * Vue component for the Data Products tab: a table of the data products detected among downlinked files (see
 * fprime_gds.common.dp.catalog), with a Decode button per row, an auto-decode toggle (server setting) and links to the
 * decoded JSON and the raw .fdp file. Data arrives through the "dataproducts" poller in datastore.js.
 */
import {_datastore} from "../datastore.js";
import {_loader} from "../loader.js";
import {_validator} from "../validate.js";
import {formatHexId, timeToString} from "./utils.js";
import {fp_row_base_component} from "./fp-row.js";

/**
 * Raw data product header time as <seconds>.<microseconds> (shown as a tooltip next to the calendar time)
 * @param time: {seconds, microseconds} object
 */
export function dpTime(time) {
    return time.seconds + "." + String(time.microseconds).padStart(6, "0");
}

/**
 * Label of the decode button for a product state, or null while a decode is queued or running. Auto-decode only
 * queues products as they arrive, so detected products always keep their button.
 * @param state: product decode state
 */
export function decodeLabel(state) {
    return {"DETECTED": "Decode", "DECODED": "Re-decode", "FAILED": "Retry"}[state] || null;
}

/**
 * Error text without the GDS host path, e.g. "FileNotFoundError: [Errno 2] No such file or directory: '/x/y.fdp'"
 * becomes "FileNotFoundError: [Errno 2] No such file or directory". The full text stays in the tooltip.
 * @param error: error string or null
 */
export function shortError(error) {
    return (error || "").replace(/:\s*'[^']*'\s*$/, "");
}

Vue.component("data-products", {
    data: function() {
        return {"products": _datastore.dataproducts, "meta": _datastore.dataproducts_meta, "pending": {}};
    },
    template: "#data-products-template",
    methods: {
        keyify(item) {
            return "dp-" + item.name + "-" + item.received;
        },
        columnify(item) {
            return [item.received, timeToString(item.time), dpTime(item.time), item.short_name || "unknown",
                    formatHexId(item.container_id), String(item.priority), String(item.data_size), item.state,
                    item.error || "", item.name];
        },
        /**
         * Request a decode of one product. The row's button is disabled until the next poll reports the new state;
         * 404/503 responses land in the GDS error counter.
         */
        decode(item) {
            this.$set(this.pending, item.name, item.state);
            _loader.load("/dataproducts/" + encodeURIComponent(item.name), "PUT", {"action": "decode"})
                .catch((error) => _validator.updateErrors([error]))
                .finally(() => setTimeout(() => this.$delete(this.pending, item.name), 1500));
        },
        setAuto(event) {
            _loader.load("/dataproducts", "PUT", {"auto_decode": event.target.checked})
                .catch((error) => _validator.updateErrors([error]));
        }
    },
    computed: {
        elements() {
            return this.products;
        }
    }
});

Vue.component("dp-row", {
    mixins: [fp_row_base_component],
    template: "#dp-row-template",
    computed: {
        decodeLabel() {
            return decodeLabel(this.item.state);
        },
        decodePending() {
            return this.$parent.$parent.pending[this.item.name] === this.item.state;
        },
        shortError() {
            return shortError(this.item.error);
        },
        calendarTime() {
            return timeToString(this.item.time);
        },
        rawTime() {
            return dpTime(this.item.time);
        },
        downloadName() {
            return this.item.name.replace(/^\.+/, "");  // browsers drop a leading dot anyway
        },
        stateClass() {
            return {"DETECTED": "badge-secondary", "QUEUED": "badge-info", "DECODING": "badge-info",
                    "DECODED": "badge-success", "FAILED": "badge-danger"}[this.item.state] || "badge-light";
        },
        encodedName() {
            return encodeURIComponent(this.item.name);
        }
    },
    methods: {
        decode() {
            this.$parent.$parent.decode(this.item);
        }
    }
});
