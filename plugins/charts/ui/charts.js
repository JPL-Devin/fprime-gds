// The Charts tab: numeric channels plotted over flight time. Each chart holds one or more
// channels; the samples come from the record API, later ones from the stream.
import { LitElement, html } from "/ui/lit.js";
import "./chart.umd.js"; // Chart.js, vendored; it defines `globalThis.Chart`

const KIND = "fprime.channel";
const KEPT = 5000; // samples kept per channel
const COLOURS = ["#1f77b4", "#d62728", "#2ca02c", "#ff7f0e", "#9467bd", "#8c564b", "#17becf", "#7f7f7f"];

class ChartsTab extends LitElement {
  static properties = { charts: { state: true }, names: { state: true }, paused: { state: true } };

  constructor() {
    super();
    this.samples = new Map(); // channel name -> [{x, y}], in arrival order
    this.names = [];
    this.charts = []; // {id, channels: [name]}
    this.drawn = new Map(); // chart id -> Chart
    this.paused = false;
    this.next = 1;
  }

  createRenderRoot() { return this; }

  connectedCallback() {
    super.connectedCallback();
    this.charts = this.gds.settings.get("charts.shown", []);
    this.next = this.charts.reduce((max, c) => Math.max(max, c.id + 1), 1);
    this.stop = this.gds.records.follow({ kinds: [KIND] }, (records) => this.accept(records));
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this.stop) this.stop();
    for (const chart of this.drawn.values()) chart.destroy();
    this.drawn.clear();
  }

  accept(records) {
    let grown = false;
    for (const record of records) {
      const value = record.body.value;
      const number = typeof value === "number" ? value : typeof value === "string" ? Number(value) : NaN;
      // A channel that is not a number (text, enumeration, array, structure) is not plotted.
      if (!record.flight_time || value === "" || Number.isNaN(number)) continue;
      let list = this.samples.get(record.body.name);
      if (!list) {
        this.samples.set(record.body.name, (list = []));
        grown = true;
      }
      list.push({ x: record.flight_time.seconds + record.flight_time.microseconds / 1e6, y: number });
      if (list.length > KEPT) list.splice(0, list.length - KEPT);
    }
    if (grown) this.names = [...this.samples.keys()].sort();
    if (!this.paused) this.draw();
  }

  save() {
    this.gds.settings.set("charts.shown", this.charts);
  }

  add() {
    const name = this.querySelector("#chart-channel").value;
    if (!name) return;
    this.charts = [...this.charts, { id: this.next++, channels: [name] }];
    this.save();
  }

  addTo(chart, name) {
    if (!name || chart.channels.includes(name)) return;
    this.charts = this.charts.map((c) => (c === chart ? { ...c, channels: [...c.channels, name] } : c));
    this.save();
  }

  remove(chart) {
    const drawn = this.drawn.get(chart.id);
    if (drawn) drawn.destroy();
    this.drawn.delete(chart.id);
    this.charts = this.charts.filter((c) => c !== chart);
    this.save();
  }

  updated() { this.draw(); }

  draw() {
    for (const chart of this.charts) {
      const canvas = this.querySelector(`canvas[data-chart="${chart.id}"]`);
      if (!canvas) continue;
      // Set through the style object: the content security policy of the page allows no style attribute.
      Object.assign(canvas.parentElement.style, { height: "16rem", position: "relative" });
      const datasets = chart.channels.map((name, i) => ({
        label: name,
        data: (this.samples.get(name) || []).slice(),
        borderColor: COLOURS[i % COLOURS.length],
        backgroundColor: COLOURS[i % COLOURS.length],
        borderWidth: 1.5,
        pointRadius: 0,
      }));
      let drawn = this.drawn.get(chart.id);
      if (drawn && drawn.canvas !== canvas) {
        drawn.destroy();
        drawn = null;
      }
      if (!drawn) {
        drawn = new globalThis.Chart(canvas, {
          type: "line",
          data: { datasets },
          options: {
            animation: false,
            parsing: false,
            maintainAspectRatio: false,
            scales: {
              x: {
                type: "linear",
                title: { display: true, text: "flight time" },
                ticks: { callback: (seconds) => new Date(seconds * 1000).toISOString().slice(11, 19), maxTicksLimit: 8 },
              },
            },
          },
        });
        this.drawn.set(chart.id, drawn);
      } else {
        drawn.data.datasets = datasets;
        drawn.update("none");
      }
      canvas.dataset.points = String(datasets.reduce((sum, d) => sum + d.data.length, 0));
    }
  }

  render() {
    const options = this.names.map((name) => html`<option value=${name}>${name}</option>`);
    return html`
      <div class="toolbar">
        <label>Channel <select id="chart-channel">${options}</select></label>
        <button id="chart-add" ?disabled=${!this.names.length} @click=${() => this.add()}>Add chart</button>
        <button id="chart-pause" @click=${() => { this.paused = !this.paused; }}>${this.paused ? "Resume" : "Pause"}</button>
        <span>${this.names.length} numeric channels have samples</span>
      </div>
      ${this.charts.map((chart) => html`
        <div class="chart" data-chart-box=${chart.id}>
          <div class="toolbar">
            <b>${chart.channels.join(", ")}</b>
            <select class="chart-more" @change=${(e) => { this.addTo(chart, e.target.value); e.target.value = ""; }}>
              <option value="">Add a channel…</option>${options}
            </select>
            <button class="chart-remove" @click=${() => this.remove(chart)}>Remove</button>
          </div>
          <div class="chart-area"><canvas data-chart=${chart.id}></canvas></div>
        </div>`)}`;
  }
}

customElements.define("charts-tab", ChartsTab);

export default { id: "charts", title: "Charts", element: "charts-tab" };
