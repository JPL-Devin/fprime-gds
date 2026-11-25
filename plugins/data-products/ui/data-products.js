// The Data Products tab: the indexed products with their header fields, the decoded
// records of the selected one, and its JSON export (DP-2..5).
import { LitElement, html, css, nothing } from "./lit-core.min.js";

const KIND = "fprime.data_product";
const COLUMNS = [
  ["container", "Container"],
  ["container_id", "Id"],
  ["time", "Flight time"],
  ["priority", "Priority"],
  ["size", "Size"],
  ["source", "Source"],
  ["validation", "Validation"],
];

// The text of one column of one product: what is shown and what the filter matches.
function cell(product, column) {
  switch (column) {
    case "time":
      return product.time ? `${product.time.seconds}.${String(product.time.microseconds).padStart(6, "0")}` : "";
    case "source":
      return product.missing ? "missing" : product.sources.map((s) => `${s.source}: ${s.path}`).join(", ");
    case "validation":
      return product.valid ? "valid" : product.error || "invalid";
    default:
      return product[column] === null || product[column] === undefined ? "" : String(product[column]);
  }
}

// A decoded value as rows of [label, text]: nested members and elements are indented.
function rows(value, label, depth, out) {
  if (value !== null && typeof value === "object") {
    if (label !== null) out.push([depth, label, Array.isArray(value) ? `${value.length} elements` : ""]);
    for (const [key, item] of Object.entries(value)) rows(item, Array.isArray(value) ? `[${key}]` : key, depth + 1, out);
  } else {
    // A record that is one scalar has no label.
    out.push([Math.max(depth, 0), label ?? "", String(value)]);
  }
  return out;
}

class DataProductsTab extends LitElement {
  static properties = {
    products: { state: true },
    filters: { state: true },
    selected: { state: true },
    detail: { state: true },
    failure: { state: true },
  };

  static styles = css`
    :host { display: block; font: 13px/1.4 system-ui, sans-serif; }
    table { border-collapse: collapse; width: 100%; }
    th, td { border: 1px solid #c8ccd0; padding: 2px 6px; text-align: left; vertical-align: top; }
    th { background: #eef0f2; }
    th input { width: 100%; box-sizing: border-box; font: inherit; }
    tbody tr { cursor: pointer; }
    tr[aria-selected="true"] { background: #dbe7f5; }
    .bad { color: #a40000; }
    .panel { margin-top: 12px; }
    h2 { font-size: 14px; margin: 12px 0 4px; }
    .tree td:first-child { white-space: pre; font-family: ui-monospace, monospace; }
    .tree td { font-family: ui-monospace, monospace; }
    a.export { margin-left: 12px; }
  `;

  constructor() {
    super();
    this.products = new Map();
    this.filters = {};
    this.selected = null;
    this.detail = null;
    this.failure = null;
  }

  connectedCallback() {
    super.connectedCallback();
    // Replay the index of this run, then follow it: each record replaces its entity.
    this.stop = this.gds.records.subscribe({ kinds: [KIND], afterSeq: 0 }, (record) => {
      this.products = new Map(this.products).set(record.entity, record.body);
      if (record.entity === this.selected) this.select(record.entity);
    });
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this.stop) this.stop();
  }

  async select(hash) {
    this.selected = hash;
    this.detail = null;
    this.failure = null;
    try {
      const detail = await this.gds.api.get(`/data-products/${encodeURIComponent(hash)}`);
      if (this.selected === hash) this.detail = detail;
    } catch (error) {
      if (this.selected === hash) this.failure = error.message;
    }
  }

  visible() {
    const wanted = Object.entries(this.filters).filter(([, text]) => text);
    return [...this.products.values()].filter((product) =>
      wanted.every(([column, text]) => cell(product, column).toLowerCase().includes(text.toLowerCase())));
  }

  render() {
    const products = this.visible();
    return html`
      <table class="products">
        <thead>
          <tr>${COLUMNS.map(([, title]) => html`<th>${title}</th>`)}</tr>
          <tr>${COLUMNS.map(([column, title]) => html`<th><input
            aria-label="Filter ${title}" placeholder="filter" .value=${this.filters[column] || ""}
            @input=${(event) => { this.filters = { ...this.filters, [column]: event.target.value }; }}></th>`)}</tr>
        </thead>
        <tbody>
          ${products.map((product) => html`
            <tr aria-selected=${product.hash === this.selected} @click=${() => this.select(product.hash)}>
              ${COLUMNS.map(([column]) => html`<td class=${column === "validation" && !product.valid
                || column === "source" && product.missing ? "bad" : ""}>${cell(product, column)}</td>`)}
            </tr>`)}
        </tbody>
      </table>
      ${products.length ? nothing : html`<p>No data product${this.products.size ? " matches the filters" : " indexed in this run"}.</p>`}
      ${this.selected ? this.renderDetail() : nothing}`;
  }

  renderDetail() {
    if (this.failure) return html`<div class="panel bad">${this.failure}</div>`;
    if (!this.detail) return html`<div class="panel">Decoding...</div>`;
    const { product, decoded, error } = this.detail;
    const tree = (value) => html`<table class="tree"><tbody>${rows(value, null, -1, []).map(([depth, label, text]) =>
      html`<tr><td>${"  ".repeat(depth)}${label}</td><td>${text}</td></tr>`)}</tbody></table>`;
    return html`
      <div class="panel">
        <h2>${product.name}
          ${decoded ? html`<a class="export" download href="/api/v1/data-products/${encodeURIComponent(product.hash)}/export">Export JSON</a>` : nothing}
        </h2>
        ${product.error ? html`<p class="bad">Validation: ${product.error}</p>` : nothing}
        ${error ? html`<p class="bad">${error.code === "missing" ? "Missing" : "Not decoded"}: ${error.message}</p>` : nothing}
        ${decoded ? html`
          <h2>Header</h2>${tree(decoded.Header)}
          <h2>Records (${decoded.Records.length})</h2>
          ${decoded.Records.map((record, index) => html`
            <h2>${index}: ${record.Record.record_name} (${record.Record.record_id})</h2>${tree(record.Data)}`)}` : nothing}
      </div>`;
  }
}
customElements.define("data-products-tab", DataProductsTab);

export default { id: "data-products", title: "Data Products", element: "data-products-tab" };
