// The Events tab: every event of the run, oldest first, coloured by severity as in the
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
    this.columns = this.gds.columns("events", [["time", "Event Time"], ["ground", "Ground time"], ["id", "Event Id"],
      ["name", "Event Name"], ["severity", "Event Severity"], ["text", "Event Description"]], () => { this.version += 1; });
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

  accepts(record) {
    const body = record.body;
    if (!this.severities.has(body.severity)) return false;
    if (this.component && this.gds.format.component(body.name) !== this.component) return false;
    const format = this.gds.format;
    return format.matches([format.flightTime(record.flight_time), format.hexId(record.id), body.name, body.severity,
      this.flat(record)], this.text);
  }

  // The text the Python GDS page hands to the description cell, where a command name is a span element: its
  // filter and its sorting read this text.
  flat(r) {
    const [before, name, after, opcode] = this.parts(r);
    return name === undefined ? before : `${before}<span title="${opcode}">${name}</span>${after}`;
  }

  // The message, with the opcode of a command event followed by its mnemonic as the
  // Python GDS shows it. Everything is text: nothing from the link is parsed as HTML.
  parts(record) {
    const text = this.gds.format.flightText(record.body.display_text);
    const found = record.body.severity === "COMMAND" ? text.match(OPCODE) : null;
    if (!found) return [text];
    const name = this.commands.get(parseInt(found[1], 16)) || "UNKNOWN";
    return [text.slice(0, found.index), name, text.slice(found.index + found[0].length), found[0]];
  }

  message(record) {
    const [before, name, after, opcode] = this.parts(record);
    return name === undefined ? before : html`${before}<span title=${opcode}>${name}</span>${after}`;
  }

  togglePause() {
    this.paused = !this.paused;
    this.frozen = this.paused ? this.events.slice() : null;
    this.version += 1;
  }

  // As "Clear All" of the Python GDS page: the events of the page are dropped; the GDS keeps its records.
  clear() {
    this.events.splice(0, this.events.length);
    if (this.frozen) this.frozen = [];
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
    const matching = source.filter((r) => this.accepts(r));
    const columns = this.columns;
    const flat = (r) => this.flat(r);
    const sorted = columns.sort(matching, (r, key) => ({
      time: format.flightTime(r.flight_time), ground: format.groundTime(r.ground_time), id: format.hexId(r.id),
      name: r.body.name, severity: r.body.severity, text: flat(r),
    })[key]);
    const shown = this.limit ? (columns.sorted() ? sorted.slice(0, this.limit) : sorted.slice(-this.limit)) : sorted;
    return html`
      <h2>Events</h2>
      <div class="toolbar">
        <label>Filters: <input type="search" id="events-filter" placeholder="name, text, id" .value=${this.text}
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
        ${columns.chooser()}
        <button class="action secondary" id="events-pause" @click=${() => this.togglePause()}>
          ${this.paused ? `Resume (${this.events.length - this.frozen.length} new)` : "Pause"}</button>
        <button class="action secondary" id="events-clear" @click=${() => this.clear()}>Clear All</button>
        <span class="muted" id="events-count">${shown.length} of ${matching.length} shown, ${source.length} received</span>
      </div>
      <table class="gds" id="events-table">
        <thead>${columns.head()}</thead>
        <tbody>
          ${shown.map((r) => html`<tr class="sev-${r.body.severity}" data-seq=${r.seq}>
            <td class="nowrap mono" data-col="time" ?hidden=${!columns.shows("time")} data-seconds=${r.flight_time ? r.flight_time.seconds : ""}
              data-microseconds=${r.flight_time ? r.flight_time.microseconds : ""}
              data-base=${r.flight_time ? r.flight_time.base : ""}>${format.flightTime(r.flight_time)}</td>
            <td class="nowrap mono" data-col="ground" ?hidden=${!columns.shows("ground")}>${format.groundTime(r.ground_time)}</td>
            <td class="nowrap mono" data-col="id" ?hidden=${!columns.shows("id")}>${format.hexId(r.id)}</td>
            <td class="nowrap" data-col="name" ?hidden=${!columns.shows("name")}>${r.body.name}</td>
            <td class="nowrap" data-col="severity" ?hidden=${!columns.shows("severity")}>${r.body.severity}</td>
            <td class="text" data-col="text" ?hidden=${!columns.shows("text")} title=${format.value(r.body.args)}>${this.message(r)}</td>
          </tr>`)}
        </tbody>
      </table>`;
  }
}
customElements.define("events-tab", EventsTab);

export default { id: "events", title: "Events", element: "events-tab" };
