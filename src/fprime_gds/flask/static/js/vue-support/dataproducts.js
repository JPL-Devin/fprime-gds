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
import {formatHexId} from "./utils.js";
import {fp_row_base_component} from "./fp-row.js";

/**
 * Format a data product header time as <seconds>.<microseconds>
 * @param time: {seconds, microseconds} object
 */
export function dpTime(time) {
    return time.seconds + "." + String(time.microseconds).padStart(6, "0");
}

/**
 * Label of the decode button for a product state, or null when no button applies. Detected (not yet decoded) and failed
 * products always keep a button: auto-decode only queues products as they arrive. Auto-decode mode hides "Re-decode".
 * @param state: product decode state
 * @param auto: auto-decode flag
 */
export function decodeLabel(state, auto) {
    if (state === "FAILED") {
        return "Retry";
    }
    if (state === "DETECTED") {
        return "Decode";
    }
    return (state === "DECODED" && !auto) ? "Re-decode" : null;
}

Vue.component("data-products", {
    data: function() {
        return {"products": _datastore.dataproducts, "meta": _datastore.dataproducts_meta};
    },
    template: "#data-products-template",
    methods: {
        keyify(item) {
            return "dp-" + item.name + "-" + item.received;
        },
        columnify(item) {
            return [item.received, dpTime(item.time), item.short_name || "unknown", formatHexId(item.container_id),
                    String(item.priority), String(item.data_size), item.state, item.error || "", item.name];
        },
        /**
         * Request a decode of one product; 404/503 responses land in the GDS error counter.
         */
        decode(item) {
            _loader.load("/dataproducts/" + encodeURIComponent(item.name), "PUT", {"action": "decode"})
                .catch((error) => _validator.updateErrors([error]));
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
            return decodeLabel(this.item.state, this.$parent.$parent.meta.auto_decode);
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
