// The Advanced tab: the state of the GDS (link, storage, counters, plugins), the
// versions, the record kinds, the tabs, and the link switch.
import { LitElement, html } from "/ui/lit.js";

class AdvancedTab extends LitElement {
  static properties = { status: { state: true }, kinds: { state: true }, runs: { state: true }, message: { state: true } };

  constructor() {
    super();
    this.status = null;
    this.kinds = [];
    this.runs = [];
    this.message = "";
  }

  createRenderRoot() { return this; }

  connectedCallback() {
    super.connectedCallback();
    this.stop = this.gds.status.subscribe((status) => { this.status = status; });
    this.gds.api.get("/kinds").then((kinds) => { this.kinds = kinds; }).catch((e) => { this.message = e.message; });
    this.gds.api.get("/runs").then((runs) => { this.runs = runs; }).catch((e) => { this.message = e.message; });
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this.stop) this.stop();
  }

  async link(enabled) {
    try {
      await this.gds.api.post("/link", { enabled });
      this.message = enabled ? "Link enabled" : "Link disabled";
    } catch (error) {
      this.message = error.message;
    }
    this.gds.status.refresh();
  }

  render() {
    const version = this.gds.version || {};
    const s = this.status;
    const format = this.gds.format;
    if (!s) return html`<div class="muted">Waiting for the status…</div>`;
    return html`
      <h2>Advanced GDS Settings and Statistics</h2>
      <h3>Versions</h3>
      <dl class="facts" id="advanced-versions">
        <dt>GDS</dt><dd data-field="gds">${version.gds}</dd>
        <dt>API</dt><dd data-field="api">${version.api}</dd>
        <dt>Dictionary</dt><dd data-field="dictionary">${version.dictionary}</dd>
        <dt>Run</dt><dd>${s.run}</dd>
        <dt>Last record</dt><dd>${s.cursor}</dd>
      </dl>
      <h3>Link and storage</h3>
      <dl class="facts" id="advanced-link">
        <dt>Link</dt><dd data-field="link">${s.link.state}${s.link.peer ? ` (${s.link.peer})` : ""}</dd>
        <dt>Storage</dt><dd>${s.storage.state}</dd>
      </dl>
      <div class="toolbar">
        <button class="action secondary" id="link-enable" @click=${() => this.link(true)}>Enable link</button>
        <button class="action secondary" id="link-disable" @click=${() => this.link(false)}>Disable link</button>
        <span id="advanced-message">${this.message}</span>
      </div>
      <h3>Miscellaneous</h3>
      <label title="When set, any channel received will update the displayed value. Otherwise only channels with newer timestamps update the displayed value.">
        <input type="checkbox" id="channels-display-last-received"
          .checked=${this.gds.session.channels_display_last_received}
          @change=${(e) => { this.gds.session.channels_display_last_received = e.target.checked; }}>
        channels_display_last_received</label>
      <h3>Statistics</h3>
      <table class="gds" id="advanced-counters">
        <thead><tr><th>Counter</th><th>Value</th></tr></thead>
        <tbody>${Object.entries(s.counters).map(([name, value]) => html`<tr><td>${name}</td><td class="mono">${value}</td></tr>`)}</tbody>
      </table>
      <h3>Plugins</h3>
      <table class="gds" id="advanced-plugins">
        <thead><tr><th>Plugin</th><th>Version</th><th>State</th><th>Fault</th><th>Counters</th></tr></thead>
        <tbody>${(version.plugins || []).map((p) => {
          const state = s.plugins[p.name] || {};
          return html`<tr class=${state.fault ? "limit-red" : ""}><td>${p.name}</td><td class="mono">${p.version}</td>
            <td>${state.state || ""}</td><td class="text">${state.fault || ""}</td>
            <td class="text">${format.value(state.counters || {})}</td></tr>`;
        })}</tbody>
      </table>
      <h3>Tabs</h3>
      <table class="gds" id="advanced-tabs">
        <thead><tr><th>Tab</th><th>Title</th><th>Module</th></tr></thead>
        <tbody>${this.gds.tabs.map((t) => html`<tr><td>${t.id}</td><td>${t.title}</td><td class="mono">${t.module}</td></tr>`)}</tbody>
      </table>
      <h3>Record kinds</h3>
      <table class="gds" id="advanced-kinds">
        <thead><tr><th>Kind</th><th>Description</th><th>Stored</th><th>Filter fields</th></tr></thead>
        <tbody>${this.kinds.map((k) => html`<tr><td class="mono">${k.id}</td><td class="text">${k.description}</td>
          <td>${k.transient ? "no" : "yes"}</td><td>${k.fields.map((f) => f.name).join(", ")}</td></tr>`)}</tbody>
      </table>
      <h3>Runs</h3>
      <pre class="log mono" id="advanced-runs">${this.runs.map((r) => JSON.stringify(r)).join("\n")}</pre>`;
  }
}
customElements.define("advanced-tab", AdvancedTab);

export default { id: "advanced", title: "Advanced", element: "advanced-tab" };
