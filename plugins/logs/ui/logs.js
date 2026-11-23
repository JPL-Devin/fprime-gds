// The Logs tab: the text logs the logs plugin serves, one per record kind and one for
// the GDS itself, read from `/api/v1/logs` and followed while the tab is open.
import { LitElement, html } from "/ui/lit.js";

const KEEP = 20000; // lines kept in the page; the download holds a page from the server
const PERIOD = 1000;

class LogsTab extends LitElement {
  static properties = {
    logs: { state: true }, selected: { state: true }, text: { state: true }, following: { state: true },
    version: { state: true }, problem: { state: true },
  };

  constructor() {
    super();
    this.logs = [];
    this.selected = "";
    this.text = "";
    this.following = true;
    this.lines = [];
    this.after = "0";
    this.version = 0;
    this.problem = "";
  }

  createRenderRoot() { return this; }

  async connectedCallback() {
    super.connectedCallback();
    try {
      this.logs = await this.gds.api.get("/logs");
    } catch (error) {
      this.problem = error.message;
      return;
    }
    this.choose(this.logs.length ? this.logs[0].name : "");
    this.timer = setInterval(() => { if (this.following) this.read(); }, PERIOD);
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    clearInterval(this.timer);
  }

  choose(name) {
    this.selected = name;
    this.lines = [];
    this.after = "0";
    this.generation = (this.generation || 0) + 1;
    delete this.dataset.loaded;
    this.read();
  }

  // Lines after the last one read: each line once, in order.
  async read() {
    if (this.reading || !this.selected) return;
    this.reading = true;
    const generation = this.generation;
    try {
      for (;;) {
        const page = await this.gds.api.get(`/logs/${encodeURIComponent(this.selected)}?after_seq=${this.after}&limit=5000`);
        if (generation !== this.generation) break;
        this.lines.push(...page.lines);
        if (this.lines.length > KEEP) this.lines.splice(0, this.lines.length - KEEP);
        if (page.lines.length) this.after = page.lines[page.lines.length - 1].seq;
        if (page.next === null) break;
        this.after = page.next;
      }
      this.problem = "";
      if (generation === this.generation) this.dataset.loaded = this.selected;
    } catch (error) {
      this.problem = error.message;
    } finally {
      this.reading = false;
      this.version += 1;
    }
  }

  updated() {
    const view = this.querySelector("pre.log");
    if (view && this.following) view.scrollIntoView({ block: "end" });
  }

  render() {
    const needle = this.text.toLowerCase();
    const lines = needle ? this.lines.filter((l) => l.text.toLowerCase().includes(needle)) : this.lines;
    const log = this.logs.find((l) => l.name === this.selected);
    return html`
      <div class="toolbar">
        <label>Available Logs
          <select id="logs-select" @change=${(e) => this.choose(e.target.value)}>
            ${this.logs.map((l) => html`<option value=${l.name} ?selected=${l.name === this.selected}>${l.name}</option>`)}
          </select>
        </label>
        <label>Filter <input type="search" id="logs-filter" .value=${this.text}
          @input=${(e) => { this.text = e.target.value; }}></label>
        <label><input type="checkbox" id="logs-follow" .checked=${this.following}
          @change=${(e) => { this.following = e.target.checked; }}>Scroll Log Output</label>
        <a id="logs-download" href=${`/api/v1/logs/${encodeURIComponent(this.selected)}?format=text&limit=10000`}
          download=${`${this.selected}.log`}>Download</a>
        <span class="muted" id="logs-count">${lines.length} of ${this.lines.length} lines</span>
        <span class="error-text">${this.problem}</span>
      </div>
      <div class="muted">${log ? log.description : ""}</div>
      <pre class="log mono" id="logs-text">${lines.length ? lines.map((l) => l.text).join("\n") : "(no lines)"}</pre>`;
  }
}
customElements.define("logs-tab", LogsTab);

export default { id: "logs", title: "Logs", element: "logs-tab" };
