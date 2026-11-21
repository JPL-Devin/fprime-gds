// The Channels tab: the latest sample of every channel, as the Python GDS shows it.
// The latest values come from the record API, later ones from the stream.
import { LitElement, html } from "/ui/lit.js";

const KIND = "fprime.channel";

// The limit a numeric value is beyond, if any: "red", "orange" or "yellow".
function beyond(limits, value) {
  if (!limits || typeof value !== "number") return "";
  for (const level of ["red", "orange", "yellow"]) {
    const high = limits.high && limits.high[level];
    const low = limits.low && limits.low[level];
    if ((high !== undefined && high !== null && value >= high) || (low !== undefined && low !== null && value <= low)) {
      return level;
    }
  }
  return "";
}

class ChannelsTab extends LitElement {
  static properties = {
    text: { state: true }, packet: { state: true }, all: { state: true }, version: { state: true },
  };

  constructor() {
    super();
    this.latest = new Map(); // channel identifier -> record
    this.text = "";
    this.packet = "";
    this.all = false;
    this.version = 0;
  }

  createRenderRoot() { return this; }

  connectedCallback() {
    super.connectedCallback();
    const dictionary = this.gds.dictionary;
    this.definitions = (dictionary.telemetryChannels || []).slice().sort((a, b) => a.name.localeCompare(b.name));
    this.packets = (dictionary.telemetryPacketSets || [])
      .flatMap((set) => set.members.map((p) => ({ name: `${set.name}.${p.name}`, id: p.id, members: new Set(p.members) })));
    this.start();
  }

  async start() {
    const accept = (records) => {
      for (const record of records) this.latest.set(record.id, record);
      this.version += 1;
    };
    // The stream starts after the newest record the latest values hold: a later sample
    // is delivered by it, an earlier one is in the latest values or superseded by them.
    const latest = (await this.gds.records.latest(KIND)).records;
    accept(latest);
    const newest = latest.reduce((max, r) => (BigInt(r.seq) > max ? BigInt(r.seq) : max), 0n);
    if (!this.isConnected) return;
    this.stop = this.gds.records.follow({ kinds: [KIND], afterSeq: newest.toString() }, accept);
    this.dataset.history = "loaded";
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this.stop) this.stop();
  }

  // Scalars as the dictionary formats them; arrays and structures member by member.
  shown(record) {
    const value = record.body.value;
    return value !== null && typeof value === "object" ? this.gds.format.value(value) : record.body.display_text;
  }

  render() {
    const format = this.gds.format;
    const needle = this.text.toLowerCase();
    const packet = this.packets.find((p) => p.name === this.packet);
    const rows = this.definitions.filter((d) => {
      const record = this.latest.get(d.id);
      if (!record && !this.all) return false;
      if (packet && !packet.members.has(d.name)) return false;
      if (!needle) return true;
      return [d.name, format.hexId(d.id), record ? this.shown(record) : ""]
        .some((field) => String(field).toLowerCase().includes(needle));
    });
    return html`
      <div class="toolbar">
        <label>Filter <input type="search" id="channels-filter" placeholder="name, id, value" .value=${this.text}
          @input=${(e) => { this.text = e.target.value; }}></label>
        <label>Telemetry packet
          <select id="channels-packet" @change=${(e) => { this.packet = e.target.value; }}>
            <option value="">all channels</option>
            ${this.packets.map((p) => html`<option value=${p.name}>${p.name} (${p.id})</option>`)}
          </select>
        </label>
        <label><input type="checkbox" id="channels-all" .checked=${this.all}
          @change=${(e) => { this.all = e.target.checked; }}>Channels without a value</label>
        <span class="muted" id="channels-count">${rows.length} of ${this.definitions.length} channels</span>
      </div>
      <table class="gds" id="channels-table">
        <thead><tr><th>Last Sample Time</th><th>Ground time</th><th>Channel Id</th><th>Channel Name</th>
          <th>Channel Value</th></tr></thead>
        <tbody>
          ${rows.map((d) => {
            const r = this.latest.get(d.id);
            const limit = r ? beyond(d.limits, r.body.value) : "";
            return html`<tr class=${limit ? `limit-${limit}` : ""} data-id=${d.id} data-seq=${r ? r.seq : ""}>
              <td class="nowrap mono" data-col="time" data-seconds=${r && r.flight_time ? r.flight_time.seconds : ""}
                data-microseconds=${r && r.flight_time ? r.flight_time.microseconds : ""}
                data-base=${r && r.flight_time ? r.flight_time.base : ""}>${r ? format.flightTime(r.flight_time) : ""}</td>
              <td class="nowrap mono" data-col="ground">${r ? format.groundTime(r.ground_time) : ""}</td>
              <td class="nowrap mono" data-col="id">${format.hexId(d.id)}</td>
              <td class="nowrap" data-col="name">${d.name}</td>
              <td class="text" data-col="value" >${r ? this.shown(r) : ""}</td>
            </tr>`;
          })}
        </tbody>
      </table>`;
  }
}
customElements.define("channels-tab", ChannelsTab);

export default { id: "channels", title: "Channels", element: "channels-tab" };
