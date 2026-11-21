// The Charts tab, after the chart display of the Python GDS page: one channel per chart, chosen
// from the numeric channels of the dictionary (array elements and structure members by path),
// plotted from the moment it is chosen on an axis that scrolls with the workstation clock.
import { LitElement, html } from "/ui/lit.js";
import "./chart.umd.js"; // Chart.js, vendored; it defines `globalThis.Chart`

const KIND = "fprime.channel";
const SHOWN = 60000; // milliseconds of the scrolling axis
const REFRESH = 100; // milliseconds between two steps of the scrolling axis
const MODES = [["realtime", "Realtime"], ["anchored", "Realtime (anchored)"], ["ert", "Earth Received Time"]];

// The entries of one channel, sorted: its name for a number, `name.0` for the elements of an array
// of numbers, `name.member` for the members of a structure. Text, enumerations, booleans and arrays
// of anything but numbers have no entry.
export function entries(type, prefix) {
  const number = (t) => t.kind === "integer" || t.kind === "float";
  const elements = (element, count, path) =>
    number(element) ? Array.from({ length: count }, (_, i) => `${path}.${i}`) : [];
  const walk = (t, path) => {
    if (number(t)) return [path];
    if (t.kind === "array") return elements(t.element, t.size, path);
    if (t.kind === "struct") {
      return t.members.flatMap((m) =>
        m.count === null ? walk(m.type, `${path}.${m.name}`) : elements(m.type, m.count, `${path}.${m.name}`));
    }
    return [];
  };
  return walk(type, prefix).sort();
}

class ChartsTab extends LitElement {
  static properties = { charts: { state: true }, help: { state: true } };

  constructor() {
    super();
    this.charts = [{ id: 0, selected: "", mode: "realtime", span: 3600, paused: false }];
    this.next = 1;
    this.help = false;
    this.data = new Map(); // chart id -> [{x, y}], the samples received since the channel was chosen
    this.drawn = new Map(); // chart id -> Chart
    this.names = [];
    this.channels = new Map(); // channel name -> its entries
  }

  createRenderRoot() { return this; }

  connectedCallback() {
    super.connectedCallback();
    const channels = (this.gds.dictionary.telemetryChannels || []).slice().sort((a, b) => a.id - b.id);
    for (const channel of channels) {
      this.channels.set(channel.name, entries(this.gds.types.resolve(channel.type), channel.name));
    }
    this.names = [...this.channels.values()].flat();
    this.timer = setInterval(() => this.scroll(), REFRESH);
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    clearInterval(this.timer);
    if (this.stop) this.stop();
    this.stop = null;
    for (const chart of this.drawn.values()) chart.destroy();
    this.drawn.clear();
  }

  // Channel records are read only while a chart has a channel, and only those that arrive from then on.
  listen() {
    if (!this.stop) this.stop = this.gds.records.subscribe({ kinds: [KIND] }, (record) => this.accept(record));
  }

  accept(record) {
    const paths = this.channels.get(record.body.name);
    if (!paths || !record.flight_time) return;
    for (const chart of this.charts) {
      if (!paths.includes(chart.selected)) continue;
      const keys = chart.selected.slice(record.body.name.length + 1);
      const value = keys ? keys.split(".").reduce((v, k) => (v === null || v === undefined ? v : v[k]), record.body.value)
        : record.body.value;
      const y = typeof value === "string" ? Number(value) : value;
      if (typeof y !== "number" || Number.isNaN(y)) continue;
      const flight = record.flight_time.seconds * 1000 + record.flight_time.microseconds / 1000;
      const x = chart.mode === "ert" ? Date.parse(record.ground_time) : flight;
      this.data.get(chart.id).push({ x, y });
      this.show(chart);
    }
  }

  change(chart, values) {
    const changed = { ...chart, ...values };
    this.charts = this.charts.map((c) => (c === chart ? changed : c));
    // Another channel or another time: the chart starts again, empty.
    if ("selected" in values || "mode" in values) {
      this.discard(chart.id);
      if (changed.selected) this.listen();
    }
  }

  discard(id) {
    const drawn = this.drawn.get(id);
    if (drawn) drawn.destroy();
    this.drawn.delete(id);
    this.data.delete(id);
  }

  add(selected = "") {
    this.charts = [...this.charts, { id: this.next++, selected, mode: "realtime", span: 3600, paused: false }];
    if (selected) this.listen();
  }

  remove(chart) {
    this.discard(chart.id);
    // The canvases move up to the charts that follow: these are drawn again, with their samples.
    for (const drawn of this.drawn.values()) drawn.destroy();
    this.drawn.clear();
    this.charts = this.charts.filter((c) => c !== chart);
  }

  // The file of the Python GDS page: the channel of each chart, one per line.
  save() {
    const text = this.charts.map((c) => c.selected).filter(Boolean).join("\n");
    const link = document.createElement("a");
    link.href = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
    link.download = "current-charts.txt";
    link.click();
    URL.revokeObjectURL(link.href);
  }

  async load(event) {
    const file = event.target.files[0];
    if (!file) return;
    const names = (await file.text()).split(/\s/).map((name) => name.trim());
    event.target.value = "";
    const last = this.charts[this.charts.length - 1];
    if (names.length && last && !last.selected) this.remove(last);
    for (const name of names) this.add(this.names.includes(name) ? name : "");
  }

  updated() {
    for (const chart of this.charts) {
      if (!chart.selected || this.drawn.has(chart.id)) continue;
      const canvas = this.querySelector(`canvas[data-chart="${chart.id}"]`);
      // Set through the style object: the content security policy of the page allows no style attribute.
      Object.assign(canvas.parentElement.style, { height: "16rem", position: "relative" });
      const data = this.data.get(chart.id) || [];
      this.data.set(chart.id, data);
      const clock = (ms) => new Date(ms).toLocaleTimeString();
      this.drawn.set(chart.id, new globalThis.Chart(canvas, {
        type: "line",
        data: {
          datasets: [{
            label: chart.selected, data, backgroundColor: "rgba(54, 162, 235, 0.5)", borderColor: "rgb(54, 162, 235)",
          }],
        },
        options: {
          animation: false,
          parsing: false,
          maintainAspectRatio: false,
          interaction: { intersect: false },
          scales: {
            x: { type: "linear", ticks: { callback: clock, autoSkip: true, maxRotation: 0, maxTicksLimit: 8 } },
            y: { title: { display: true, text: "Value" } },
          },
        },
      }));
      canvas.dataset.points = "0";
      this.show(chart);
    }
  }

  // Samples older than the data window are dropped; the scrolling axis ends at the present.
  show(chart, now = Date.now()) {
    const drawn = this.drawn.get(chart.id);
    if (!drawn || chart.paused) return;
    const data = this.data.get(chart.id);
    const scrolling = chart.mode !== "anchored";
    if (scrolling) {
      const kept = data.findIndex((point) => point.x >= now - chart.span * 1000);
      data.splice(0, kept < 0 ? data.length : kept);
    }
    drawn.options.scales.x.min = scrolling ? now - SHOWN : undefined;
    drawn.options.scales.x.max = scrolling ? now : undefined;
    drawn.update("none");
    drawn.canvas.dataset.points = String(data.length);
    drawn.canvas.dataset.first = data.length ? String(data[0].y) : "";
    drawn.canvas.dataset.from = scrolling ? String(now - SHOWN) : "";
  }

  scroll() {
    const now = Date.now();
    for (const chart of this.charts) if (chart.mode !== "anchored") this.show(chart, now);
  }

  render() {
    const options = this.names.map((name) => html`<option value=${name}>${name}</option>`);
    return html`
      <div class="toolbar">
        <button id="chart-add" @click=${() => this.add()}>Add Chart</button>
        <button id="chart-save" @click=${() => this.save()}>Save</button>
        <button id="chart-open" @click=${() => this.querySelector("#chart-load").click()}>Load</button>
        <input id="chart-load" type="file" hidden @change=${(e) => this.load(e)}>
        <button id="chart-help" @click=${() => { this.help = !this.help; }}>Help</button>
      </div>
      ${this.help ? html`<div id="chart-help-text" class="note">
        <p><b>Time Modes:</b></p>
        <p><b>Realtime</b> - Chart scrolls and displays data using channel timestamps (scrolling based off of workstation time)</p>
        <p><b>Realtime (anchored)</b> - No scrolling, chart redraws and centers around all data points using channel timestamp</p>
        <p><b>Earth Received Time</b> - Chart scrolls and displays data using ground station receive time</p>
      </div>` : ""}
      ${this.charts.map((chart) => html`
        <div class="chart" data-chart-box=${chart.id}>
          <div class="toolbar">
            <select class="chart-channel" .value=${chart.selected}
              @change=${(e) => this.change(chart, { selected: e.target.value })}>
              <option value="">Select channel</option>${options}
            </select>
            <label>Data Window (S) <input class="chart-span" type="number" min="1" .value=${String(chart.span)}
              @change=${(e) => this.change(chart, { span: Number(e.target.value) || 3600 })}></label>
            <label>Time: <select class="chart-mode" .value=${chart.mode}
              @change=${(e) => this.change(chart, { mode: e.target.value })}>
              ${MODES.map(([mode, title]) => html`<option value=${mode}>${title}</option>`)}
            </select></label>
            <button class="chart-pause" ?disabled=${!chart.selected}
              @click=${() => this.change(chart, { paused: !chart.paused })}>${chart.paused ? "Resume" : "Pause"}</button>
            <button class="chart-remove" @click=${() => this.remove(chart)}>Remove</button>
          </div>
          ${chart.selected ? html`<div class="chart-area"><canvas data-chart=${chart.id}></canvas></div>` : ""}
        </div>`)}`;
  }
}

customElements.define("charts-tab", ChartsTab);

export default { id: "charts", title: "Charts", element: "charts-tab" };
