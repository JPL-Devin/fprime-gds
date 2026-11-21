// The Sequences tab, after the sequencer of the Python GDS page: write or open a command sequence,
// uplink its binary (built with the compiler of `fgds seqgen`, sent through the uplink queue of the
// files plugin), then run it with the CS_RUN command of the dictionary.
import { LitElement, html } from "/ui/lit.js";

class SequencesTab extends LitElement {
  static properties = {
    output: { state: true }, error: { state: true }, active: { state: true }, uplinked: { state: true },
  };

  constructor() {
    super();
    this.output = ""; // what the compiler says, as the Python GDS page shows it
    this.error = ""; // what is wrong with the request itself, shown under the name
    this.active = false;
    this.uplinked = null; // path, on the flight side, of the last sequence uplinked
  }

  createRenderRoot() { return this; }

  field(id) { return this.querySelector("#" + id); }

  async uplink() {
    const name = this.field("sequence-name").value.trim();
    this.output = "";
    this.error = "";
    if (!/^[^;\\/]+\.seq$/.test(name)) {
      this.error = "Supply filename with .seq suffix";
      return;
    }
    this.active = true;
    try {
      // The messages name the sequence by the path of its staged copy, as the Python GDS does.
      const queue = await this.gds.api.get("/files/uplink").catch(() => ({}));
      const path = queue.directory ? `${queue.directory}/${name}` : name;
      const built = await this.gds.api.post("/sequences/compile", { name: path, text: this.field("sequence-text").value });
      const bytes = Uint8Array.from(built.hex.match(/../g) || [], (pair) => parseInt(pair, 16));
      const binary = name.replace(/\.seq$/, ".bin");
      const directory = this.field("sequence-directory").value.trim() || "/seq";
      const destination = directory.replace(/\/$/, "") + "/" + binary;
      const query = new URLSearchParams({ name: binary, destination });
      const response = await fetch("/api/v1/files/uplink?" + query, { method: "POST", body: bytes });
      if (!response.ok) {
        const body = await response.json().catch(() => ({}));
        throw new Error(body.error ? body.error.message : response.statusText);
      }
      this.output = `${built.output}Uplinking to ${destination}. Please confirm uplink EVRs before running.\n`;
      this.uplinked = destination;
    } catch (e) {
      this.output = e.message;
    }
    this.active = false;
  }

  async run() {
    if (!this.uplinked) return;
    const command = (this.gds.dictionary.commands || []).find((c) => c.name.endsWith(".CS_RUN") || c.name === "CS_RUN");
    this.output = "";
    this.error = "";
    if (!command) {
      this.error = "CS_RUN command not found in dictionary. Cannot run sequence.";
      return;
    }
    this.active = true;
    try {
      await this.gds.api.post("/commands", { name: command.name, args: [this.uplinked, "BLOCK"] });
      this.output = "Sent CS_RUN for " + this.uplinked;
    } catch (e) {
      this.error = e.message || "Failed to send CS_RUN command";
    }
    this.active = false;
  }

  saveAs() {
    const name = this.field("sequence-name").value.trim();
    if (!name) return;
    const link = document.createElement("a");
    link.href = URL.createObjectURL(new Blob([this.field("sequence-text").value], { type: "text/plain" }));
    link.download = name;
    link.click();
    URL.revokeObjectURL(link.href);
  }

  async open(event) {
    const file = event.target.files[0];
    if (!file) return;
    this.field("sequence-text").value = await file.text();
    this.field("sequence-name").value = file.name;
    event.target.value = "";
  }

  // Set through the style object: the content security policy of the page allows no style attribute.
  firstUpdated() {
    Object.assign(this.field("sequence-text").style, { width: "100%", fontFamily: "monospace" });
    Object.assign(this.field("sequence-result").style, { width: "100%", fontFamily: "monospace" });
  }

  render() {
    return html`
      <div class="toolbar">
        <input id="sequence-name" type="text" placeholder="Sequence name ending in .seq" ?disabled=${this.active}>
        <label><input id="sequence-directory" type="text" placeholder="/seq" value="/seq"
          title="Remote directory for sequence uplink" ?disabled=${this.active}> Uplink Directory</label>
        <button id="sequence-uplink" ?disabled=${this.active} @click=${() => this.uplink()}>Uplink</button>
        <button id="sequence-run" ?disabled=${this.active || !this.uplinked}
          title="Run the last uplinked sequence via cmdSeq.CS_RUN" @click=${() => this.run()}>Run</button>
        <button id="sequence-save" ?disabled=${this.active} @click=${() => this.saveAs()}>Save As</button>
        <button id="sequence-open" @click=${() => this.field("sequence-file").click()}>Open</button>
        <input id="sequence-file" type="file" accept=".seq" hidden @change=${(e) => this.open(e)}>
      </div>
      <div id="sequence-error" class="error-text">${this.error}</div>
      <textarea id="sequence-text" rows="20" spellcheck="false"></textarea>
      <textarea id="sequence-result" rows="5" readonly .value=${this.output}></textarea>
      <small>Sequence compilation output</small>`;
  }
}

customElements.define("fgds-sequences", SequencesTab);

export default { id: "sequences", title: "Sequences", element: "fgds-sequences" };
