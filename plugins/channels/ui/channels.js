// The Channels tab: the latest sample of every channel, as the Python GDS shows it.
// The latest values come from the record API, later ones from the stream.
import { LitElement, html } from "/ui/lit.js";

const KIND = "fprime.channel";

// The row colour of the Python GDS page: none for a channel without limits; otherwise the
// first limit the value is beyond ("red", "orange", "yellow"), or "ok" inside all of them.
function beyond(limits, value) {
  const set = (side, level) => {
    const limit = limits && limits[side] && limits[side][level];
    return limit === undefined ? null : limit;
  };
  const levels = ["red", "orange", "yellow"];
  if (levels.every((level) => set("low", level) === null && set("high", level) === null)) return "";
  for (const level of levels) {
    const low = set("low", level);
    const high = set("high", level);
    if ((low !== null && value < low) || (high !== null && value > high)) return level;
  }
  return "ok";
}

class ChannelsTab extends LitElement {
  static properties = {
    text: { state: true }, packet: { state: true }, all: { state: true }, version: { state: true },
    editing: { state: true }, view: { state: true },
  };

  constructor() {
    super();
    this.latest = new Map(); // channel identifier -> record
    this.text = "";
    this.packet = "";
    this.all = false;
    this.version = 0;
    this.editing = false;
    this.view = []; // channel names of "Edit View"; empty: every channel
  }

  createRenderRoot() { return this; }

  connectedCallback() {
    super.connectedCallback();
    const dictionary = this.gds.dictionary;
    this.definitions = (dictionary.telemetryChannels || []).slice().sort((a, b) => a.id - b.id);
    this.view = this.gds.settings.get("channels.view", []);
    this.columns = this.gds.columns("channels", [["time", "Last Sample Time"], ["ground", "Ground time"], ["id", "Channel Id"],
      ["name", "Channel Name"], ["value", "Channel Value"]], () => { this.version += 1; });
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

  setView(names) {
    this.view = names;
    this.gds.settings.set("channels.view", names);
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this.stop) this.stop();
  }

  // What the Python GDS page shows. With a format string: the formatted text. Without:
  // an array or a structure member by member, each leaf as formatted text; a scalar is
  // the value itself, which the page writes (a float as `String(number)`).
  shown(record, definition) {
    const body = record.body;
    if (definition.format) return body.display_text;
    return body.display_members !== undefined ? this.gds.format.composite(body.display_members) : this.gds.format.value(body.value);
  }

  render() {
    const format = this.gds.format;
    const needle = this.text.toLowerCase();
    const packet = this.packets.find((p) => p.name === this.packet);
    const columns = this.columns;
    const rows = columns.sort(this.definitions.filter((d) => {
      const record = this.latest.get(d.id);
      // "Edit View" of the Python GDS page: while editing every channel is listed; with a view, its channels
      // are listed whether they have a value or not, and no others.
      if (this.editing) { /* every channel */ }
      else if (this.view.length) { if (!this.view.includes(d.name)) return false; }
      else if (!record && !this.all) return false;
      if (packet && !packet.members.has(d.name)) return false;
      if (!needle) return true;
      return [d.name, format.hexId(d.id), record ? this.shown(record, d) : ""]
        .some((field) => String(field).toLowerCase().includes(needle));
    }), (d, key) => {
      const r = this.latest.get(d.id);
      return ({
        time: r ? format.flightTime(r.flight_time) : "", ground: r ? format.groundTime(r.ground_time) : "",
        id: format.hexId(d.id), name: d.name,
        // The Python GDS page sorts by `String()` of the value: an array is its members joined by commas, a
        // structure is `[object Object]`.
        value: r ? (d.format ? r.body.display_text : r.body.display_members !== undefined ? r.body.display_members : r.body.value) : "",
      })[key];
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
        ${columns.chooser()}
        ${this.editing ? html`
          <button class="action secondary" id="view-clear" @click=${() => this.setView([])}>Clear</button>
          <a class="action secondary" id="view-save" download="current-view.txt"
            href=${"data:text/plain;charset=utf-8," + encodeURIComponent(this.view.join("\n"))}>Save</a>
          <label class="action secondary">Load<input type="file" id="view-load" hidden @input=${async (e) => {
            this.setView((await e.target.files[0].text()).split(/\s/).map((name) => name.trim()));
            e.target.value = "";
          }}></label>
          <button class="action" id="view-done" @click=${() => { this.editing = false; this.text = ""; }}>Done</button>`
        : html`<button class="action" id="view-edit" @click=${() => { this.editing = true; this.text = ""; }}>Edit View</button>`}
        <button class="action secondary" id="channels-clear" @click=${() => { this.latest.clear(); this.version += 1; }}>Clear All</button>
        <span class="muted" id="channels-count">${rows.length} of ${this.definitions.length} channels</span>
      </div>
      <table class="gds" id="channels-table">
        <thead>${columns.head(this.editing ? html`<th><input type="checkbox" id="view-all"
          .checked=${this.view.length === this.definitions.length}
          @input=${(e) => this.setView(e.target.checked
            ? [...new Set([...this.view, ...rows.map((d) => d.name)])]
            : this.view.filter((name) => !rows.some((d) => d.name === name)))}></th>` : "")}</thead>
        <tbody>
          ${rows.map((d) => {
            const r = this.latest.get(d.id);
            const limit = r ? beyond(d.limits, r.body.value) : "";
            return html`<tr class=${limit ? `limit-${limit}` : ""} data-id=${d.id} data-seq=${r ? r.seq : ""}>
              ${this.editing ? html`<td><input type="checkbox" class="in-view" .checked=${this.view.includes(d.name)}
                @input=${(e) => this.setView(e.target.checked ? [...this.view, d.name] : this.view.filter((name) => name !== d.name))}></td>` : ""}
              <td class="nowrap mono" data-col="time" ?hidden=${!columns.shows("time")} data-seconds=${r && r.flight_time ? r.flight_time.seconds : ""}
                data-microseconds=${r && r.flight_time ? r.flight_time.microseconds : ""}
                data-base=${r && r.flight_time ? r.flight_time.base : ""}>${r ? format.flightTime(r.flight_time) : ""}</td>
              <td class="nowrap mono" data-col="ground" ?hidden=${!columns.shows("ground")}>${r ? format.groundTime(r.ground_time) : ""}</td>
              <td class="nowrap mono" data-col="id" ?hidden=${!columns.shows("id")}>${format.hexId(d.id)}</td>
              <td class="nowrap" data-col="name" ?hidden=${!columns.shows("name")}>${d.name}</td>
              <td class="text" data-col="value" ?hidden=${!columns.shows("value")}>${r ? this.shown(r, d) : ""}</td>
            </tr>`;
          })}
        </tbody>
      </table>`;
  }
}
customElements.define("channels-tab", ChannelsTab);

export default { id: "channels", title: "Channels", element: "channels-tab" };
