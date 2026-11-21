// The Events tab: every event of the run, newest first, coloured by severity as in the
// Python GDS. Data comes from the record API (history) and the stream (live).
import { LitElement, html } from "/ui/lit.js";

const KIND = "fprime.event";
const SEVERITIES = ["FATAL", "WARNING_HI", "WARNING_LO", "ACTIVITY_HI", "ACTIVITY_LO", "COMMAND", "DIAGNOSTIC"];
const OPCODE = /Opcode (0x[0-9a-fA-F]+)/;
const KEEP = 200000; // events kept in the page; older ones stay in the record API

class EventsTab extends LitElement {
  static properties = {
    text: { state: true }, severities: { state: true }, component: { state: true },
    paused: { state: true }, limit: { state: true }, version: { state: true },
  };

  constructor() {
    super();
    this.events = [];
    this.frozen = null;
    this.text = "";
    this.component = "";
    this.severities = new Set(SEVERITIES);
    this.paused = false;
    this.limit = 500;
    this.version = 0;
  }

  createRenderRoot() { return this; }

  connectedCallback() {
    super.connectedCallback();
    this.limit = this.gds.settings.get("events.rows", 500);
    this.commands = new Map((this.gds.dictionary.commands || []).map((c) => [c.opcode, c.name]));
    this.stop = this.gds.records.follow({ kinds: [KIND] }, (records) => {
      this.events.push(...records);
      if (this.events.length > KEEP) this.events.splice(0, this.events.length - KEEP);
      if (!this.paused) this.version += 1;
    });
    this.stop.ready.then(() => { this.dataset.history = "loaded"; });
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this.stop) this.stop();
  }

  matches(record) {
    const body = record.body;
    if (!this.severities.has(body.severity)) return false;
    if (this.component && this.gds.format.component(body.name) !== this.component) return false;
    if (!this.text) return true;
    const needle = this.text.toLowerCase();
    return [body.name, body.display_text, body.severity, this.gds.format.hexId(record.id)]
      .some((field) => String(field).toLowerCase().includes(needle));
  }

  // The message, with the opcode of a command event followed by its mnemonic as the
  // Python GDS shows it. Everything is text: nothing from the link is parsed as HTML.
  message(record) {
    const text = record.body.display_text;
    const found = record.body.severity === "COMMAND" ? text.match(OPCODE) : null;
    if (!found) return text;
    const name = this.commands.get(parseInt(found[1], 16)) || "UNKNOWN";
    return html`${text.slice(0, found.index)}<span title=${found[0]}>${name}</span>${text.slice(found.index + found[0].length)}`;
  }

  togglePause() {
    this.paused = !this.paused;
    this.frozen = this.paused ? this.events.slice() : null;
    this.version += 1;
  }

  toggleSeverity(severity, on) {
    const next = new Set(this.severities);
    if (on) next.add(severity); else next.delete(severity);
    this.severities = next;
  }

  render() {
    const format = this.gds.format;
    const source = this.frozen || this.events;
    const components = [...new Set((this.gds.dictionary.events || []).map((e) => format.component(e.name)))].sort();
    const matching = source.filter((r) => this.matches(r));
    const shown = (this.limit ? matching.slice(-this.limit) : matching).reverse();
    return html`
      <div class="toolbar">
        <label>Filter <input type="search" id="events-filter" placeholder="name, text, id" .value=${this.text}
          @input=${(e) => { this.text = e.target.value; }}></label>
        <label>Component
          <select id="events-component" @change=${(e) => { this.component = e.target.value; }}>
            <option value="">all</option>
            ${components.map((c) => html`<option value=${c}>${c}</option>`)}
          </select>
        </label>
        ${SEVERITIES.map((s) => html`<label><input type="checkbox" data-severity=${s} .checked=${this.severities.has(s)}
          @change=${(e) => this.toggleSeverity(s, e.target.checked)}>${s}</label>`)}
        <label>Rows
          <select id="events-rows" @change=${(e) => {
            this.limit = Number(e.target.value);
            this.gds.settings.set("events.rows", this.limit);
          }}>
            ${[100, 500, 5000, 0].map((n) => html`<option value=${n} ?selected=${n === this.limit}>${n || "all"}</option>`)}
          </select>
        </label>
        <button class="action secondary" id="events-pause" @click=${() => this.togglePause()}>
          ${this.paused ? `Resume (${this.events.length - this.frozen.length} new)` : "Pause"}</button>
        <span class="muted" id="events-count">${shown.length} of ${matching.length} shown, ${source.length} received</span>
      </div>
      <table class="gds" id="events-table">
        <thead><tr><th>Event time</th><th>Ground time</th><th>Event Id</th><th>Event Name</th><th>Event Severity</th>
          <th>Event Description</th></tr></thead>
        <tbody>
          ${shown.map((r) => html`<tr class="sev-${r.body.severity}" data-seq=${r.seq}>
            <td class="nowrap mono" data-col="time" data-seconds=${r.flight_time ? r.flight_time.seconds : ""}
              data-microseconds=${r.flight_time ? r.flight_time.microseconds : ""}
              data-base=${r.flight_time ? r.flight_time.base : ""}>${format.flightTime(r.flight_time)}</td>
            <td class="nowrap mono" data-col="ground">${format.groundTime(r.ground_time)}</td>
            <td class="nowrap mono" data-col="id">${format.hexId(r.id)}</td>
            <td class="nowrap" data-col="name">${r.body.name}</td>
            <td class="nowrap" data-col="severity">${r.body.severity}</td>
            <td class="text" data-col="text" title=${format.value(r.body.args)}>${this.message(r)}</td>
          </tr>`)}
        </tbody>
      </table>`;
  }
}
customElements.define("events-tab", EventsTab);

export default { id: "events", title: "Events", element: "events-tab" };
