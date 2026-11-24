// The Charts tab, after the chart display of the Python GDS page: one channel per chart, chosen
// from the numeric channels of the dictionary (array elements and structure members by path),
// plotted from the moment it is chosen on an axis that scrolls with the workstation clock. The view
// of a chart changes as on that page: ALT and the wheel, or ALT and a dragged selection, zoom;
// SHIFT and a drag pan; `Reset Zoom` gives the first view back; `Lock Scale` gives every chart the
// time range of the one that is changed.
import { LitElement, html } from "/ui/lit.js";
import "./chart.umd.js"; // Chart.js, vendored; it defines `globalThis.Chart`

const KIND = "fprime.channel";
const SHOWN = 60000; // milliseconds of the scrolling axis
const REFRESH = 100; // milliseconds between two steps of the scrolling axis
const STEP = 0.1; // part of the range one wheel notch adds or removes
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
  static properties = { charts: { state: true }, help: { state: true }, locked: { state: true }, unknown: { state: true } };

  constructor() {
    super();
    this.charts = [{ id: 0, selected: "", mode: "realtime", span: 3600, paused: false, collapsed: false }];
    this.next = 1;
    this.help = false;
    this.locked = false;
    this.unknown = [];
    this.views = new Map(); // chart id -> {x, y}: the ranges chosen by zoom and pan; none is the first view
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
    // The picture of a paused chart stays at the time of the pause.
    if ("paused" in values) changed.frozen = Date.now();
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
    this.views.delete(id);
  }

  // With `Lock Scale`, Pause stops and starts every chart, as on the Python GDS page.
  pause(chart) {
    const paused = !chart.paused;
    const frozen = Date.now();
    this.charts = this.charts.map((c) => (c === chart || (this.locked && c.selected) ? { ...c, paused, frozen } : c));
  }

  now(chart) {
    return chart.paused ? chart.frozen : Date.now();
  }

  // The ranges of a chart become these; a time range is kept as a distance to the present on a scrolling axis,
  // within the data window. With `Lock Scale` the other charts take the time range too.
  view(chart, { x, y }, shared = this.locked) {
    const view = this.views.get(chart.id) || {};
    if (y && y[0] < y[1]) view.y = { min: y[0], max: y[1] };
    if (x && x[0] < x[1]) {
      if (chart.mode === "anchored") {
        view.x = { min: x[0], max: x[1] };
      } else {
        const now = this.now(chart);
        const width = Math.min(x[1] - x[0], chart.span * 1000);
        const to = Math.min(0, Math.max(x[1] - now, width - chart.span * 1000));
        view.x = { from: to - width, to };
      }
    }
    this.views.set(chart.id, view);
    this.show(chart, undefined, true);
    if (x && shared) this.follow(chart);
  }

  // The other charts take the time range of this one.
  follow(leader) {
    const drawn = this.drawn.get(leader.id);
    if (!drawn) return;
    const { min, max } = drawn.scales.x;
    const chosen = Boolean((this.views.get(leader.id) || {}).x);
    for (const chart of this.charts) {
      if (chart === leader || !this.drawn.has(chart.id)) continue;
      if (chosen) {
        this.view(chart, { x: [min, max] }, false);
      } else if ((this.views.get(chart.id) || {}).x) {
        // The leader is on its first view (after `Reset Zoom`): so are the others, not on a copy of its range.
        delete this.views.get(chart.id).x;
        this.show(chart, undefined, true);
      }
    }
  }

  reset(chart) {
    for (const each of this.locked ? this.charts : [chart]) {
      this.views.delete(each.id);
      this.show(each, undefined, true);
    }
  }

  // ALT and the wheel: over the time axis or the value axis that axis alone, over the plot both, about the pointer.
  wheel(chart, event) {
    const drawn = this.drawn.get(chart.id);
    if (!event.altKey || !drawn) return;
    event.preventDefault();
    const { x, y } = drawn.scales;
    const area = drawn.chartArea;
    const factor = event.deltaY < 0 ? 1 - STEP : 1 + STEP;
    const about = (scale, pixel) => {
      const centre = scale.getValueForPixel(pixel);
      return [centre - (centre - scale.min) * factor, centre + (scale.max - centre) * factor];
    };
    const overTime = event.offsetY > area.bottom;
    const overValue = event.offsetX < area.left;
    this.view(chart, {
      x: overValue && !overTime ? null : about(x, Math.min(Math.max(event.offsetX, area.left), area.right)),
      y: overTime && !overValue ? null : about(y, Math.min(Math.max(event.offsetY, area.top), area.bottom)),
    });
  }

  // SHIFT and a drag move both ranges with the pointer; ALT and a drag select the part of the plot to show.
  press(chart, event) {
    const drawn = this.drawn.get(chart.id);
    if (!drawn || event.button !== 0 || !(event.shiftKey || event.altKey)) return;
    event.preventDefault();
    const canvas = drawn.canvas;
    canvas.setPointerCapture(event.pointerId);
    const { x, y } = drawn.scales;
    const start = { px: event.offsetX, py: event.offsetY, x: [x.min, x.max], y: [y.min, y.max], at: this.now(chart) };
    const panning = event.shiftKey;
    const area = drawn.chartArea;
    const box = panning ? null : document.createElement("div");
    if (box) {
      box.className = "chart-selection";
      Object.assign(box.style, { position: "absolute", background: "rgba(54, 162, 235, 0.25)", pointerEvents: "none" });
      canvas.parentElement.append(box);
    }
    const inside = (e) => [Math.min(Math.max(e.offsetX, area.left), area.right), Math.min(Math.max(e.offsetY, area.top), area.bottom)];
    const move = (e) => {
      if (panning) {
        const dx = (e.offsetX - start.px) * (start.x[1] - start.x[0]) / (area.right - area.left);
        const dy = (e.offsetY - start.py) * (start.y[1] - start.y[0]) / (area.bottom - area.top);
        // On a scrolling axis the range moved with the clock since the drag began.
        const moved = chart.mode === "anchored" ? 0 : this.now(chart) - start.at;
        this.view(chart, { x: [start.x[0] - dx + moved, start.x[1] - dx + moved], y: [start.y[0] + dy, start.y[1] + dy] });
      } else {
        const [px, py] = inside(e);
        const [sx, sy] = inside({ offsetX: start.px, offsetY: start.py });
        Object.assign(box.style, {
          left: `${Math.min(px, sx)}px`, top: `${Math.min(py, sy)}px`, width: `${Math.abs(px - sx)}px`, height: `${Math.abs(py - sy)}px`,
        });
      }
    };
    const release = (e) => {
      canvas.removeEventListener("pointermove", move);
      canvas.removeEventListener("pointerup", release);
      canvas.removeEventListener("pointercancel", release);
      if (!box) return;
      box.remove();
      const [px, py] = inside(e);
      const [sx, sy] = inside({ offsetX: start.px, offsetY: start.py });
      if (e.type !== "pointerup" || Math.abs(px - sx) < 5 || Math.abs(py - sy) < 5) return;
      const moved = chart.mode === "anchored" ? 0 : this.now(chart) - start.at;
      const scales = { x: drawn.scales.x, y: drawn.scales.y };
      const ends = (scale, a, b) => [scale.getValueForPixel(a), scale.getValueForPixel(b)].sort((m, n) => m - n);
      // The time axis is read as it was when the drag began, moved by the time since.
      const width = start.x[1] - start.x[0];
      const time = (pixel) => start.x[0] + moved + (pixel - area.left) * width / (area.right - area.left);
      this.view(chart, { x: [time(Math.min(px, sx)), time(Math.max(px, sx))], y: ends(scales.y, py, sy) });
    };
    canvas.addEventListener("pointermove", move);
    canvas.addEventListener("pointerup", release);
    canvas.addEventListener("pointercancel", release);
  }

  add(selected = "") {
    this.charts = [...this.charts, { id: this.next++, selected, mode: "realtime", span: 3600, paused: false, collapsed: false }];
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
    const names = (await file.text()).split(/\s+/).filter(Boolean);
    event.target.value = "";
    const last = this.charts[this.charts.length - 1];
    if (names.length && last && !last.selected) this.remove(last);
    // A name the dictionary does not have gets no chart: it is named in a line under the buttons.
    this.unknown = names.filter((name) => !this.names.includes(name));
    for (const name of names) if (this.names.includes(name)) this.add(name);
  }

  updated() {
    for (const chart of this.charts) {
      if (!chart.selected || this.drawn.has(chart.id)) continue;
      const canvas = this.querySelector(`canvas[data-chart="${chart.id}"]`);
      // Set through the style object: the content security policy of the page allows no style attribute.
      Object.assign(canvas.parentElement.style, { height: "16rem", position: "relative", resize: "both", overflow: "hidden" });
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
      canvas.addEventListener("wheel", (event) => this.wheel(this.charts.find((c) => c.id === chart.id), event), { passive: false });
      canvas.addEventListener("pointerdown", (event) => this.press(this.charts.find((c) => c.id === chart.id), event));
      canvas.dataset.points = "0";
      this.show(chart);
    }
  }

  // Samples older than the data window are dropped; the scrolling axis ends at the present, or where zoom and
  // pan have put it. A paused chart is drawn again only when its view is changed.
  show(chart, now = Date.now(), changed = false) {
    chart = this.charts.find((c) => c.id === chart.id) || chart;
    const drawn = this.drawn.get(chart.id);
    if (!drawn || (chart.paused && !changed)) return;
    if (chart.paused) now = chart.frozen;
    const data = this.data.get(chart.id);
    const scrolling = chart.mode !== "anchored";
    if (scrolling && !chart.paused) {
      const kept = data.findIndex((point) => point.x >= now - chart.span * 1000);
      data.splice(0, kept < 0 ? data.length : kept);
    }
    const view = this.views.get(chart.id) || {};
    const { x, y } = drawn.options.scales;
    if (scrolling) {
      x.min = now + (view.x && "from" in view.x ? view.x.from : -SHOWN);
      x.max = now + (view.x && "to" in view.x ? view.x.to : 0);
    } else {
      x.min = view.x && "min" in view.x ? view.x.min : undefined;
      x.max = view.x && "max" in view.x ? view.x.max : undefined;
    }
    y.min = view.y ? view.y.min : undefined;
    y.max = view.y ? view.y.max : undefined;
    drawn.update("none");
    const canvas = drawn.canvas;
    canvas.dataset.points = String(data.length);
    canvas.dataset.first = data.length ? String(data[0].y) : "";
    canvas.dataset.from = String(drawn.scales.x.min);
    canvas.dataset.to = String(drawn.scales.x.max);
    canvas.dataset.low = String(drawn.scales.y.min);
    canvas.dataset.high = String(drawn.scales.y.max);
    canvas.dataset.zoomed = String(Boolean(view.x || view.y));
  }

  scroll() {
    const now = Date.now();
    for (const chart of this.charts) if (chart.mode !== "anchored") this.show(chart, now);
    // With `Lock Scale` every chart keeps the time range of the first one.
    const first = this.charts.find((chart) => this.drawn.has(chart.id));
    if (this.locked && first) this.follow(first);
  }

  render() {
    // `selected` on the option, not `value` on the list: the value of a list drawn for the first time is set
    // before its options exist, and is lost.
    const options = (selected) => this.names.map((name) => html`<option value=${name} ?selected=${name === selected}>${name}</option>`);
    return html`
      <h2>Charts</h2>
      <div class="toolbar">
        <button id="chart-add" @click=${() => this.add()}>Add Chart</button>
        <button id="chart-lock" class=${this.locked ? "active" : ""} aria-pressed=${this.locked ? "true" : "false"}
          @click=${() => { this.locked = !this.locked; }}>Lock Scale</button>
        <button id="chart-save" @click=${() => this.save()}>Save</button>
        <button id="chart-open" @click=${() => this.querySelector("#chart-load").click()}>Load</button>
        <input id="chart-load" type="file" hidden @change=${(e) => this.load(e)}>
        <button id="chart-help" @click=${() => { this.help = !this.help; }}>Help</button>
      </div>
      ${this.unknown.length ? html`<p id="chart-unknown" class="note" role="alert">Not loaded, no such channel in the dictionary: ${this.unknown.join(", ")}</p>` : ""}
      ${this.help ? html`<div id="chart-help-text" class="note">
        <p><b>Zoom in and out</b> by holding <b>ALT</b> and using mouse wheel to scroll while hovering over an axis</p>
        <p><b>Zoom in</b> by holding <b>ALT</b> and clicking and dragging a selection on the chart</p>
        <p><b>Pan</b> by holding <b>SHIFT</b> and clicking and dragging the chart</p>
        <p><b>Change size</b> by clicking and dragging the icon at the bottom right of the chart box</p>
        <p><b>Time Modes:</b></p>
        <p><b>Realtime</b> - Chart scrolls and displays data using channel timestamps (scrolling based off of workstation time)</p>
        <p><b>Realtime (anchored)</b> - No scrolling, chart redraws and centers around all data points using channel timestamp</p>
        <p><b>Earth Received Time</b> - Chart scrolls and displays data using ground station receive time</p>
      </div>` : ""}
      ${this.charts.map((chart) => html`
        <div class="chart" data-chart-box=${chart.id}>
          <div class="toolbar chart-header">
            <span class="chart-title">${chart.selected}</span>
            <button class="chart-collapse" aria-expanded=${chart.collapsed ? "false" : "true"}
              @click=${() => this.change(chart, { collapsed: !chart.collapsed })}>${chart.collapsed ? "Expand" : "Collapse"}</button>
            <button class="chart-remove" @click=${() => this.remove(chart)}>Remove</button>
          </div>
          <div class="chart-body" ?hidden=${chart.collapsed}>
          <div class="toolbar">
            <select class="chart-channel" .value=${chart.selected}
              @change=${(e) => this.change(chart, { selected: e.target.value })}>
              <option value="" ?selected=${!chart.selected}>Select a Channel</option>${options(chart.selected)}
            </select>
            <label for="chart-span-${chart.id}">Data Window:</label>
            <input class="chart-span" id="chart-span-${chart.id}" type="number" min="1" .value=${String(chart.span)}
              @change=${(e) => this.change(chart, { span: Number(e.target.value) || 3600 })}>
            <label for="chart-span-${chart.id}">(S)</label>
            <label>Time: <select class="chart-mode" .value=${chart.mode}
              @change=${(e) => this.change(chart, { mode: e.target.value })}>
              ${MODES.map(([mode, title]) => html`<option value=${mode}>${title}</option>`)}
            </select></label>
            <button class="chart-pause" ?disabled=${!chart.selected}
              @click=${() => this.pause(chart)}>${chart.paused ? "Resume" : "Pause"}</button>
            <button class="chart-reset" ?disabled=${!chart.selected} @click=${() => this.reset(chart)}>Reset Zoom</button>
          </div>
          ${chart.selected ? html`<div class="chart-area"><canvas data-chart=${chart.id}></canvas></div>` : ""}
          </div>
        </div>`)}`;
  }
}

customElements.define("charts-tab", ChartsTab);

export default { id: "charts", title: "Charts", element: "charts-tab" };
